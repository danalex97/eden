/*
 *  Copyright (c) 2004-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "eden/fs/inodes/DeferredDiffEntry.h"

#include <folly/Optional.h>
#include <folly/Unit.h>
#include <folly/futures/Future.h>
#include "eden/fs/inodes/DiffContext.h"
#include "eden/fs/inodes/EdenMount.h"
#include "eden/fs/inodes/FileInode.h"
#include "eden/fs/inodes/InodeDiffCallback.h"
#include "eden/fs/inodes/TreeInode.h"
#include "eden/fs/store/BlobMetadata.h"
#include "eden/fs/store/ObjectStore.h"
#include "eden/fs/utils/Bug.h"

using folly::makeFuture;
using folly::Future;
using folly::Unit;
using std::make_unique;
using std::unique_ptr;
using std::vector;

namespace facebook {
namespace eden {

namespace {

class UntrackedDiffEntry : public DeferredDiffEntry {
 public:
  UntrackedDiffEntry(
      const DiffContext* context,
      RelativePath path,
      InodePtr inode,
      GitIgnoreStack* ignore,
      bool isIgnored)
      : DeferredDiffEntry{context, std::move(path)},
        ignore_{ignore},
        isIgnored_{isIgnored},
        inode_{std::move(inode)} {}

  /*
   * This is a template just to avoid ambiguity with the prior constructor,
   * since folly::Future<X> can unfortunately be implicitly constructed from X.
   */
  template <
      typename InodeFuture,
      typename X = typename std::enable_if<
          std::is_same<folly::Future<InodePtr>, InodeFuture>::value,
          void>>
  UntrackedDiffEntry(
      const DiffContext* context,
      RelativePath path,
      InodeFuture&& inodeFuture,
      GitIgnoreStack* ignore,
      bool isIgnored)
      : DeferredDiffEntry{context, std::move(path)},
        ignore_{ignore},
        isIgnored_{isIgnored},
        inodeFuture_{std::forward<InodeFuture>(inodeFuture)} {}

  folly::Future<folly::Unit> run() override {
    // If we have an inodeFuture_ to wait on, wait for it to finish,
    // then store the resulting inode_ and invoke run() again.
    if (inodeFuture_.hasValue()) {
      CHECK(!inode_) << "cannot have both inode_ and inodeFuture_ set";
      return inodeFuture_->then([this](InodePtr inode) {
        inode_ = std::move(inode);
        inodeFuture_.clear();
        return run();
      });
    }

    auto treeInode = inode_.asTreePtrOrNull();
    if (!treeInode.get()) {
      auto bug = EDEN_BUG()
          << "UntrackedDiffEntry should only used with tree inodes";
      return makeFuture<Unit>(bug.toException());
    }

    // Recursively diff the untracked directory.
    return treeInode->diff(context_, getPath(), nullptr, ignore_, isIgnored_);
  }

 private:
  GitIgnoreStack* ignore_{nullptr};
  bool isIgnored_{false};
  InodePtr inode_;
  folly::Optional<folly::Future<InodePtr>> inodeFuture_;
};

/*
 * Helper functions for diffing removed directories.
 *
 * This is used by both RemovedDiffEntry and ModifiedEntryInfo.
 * (ModifiedBlobDiffEntry needs it for handling cases where a directory was
 * replaced with a file.)
 */
namespace {
// Overload that takes an already loaded Tree
folly::Future<folly::Unit> diffRemovedTree(
    const DiffContext* context,
    RelativePath currentPath,
    const Tree* tree);
// Overload that takes a TreeEntry, and has to load the Tree in question first
folly::Future<folly::Unit> diffRemovedTree(
    const DiffContext* context,
    RelativePath currentPath,
    const TreeEntry& entry);

folly::Future<folly::Unit> diffRemovedTree(
    const DiffContext* context,
    RelativePath currentPath,
    const TreeEntry& entry) {
  DCHECK_EQ(TreeEntryType::TREE, entry.getType());
  return context->store->getTreeFuture(entry.getHash()).then([
    context,
    currentPath = RelativePath{std::move(currentPath)}
  ](unique_ptr<Tree> && tree) {
    return diffRemovedTree(context, std::move(currentPath), tree.get());
  });
}

folly::Future<folly::Unit> diffRemovedTree(
    const DiffContext* context,
    RelativePath currentPath,
    const Tree* tree) {
  vector<Future<Unit>> subFutures;
  for (const auto& entry : tree->getTreeEntries()) {
    if (entry.getType() == TreeEntryType::TREE) {
      auto f = diffRemovedTree(context, currentPath + entry.getName(), entry);
      subFutures.emplace_back(std::move(f));
    } else {
      context->callback->removedFile(currentPath + entry.getName(), entry);
    }
  }

  return folly::collectAll(subFutures).then([
    currentPath = RelativePath{std::move(currentPath)},
    tree = std::move(tree),
    context
  ](vector<folly::Try<Unit>> results) {
    // Call diffError() for each error that occurred
    for (size_t n = 0; n < results.size(); ++n) {
      auto& result = results[n];
      if (result.hasException()) {
        const auto& entry = tree->getEntryAt(n);
        context->callback->diffError(
            currentPath + entry.getName(), result.exception());
      }
    }
    // Return successfully after recording the errors.  (If we failed then
    // our caller would also record us as an error, which we don't want.)
    return makeFuture();
  });
}
} // unnamed namespace

