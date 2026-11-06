# Maintaining a GitHub fork

Use `origin` for your fork and `upstream` for the canonical repository. These
are remotes; `upstream/main` means the `main` branch on the upstream remote,
so a separate local branch named `upstream` is unnecessary. Confirm the
canonical repository URL before adding the remote; this checkout's source
metadata currently points to `byteowlz/oqto`.

Keep `main` as a clean mirror of upstream. Put fork-specific changes on
`rosinski`, preferably through short-lived topic branches. Start changes
intended for contribution to upstream from `main` instead.

```bash
git remote add upstream https://github.com/byteowlz/oqto.git
git fetch upstream

git switch main
git merge --ff-only upstream/main
git push origin main

git switch rosinski
git merge main
git push -u origin rosinski  # first push, or to set tracking
```

Create a topic branch for fork-specific work with
`git switch -c feat/<topic> rosinski`, then merge it back to `rosinski` when
ready. If the fast-forward update of `main` fails, inspect the histories
before choosing how to reconcile them; do not force-reset the branch.
