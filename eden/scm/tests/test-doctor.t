Enable writing to hgcommits/v1:

  $ setconfig format.use-zstore-commit-data=1 format.use-zstore-commit-data-revlog-fallback=1

  $ rm .hg/store/hgcommits/v1/index2-id
  hgcommits/v1: repaired