class RemovedDiffEntry : public DeferredDiffEntry {
 public:
  RemovedDiffEntry(
      const DiffContext* context,
      RelativePath path,
      const TreeEntry& scmEntry)
      : DeferredDiffEntry{context, std::move(path)}, scmEntry_{scmEntry} {
    // We only need to defer processing for removed directories;
    // we never create RemovedDiffEntry objects for removed files.
    DCHECK_EQ(TreeEntryType::TREE, scmEntry_.getType());
  }

  folly::Future<folly::Unit> run() override {
    return diffRemovedTree(context_, getPath(), scmEntry_);
  }

 private:
  TreeEntry scmEntry_;
};

class ModifiedDiffEntry : public DeferredDiffEntry {
 public:
  ModifiedDiffEntry(
      const DiffContext* context,
      RelativePath path,
      const TreeEntry& scmEntry,
      InodePtr inode,
      GitIgnoreStack* ignore,
      bool isIgnored)
      : DeferredDiffEntry{context, std::move(path)},
        ignore_{ignore},
        isIgnored_{isIgnored},
        scmEntry_{scmEntry},
        inode_{std::move(inode)} {}

  ModifiedDiffEntry(
      const DiffContext* context,
      RelativePath path,
      const TreeEntry& scmEntry,
      folly::Future<InodePtr>&& inodeFuture,
      GitIgnoreStack* ignore,
      bool isIgnored)
      : DeferredDiffEntry{context, std::move(path)},
        ignore_{ignore},
        isIgnored_{isIgnored},
        scmEntry_{scmEntry},
        inodeFuture_{std::move(inodeFuture)} {}

  folly::Future<folly::Unit> run() override {
    // If we have an inodeFuture_, wait on it to complete.
    // TODO: Load the inode in parallel with loading the source control data
    // below.
    if (inodeFuture_.hasValue()) {
      CHECK(!inode_) << "cannot have both inode_ and inodeFuture_ set";
      return inodeFuture_->then([this](InodePtr inode) {
        inode_ = std::move(inode);
        inodeFuture_.clear();
        return run();
      });
    }

    if (scmEntry_.getType() == TreeEntryType::TREE) {
      return runForScmTree();
    } else {
      return runForScmBlob();
    }
  }

 private:
  folly::Future<folly::Unit> runForScmTree() {
    auto treeInode = inode_.asTreePtrOrNull();
    if (!treeInode) {
      // This is a Tree in the source control state, but a file or symlink
      // in the current filesystem state.
      // Report this file as untracked, and everything in the source control
      // tree as removed.
      if (isIgnored_) {
        if (context_->listIgnored) {
          context_->callback->ignoredFile(getPath());
        }
      } else {
        context_->callback->untrackedFile(getPath());
      }
      return diffRemovedTree(context_, getPath(), scmEntry_);
    }

    // Possibly modified directory.  Load the Tree in question.
    return context_->store->getTreeFuture(scmEntry_.getHash()).then([
      this,
      treeInode = std::move(treeInode)
    ](unique_ptr<Tree> && tree) {
      return treeInode->diff(
          context_, getPath(), std::move(tree), ignore_, isIgnored_);
    });
  }

