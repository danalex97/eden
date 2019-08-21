# Initial setup
  $ setconfig extensions.lfs=
  $ setconfig extensions.rebase=
  $ setconfig extensions.snapshot=
  $ setconfig extensions.treemanifest=!
  $ setconfig visibility.enabled=true

# Prepare server and client repos.
  $ hg init server
  $ hg clone -q server client
  $ cd client
  $ hg debugvisibility start

# Add a file to the store
  $ echo "foo" > foofile
  $ mkdir bar
  $ echo "bar" > bar/file
  $ hg add foofile bar/file
  $ hg commit -m "add some files"
  $ hg push
  pushing to $TESTTMP/server
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 1 changesets with 2 changes to 2 files

# Call this state a base revision
  $ BASEREV="$(hg id -i)"
  $ echo "$BASEREV"
  3490593cf53c

# Snapshot test plan:
# 1) Empty snapshot (no changes);
# 2) Snapshot with an empty manifest (changes only in tracked files);
# 3) Snapshot with a manifest (merge state + mixed changes);
# 4) Same as 3 but test the --clean flag on creation;
# 5) Same as 3 but test the --force flag on restore;
# 6) TODO(alexeyqu): Same as 3 but sync to the server and another client.


# 1) Empty snapshot -- no need to create a snapshot now
  $ hg debugsnapshot
  nothing changed


# 2) Snapshot with an empty manifest (changes only in tracked files)
  $ hg rm bar/file
  $ echo "change" >> foofile
  $ echo "another" > bazfile
  $ hg add bazfile
  $ BEFORESTATUS="$(hg status --verbose)"
  $ echo "$BEFORESTATUS"
  M foofile
  A bazfile
  R bar/file
  $ BEFOREDIFF="$(hg diff)"
  $ echo "$BEFOREDIFF"
  diff -r 3490593cf53c bar/file
  --- a/bar/file	Thu Jan 01 00:00:00 1970 +0000
  +++ /dev/null	Thu Jan 01 00:00:00 1970 +0000
  @@ -1,1 +0,0 @@
  -bar
  diff -r 3490593cf53c bazfile
  --- /dev/null	Thu Jan 01 00:00:00 1970 +0000
  +++ b/bazfile	Thu Jan 01 00:00:00 1970 +0000
  @@ -0,0 +1,1 @@
  +another
  diff -r 3490593cf53c foofile
  --- a/foofile	Thu Jan 01 00:00:00 1970 +0000
  +++ b/foofile	Thu Jan 01 00:00:00 1970 +0000
  @@ -1,1 +1,2 @@
   foo
  +change
# Create a snapshot and check the result
  $ EMPTYOID="$(hg debugsnapshot | head -n 1 | cut -f2 -d' ')"
  $ echo "$EMPTYOID"
  509c24c4681a119720ced4260520dce99e1370ee
  $ hg log --hidden -r "$EMPTYOID" -T '{extras % \"{extra}\n\"}' | grep snapshotmanifestid
  snapshotmanifestid=
# The snapshot commit is hidden
  $ hg log --hidden -r  "not hidden() & $EMPTYOID"
# But it exists!
  $ hg show --hidden "$EMPTYOID"
  changeset:   1:509c24c4681a
  tag:         tip
  user:        test
  date:        Thu Jan 01 00:00:00 1970 +0000
  files:       bar/file bazfile foofile
  description:
  snapshot
  
  
  diff -r 3490593cf53c -r 509c24c4681a bar/file
  --- a/bar/file	Thu Jan 01 00:00:00 1970 +0000
  +++ /dev/null	Thu Jan 01 00:00:00 1970 +0000
  @@ -1,1 +0,0 @@
  -bar
  diff -r 3490593cf53c -r 509c24c4681a bazfile
  --- /dev/null	Thu Jan 01 00:00:00 1970 +0000
  +++ b/bazfile	Thu Jan 01 00:00:00 1970 +0000
  @@ -0,0 +1,1 @@
  +another
  diff -r 3490593cf53c -r 509c24c4681a foofile
  --- a/foofile	Thu Jan 01 00:00:00 1970 +0000
  +++ b/foofile	Thu Jan 01 00:00:00 1970 +0000
  @@ -1,1 +1,2 @@
   foo
  +change
  
