---
name: release
description: Release a JeffyCoin version, or prepare one. Use when Jonathan says to release, tag, "push the next version", or "start the build", and when finishing a change that should ship. Covers the version bump, the changelog move, and tools/release.
---

# Releasing JeffyCoin

A release is a `v*` tag on `main`. The tag starts the build for Windows and
Linux (`.github/workflows/release.yml`). The workflow refuses a tag that is
not on `main` or does not equal the version in `app/Cargo.toml`.

Push, merge, and tag only when Jonathan has said to. Preparing (steps 1 to 4)
needs no permission; step 5 does.

## Steps

1. **Version.** `tools/planned` shows the version of each planned release;
   ship the first one. Patch (`2.2.x`) for fixes and features. Minor
   (`2.x.0`) for a consensus change, which also needs a higher block version
   and his decision. Set `version` in `app/Cargo.toml`, then build the app
   once so `app/Cargo.lock` follows.
2. **Changelog.** In `CHANGELOG.md` add `## <version> (<date, UTC>)` at the
   top: what changed, in the reader's terms, with `Fixed` / `Added` /
   `Changed` and a `Consensus:` line (`tools/planned show <version>` has the
   source text). Delete the release's `[[release]]` block and its items from
   `planned.toml`, then `tools/planned check`. Say what was not tested
   (usually Windows).
3. **Check.** `tools/check --gpu` if the miner or app mining changed,
   otherwise `tools/check`.
4. **Commit** on the work branch. No attribution lines.
5. **Release** (only when told): `tools/release <version> --yes --no-check`.
   It pushes the branch, fast-forwards `main`, tags, waits for the build, and
   prints the result. Without `--yes` it only says what it would do.

## After the build

- Report the result in two or three lines with the release link.
- The app's updater offers the new version to installed copies; a copy that
  is not installed must be installed first.
- If the release changes consensus, older versions are not prompted beyond a
  line in the Updates section (2.1.2 and earlier) or the newer-rules banner
  (2.2.0 and later). Tell Jonathan who has to be told.
- The Windows build has not been run by anyone at this point. Confirm it from
  a friend's logs with the `sentry-logs` skill.

## If the build fails

`tools/release` prints the end of the failed step. Fix on the branch, then
make a new patch version; do not move a tag that was pushed.