  folly::Future<folly::Unit> runForScmBlob() {
    auto fileInode = inode_.asFilePtrOrNull();
    if (!fileInode) {
      // This is a file in the source control state, but a directory
      // in the current filesystem state.
      // Report this file as removed, and everything in the source control
      // tree as untracked/ignored.
      context_->callback->removedFile(getPath(), scmEntry_);
      auto treeInode = inode_.asTreePtr();
      if (isIgnored_ && !context_->listIgnored) {
        return makeFuture();
      }
      return treeInode->diff(context_, getPath(), nullptr, ignore_, isIgnored_);
    }

    // Possibly modified file.  First check the mode.
    // If it is different the file is definitely modified.
    if (fileInode->getMode() != scmEntry_.getMode()) {
      context_->callback->modifiedFile(getPath(), scmEntry_);
      return makeFuture();
    }

    // TODO: If at some point we add file size info to the TreeEntry, we could
    // first check to see if the file size is different, before having to load
    // the SHA1 data.

    // Load the blob SHA1 and the file contents SHA1, so we can check if the
    // contents are modified.  Note that we want the FileInode to always
    // return a SHA1 to us, even for symlink contents.
    auto blobSha1Future = context_->store->getBlobMetadata(scmEntry_.getHash());
    auto fileSha1Future = fileInode->getSHA1(false);
    return folly::collect(blobSha1Future, fileSha1Future)
        .then([this](const std::tuple<BlobMetadata, Hash>& result) {
          if (std::get<0>(result).sha1 != std::get<1>(result)) {
            context_->callback->modifiedFile(getPath(), scmEntry_);
          }
        });
  }

  GitIgnoreStack* ignore_{nullptr};
  bool isIgnored_{false};
  TreeEntry scmEntry_;
  folly::Optional<folly::Future<InodePtr>> inodeFuture_;
  InodePtr inode_;
  unique_ptr<Tree> scmTree_;
};

class ModifiedBlobDiffEntry : public DeferredDiffEntry {
 public:
  ModifiedBlobDiffEntry(
      const DiffContext* context,
      RelativePath path,
      const TreeEntry& scmEntry,
      Hash currentBlobHash)
      : DeferredDiffEntry{context, std::move(path)},
        scmEntry_{scmEntry},
        currentBlobHash_{currentBlobHash} {}

  folly::Future<folly::Unit> run() override {
    auto f1 = context_->store->getBlobMetadata(scmEntry_.getHash());
    auto f2 = context_->store->getBlobMetadata(currentBlobHash_);
    return folly::collect(f1, f2).then(
        [this](const std::tuple<BlobMetadata, BlobMetadata>& info) {
          if (std::get<0>(info).sha1 != std::get<1>(info).sha1) {
            context_->callback->modifiedFile(getPath(), scmEntry_);
          }
        });
  }

 private:
  TreeEntry scmEntry_;
  Hash currentBlobHash_;
};

} // unnamed namespace

unique_ptr<DeferredDiffEntry> DeferredDiffEntry::createUntrackedEntry(
    const DiffContext* context,
    RelativePath path,
    InodePtr inode,
    GitIgnoreStack* ignore,
    bool isIgnored) {
  return make_unique<UntrackedDiffEntry>(
      context, std::move(path), std::move(inode), ignore, isIgnored);
}

unique_ptr<DeferredDiffEntry>
DeferredDiffEntry::createUntrackedEntryFromInodeFuture(
    const DiffContext* context,
    RelativePath path,
    Future<InodePtr>&& inodeFuture,
    GitIgnoreStack* ignore,
    bool isIgnored) {
  return make_unique<UntrackedDiffEntry>(
      context, std::move(path), std::move(inodeFuture), ignore, isIgnored);
}

unique_ptr<DeferredDiffEntry> DeferredDiffEntry::createRemovedEntry(
    const DiffContext* context,
    RelativePath path,
    const TreeEntry& scmEntry) {
  return make_unique<RemovedDiffEntry>(context, std::move(path), scmEntry);
}

unique_ptr<DeferredDiffEntry> DeferredDiffEntry::createModifiedEntry(
    const DiffContext* context,
    RelativePath path,
    const TreeEntry& scmEntry,
    InodePtr inode,
    GitIgnoreStack* ignore,
    bool isIgnored) {
  return make_unique<ModifiedDiffEntry>(
      context, std::move(path), scmEntry, std::move(inode), ignore, isIgnored);
}

unique_ptr<DeferredDiffEntry>
DeferredDiffEntry::createModifiedEntryFromInodeFuture(
    const DiffContext* context,
    RelativePath path,
    const TreeEntry& scmEntry,
    folly::Future<InodePtr>&& inodeFuture,
    GitIgnoreStack* ignore,
    bool isIgnored) {
  return make_unique<ModifiedDiffEntry>(
      context,
      std::move(path),
      scmEntry,
      std::move(inodeFuture),
      ignore,
      isIgnored);
}

unique_ptr<DeferredDiffEntry> DeferredDiffEntry::createModifiedEntry(
    const DiffContext* context,
    RelativePath path,
    const TreeEntry& scmEntry,
    Hash currentBlobHash) {
  return make_unique<ModifiedBlobDiffEntry>(
      context, std::move(path), scmEntry, currentBlobHash);
}
}
}