# Rollback to BASEREV and checkout on EMPTYOID
  $ hg update -q --clean "$BASEREV" && rm bazfile
  $ hg status --verbose
  $ hg debugcheckoutsnapshot "$EMPTYOID"
  will checkout on 509c24c4681a119720ced4260520dce99e1370ee
  checkout complete
  $ test "$BEFORESTATUS" = "$(hg status --verbose)"
  $ test "$BEFOREDIFF" = "$(hg diff)"


# 3) Snapshot with a manifest + merge conflict!
  $ hg update -q --clean "$BASEREV" && rm bazfile
  $ hg status --verbose
  $ echo "a" > mergefile
  $ hg add mergefile
  $ hg commit -m "merge #1"
  $ MERGEREV="$(hg id -i)"
  $ hg checkout "$BASEREV"
  0 files updated, 0 files merged, 1 files removed, 0 files unresolved
  $ echo "b" > mergefile
  $ hg add mergefile
  $ hg commit -m "merge #2"
  $ hg merge "$MERGEREV"
  merging mergefile
  warning: 1 conflicts while merging mergefile! (edit, then use 'hg resolve --mark')
  0 files updated, 0 files merged, 0 files removed, 1 files unresolved
  use 'hg resolve' to retry unresolved file merges or 'hg update -C .' to abandon
  [1]

# Make some changes on top of that: add, remove, edit
  $ hg rm bar/file
  $ rm foofile
  $ echo "another" > bazfile
  $ hg add bazfile
  $ echo "fizz" > untrackedfile
  $ BEFORESTATUS="$(hg status --verbose)"
  $ echo "$BEFORESTATUS"
  M mergefile
  A bazfile
  R bar/file
  ! foofile
  ? mergefile.orig
  ? untrackedfile
  # The repository is in an unfinished *merge* state.
  
  # Unresolved merge conflicts:
  # 
  #     mergefile
  # 
  # To mark files as resolved:  hg resolve --mark FILE
  
  # To continue:                hg commit
  # To abort:                   hg update --clean .    (warning: this will discard uncommitted changes)
  $ BEFOREDIFF="$(hg diff)"
  $ echo "$BEFOREDIFF"
  diff -r 6eb2552aed20 bar/file
  --- a/bar/file	Thu Jan 01 00:00:00 1970 +0000
  +++ /dev/null	Thu Jan 01 00:00:00 1970 +0000
  @@ -1,1 +0,0 @@
  -bar
  diff -r 6eb2552aed20 bazfile
  --- /dev/null	Thu Jan 01 00:00:00 1970 +0000
  +++ b/bazfile	Thu Jan 01 00:00:00 1970 +0000
  @@ -0,0 +1,1 @@
  +another
  diff -r 6eb2552aed20 mergefile
  --- a/mergefile	Thu Jan 01 00:00:00 1970 +0000
  +++ b/mergefile	Thu Jan 01 00:00:00 1970 +0000
  @@ -1,1 +1,5 @@
  +<<<<<<< working copy: 6eb2552aed20 - test: merge #2
   b
  +=======
  +a
  +>>>>>>> merge rev:    f473d4d5a1c0 - test: merge #1

# Create the snapshot
  $ OID="$(hg debugsnapshot | cut -f2 -d' ')"
  $ echo "$OID"
  f788a565a3b78ea7b351a3c8d0bd8622ebeaf52e

# hg status/diff are unchanged
  $ test "$BEFORESTATUS" = "$(hg status --verbose)"
  $ test "$BEFOREDIFF" = "$(hg diff)"

