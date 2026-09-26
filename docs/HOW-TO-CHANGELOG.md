# How to write release notes and publish a release

A release is published by pushing a Git tag, and only by that. Commits and
merges to `main` never build or publish anything. When a `v*` tag reaches
GitHub, [`release.yml`](../.github/workflows/release.yml) checks it, builds
every platform and creates the GitHub release, with your handwritten notes as
its description and the packages as its downloads.

## Versions and tags

Versions follow [Semantic Versioning](https://semver.org): `MAJOR.MINOR.PATCH`.
The tag is the version with a `v` in front.

| Tag | Meaning |
| --- | --- |
| `v0.0.1`, `v0.0.2` | Alpha series: early, anything can change |
| `v0.1.0` | First usable version |
| `v1.0.0` | Stable |

- Everything starting with `0.` is unstable by definition.
- Versions are numbers only: `v0.0.1` is valid, `v0.0.1-rc.1` or `0.0.1` is
  rejected by the workflow.
- The tag must match `version` in [`Cargo.toml`](../Cargo.toml) exactly:
  `v0.2.0` requires `version = "0.2.0"`. The workflow stops otherwise.
- Use annotated tags (`git tag -a`).
- A published tag is never moved or reused. If a release is wrong, publish the
  next patch version.

## Where the notes go

One Markdown file per version, named after the tag:

```text
docs/release-notes/
├── v0.0.1.md
├── v0.0.2.md
└── v0.1.0/          # optional: images for v0.1.0.md
    └── history.png
```

The workflow publishes `docs/release-notes/<tag>.md` as the release
description exactly as written. It refuses to release a tag without that
file. GitHub's automatic "generated release notes" are turned off.

Images live in a folder named after the version. Link them through the tag so
the link never breaks:
`https://raw.githubusercontent.com/don-linux/Cthulhu-Git/v0.1.0/docs/release-notes/v0.1.0/history.png`

## What goes in the file

Write for someone deciding whether to update, not for a reviewer. Say what
changed for them, not which files changed.

Section names follow [Keep a Changelog](https://keepachangelog.com). Use only
the sections that apply, in this order:

| Section | For |
| --- | --- |
| Added | New features |
| Changed | Changes to existing behavior |
| Deprecated | Features that will be removed later |
| Removed | Features removed in this version |
| Fixed | Bug fixes |
| Security | Vulnerabilities fixed |

Each entry starts with a short bold sentence, then one or two sentences of
detail. Credit contributors and link the pull request or issue:
`By @someone; thanks @reporter. (#12)`.

## Template

Copy this into `docs/release-notes/vX.Y.Z.md` and replace every `X.Y.Z` and
`vA.B.C` (the previous tag):

````markdown
Cthulhu Git X.Y.Z <one or two sentences: what this release is about>.

**Download Cthulhu Git:** [Mac (Apple Silicon and Intel)](https://github.com/don-linux/Cthulhu-Git/releases/download/vX.Y.Z/cthulhu-git-vX.Y.Z-macos-universal.dmg) · [Windows](https://github.com/don-linux/Cthulhu-Git/releases/download/vX.Y.Z/cthulhu-git-vX.Y.Z-x86_64-pc-windows-msvc.zip) · [Linux](https://github.com/don-linux/Cthulhu-Git/releases/download/vX.Y.Z/cthulhu-git-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz)

The Mac app is not signed by Apple yet. The first time, open it from Finder, then go to **System Settings → Privacy & Security** and click **Open Anyway**. From a terminal, `xattr -dr com.apple.quarantine "/Applications/Cthulhu Git.app"` does the same.

![What the screenshot shows](https://raw.githubusercontent.com/don-linux/Cthulhu-Git/vX.Y.Z/docs/release-notes/vX.Y.Z/screenshot.png)

## Added

- **Short sentence.** Detail. By @someone. (#12)

## Fixed

- **Short sentence.** Detail. By @someone; thanks @reporter. (#13)

## Thanks

@someone, @reporter, and everyone who reported problems after A.B.C.

**Full changelog**: https://github.com/don-linux/Cthulhu-Git/compare/vA.B.C...vX.Y.Z
````

Drop the screenshot line when there is no image. The first release has no
"Full changelog" line because there is no previous tag.

## Release checklist

1. **Open a pull request to `main`** containing:
   - `version = "X.Y.Z"` in `Cargo.toml` (run `cargo build` so `Cargo.lock`
     follows);
   - `docs/release-notes/vX.Y.Z.md`.
2. **Wait for CI.** [`ci.yml`](../.github/workflows/ci.yml) runs formatting,
   Clippy and tests, and builds the three platforms with the same steps as a
   release. The packages are downloadable from the run's **Artifacts** for 7
   days; try them before tagging.
3. **Merge** the pull request.
4. **Tag the merged commit and push the tag:**

   ```bash
   git switch main
   git pull origin main
   git tag -a vX.Y.Z -m "Cthulhu Git X.Y.Z"
   git push origin vX.Y.Z
   ```

5. **Watch the Release workflow** in the Actions tab. When it finishes, the
   release is on the
   [releases page](https://github.com/don-linux/Cthulhu-Git/releases).

Before building, the workflow checks that:

- the tag has the right format;
- the tag matches `Cargo.toml`;
- the notes file exists;
- the tagged commit is on `main`;
- formatting, Clippy and tests pass.

## If the Release workflow fails

The release is created only after every platform has built, so a failure
leaves a tag without a release. Fix it and tag the same version again:

```bash
git push origin :refs/tags/vX.Y.Z   # delete the tag on GitHub
git tag -d vX.Y.Z                   # delete it locally
```

Fix the problem in a new pull request, merge it, and repeat step 4 of the
checklist.

Only do this while no release exists for the tag. Once a release has been
published, people may have downloaded it: leave it alone and publish the next
patch version.

## What each release contains

| File | Platform |
| --- | --- |
| `cthulhu-git-vX.Y.Z-macos-universal.dmg` | macOS 11+, Apple Silicon and Intel in one app |
| `cthulhu-git-vX.Y.Z-x86_64-pc-windows-msvc.zip` | Windows x86-64, portable `.exe` |
| `cthulhu-git-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz` | Linux x86-64, built on GitHub's `ubuntu-latest`: needs a glibc at least as new as that runner's |
| `checksums.txt` | SHA-256 of every file above |