# Check the manifest id and its contents
  $ MANIFESTOID="$(hg log --hidden -r \"$OID\" -T '{extras % \"{extra}\n\"}' | grep snapshotmanifestid | cut -d'=' -f2)"
  $ echo "$MANIFESTOID"
  94d0daf8fadba1a239eca4ddb5cc1a71728097928543dfb02b8be399ae6fcb56
  $ cat .hg/store/lfs/objects/"${MANIFESTOID:0:2}"/"${MANIFESTOID:2}"
  {"deleted": {"foofile": null}, "localvfsfiles": {"merge/fc4ffdcb8ed23cecd44a0e11d23af83b445179b4": {"oid": "0263829989b6fd954f72baaf2fc64bc2e2f01d692d4de72986ea808f6e99813f", "size": "2"}, "merge/state": {"oid": "fdfea51dfeeae94bd846473c7bef891823af465d33f48e92ed2556bde6b346cb", "size": "166"}, "merge/state2": {"oid": "0e421047ebcf7d0cada48ddd801304725de33da3c4048ccb258041946cd0e81d", "size": "361"}}, "unknown": {"mergefile.orig": {"oid": "0263829989b6fd954f72baaf2fc64bc2e2f01d692d4de72986ea808f6e99813f", "size": "2"}, "untrackedfile": {"oid": "b05b74c474c1706953bed876a19f146b371ddf51a36474fe0c094922385cc479", "size": "5"}}} (no-eol)

# Move back to BASEREV
  $ hg update -q --clean "$BASEREV" && rm bazfile
  $ rm mergefile.orig
  $ hg status
  ? untrackedfile

# Check out on the snapshot -- negative tests
# Non-empty WC state
  $ hg debugcheckoutsnapshot "$OID"
  abort: You must have a clean working copy to checkout on a snapshot. Use --force to bypass that.
  
  [255]
# Bad id
  $ rm untrackedfile
  $ hg debugcheckoutsnapshot somebadid
  somebadid is not a valid revision id
  abort: unknown revision 'somebadid'!
  (if somebadid is a remote bookmark or commit, try to 'hg pull' it first)
  [255]
# Still bad id -- not a snapshot
  $ hg debugcheckoutsnapshot "$BASEREV"
  abort: 3490593cf53c is not a valid snapshot id
  
  [255]
# Check out on the snapshot -- positive tests
  $ hg debugcheckoutsnapshot "$OID"
  will checkout on f788a565a3b78ea7b351a3c8d0bd8622ebeaf52e
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved
  checkout complete
  $ test "$BEFORESTATUS" = "$(hg status --verbose)"
  $ test "$BEFOREDIFF" = "$(hg diff)"


# 4) Test the --clean flag
  $ find .hg/merge | sort
  .hg/merge
  .hg/merge/fc4ffdcb8ed23cecd44a0e11d23af83b445179b4
  .hg/merge/state
  .hg/merge/state2
  $ hg debugsnapshot --clean
  snapshot f788a565a3b78ea7b351a3c8d0bd8622ebeaf52e created
  3 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ hg status --verbose
  $ test -d .hg/merge
  [1]
  $ hg debugcheckoutsnapshot "$OID"
  will checkout on f788a565a3b78ea7b351a3c8d0bd8622ebeaf52e
  checkout complete


# 5) Test the --force flag
  $ hg update -q --clean "$BASEREV"
  $ hg status --verbose
  ? bazfile
  ? mergefile.orig
  ? untrackedfile
  $ hg debugcheckoutsnapshot "$OID"
  abort: You must have a clean working copy to checkout on a snapshot. Use --force to bypass that.
  
  [255]
  $ hg debugcheckoutsnapshot --force "$OID"
  will checkout on f788a565a3b78ea7b351a3c8d0bd8622ebeaf52e
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved
  checkout complete

# Finally, resolve the conflict
  $ hg resolve --mark mergefile
  (no more unresolved files)
  $ hg remove foofile
  $ hg status --verbose
  M mergefile
  A bazfile
  R bar/file
  R foofile
  ? mergefile.orig
  ? untrackedfile
  # The repository is in an unfinished *merge* state.
  
  # No unresolved merge conflicts.
  
  # To continue:                hg commit
  # To abort:                   hg update --clean .    (warning: this will discard uncommitted changes)
  
  $ hg commit -m "merge commit"
  $ hg status --verbose
  ? mergefile.orig
  ? untrackedfile
