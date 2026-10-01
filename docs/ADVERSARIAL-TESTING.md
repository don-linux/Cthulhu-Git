# Adversarial testing

Run a pass before a release that changes the Git layer (`src/git/`), settings,
startup, the window state machine, or the GitHub Actions workflows. A small
patch does not need one. The last write-up is
[adversarial-reports/v0.0.2.md](adversarial-reports/v0.0.2.md).

The tests assert the behavior the app should have. A failing assertion is a
finding. Do not weaken the test to match a bug, and do not `#[ignore]` it
until the finding is triaged.

## Rules

- Tests do not change the process environment or the working directory. When a
  case needs a private `PATH`, `HOME`, or `XDG_CONFIG_HOME`, re-exec the test
  binary the way `tests/repo.rs` does (`--exact`, `--include-ignored`).
- Fake `git` executables are symlinks to one shared script under
  `CARGO_TARGET_TMPDIR`, each with its own sidecar. A fresh executable per
  test races with `ETXTBSY`.
- A case that can hang uses `recv_timeout` and a fake that sleeps only a few
  seconds. Do not join the worker.
- Never read or write `Settings::default_path()`. Every settings file lives
  under `tempfile`.
- `cargo fmt` and `cargo clippy --all-targets -- -D warnings` stay clean.
- One area owns its files. Two passes must not edit the same file.

## Areas

| Area | Owns | Attacks |
| ---- | ---- | ------- |
| A. Git execution | `tests/adversarial_git_security.rs` | Clean and process filters, `include.path`, aliases, inherited `GIT_REPLACE_REF_BASE`, `GIT_SHALLOW_FILE`, `GIT_GRAFT_FILE`, `GIT_TRACE`, `GIT_CONFIG_*`. Mis-typed "not a git repository". Shell quoting of `safe.directory`. A slow `--version`. An output bomb. |
| B. Repository states | `tests/adversarial_repo_states.rs` | NUL, CR, bidi, and huge subjects. SHA-256. Worktrees, submodules, bare repos, a `.git` directory, a broken gitfile, a corrupt `HEAD`, shallow clones, packed-refs. Directory names ending in CR or LF, quotes, non-UTF-8. `history` limits, including `usize::MAX`. |
| C. Discovery | `tests/adversarial_discover.rs` | Relative `CTHULHU_GIT`, a symlink to a directory, a sleeping override, `2.15.0-rc0`, quoted `PATH` entries, a broken Xcode shim. `cargo check` for `x86_64-pc-windows-gnu` and `x86_64-apple-darwin`. A real Git 2.15 build is an experiment, not a committed test. |
| D. Settings | `tests/adversarial_settings.rs` | UTF-8 BOM, `null` fields, wrong types, a huge file. A settings symlink and a temp symlink pointing outside the config directory. A read-only parent. Concurrent saves. Torn JSON. |
| E. Window | `src/ui/adversarial_tests.rs` and the `mod` line in `src/ui/mod.rs` | Headless `egui_kittest` (`Harness::new_eframe`, `run_steps`, never `run` while a spinner is up). Corrupt settings must not be overwritten. Home must keep `last_repository`. Long names, bidi subjects, 1000 commits, the minimum window. Optional Xvfb screenshots of the release binary. |
| F. Actions | Report only, until the fix is reviewed | `actionlint` and `zizmor`. `${{ }}` inside `run:`. Actions pinned to a tag. Who can publish. Whether Windows and macOS run `cargo test`. |
| G. Parsers | `src/git/proptest_tests.rs` and the `mod` line in `src/git/mod.rs` | `proptest`, 64 cases, no spawned git. `GitVersion::parse`, `parse_head`, `parse_log`, `classify_failure`, `short_oid`, settings round-trip. No panics. |

`proptest` and `egui_kittest` (feature `eframe`) are already dev-dependencies.
Do not let a pass edit `Cargo.toml` unless a new harness is required.

## After the pass

1. Reproduce each finding and drop duplicates.
2. Critical and high: fix the code, one commit per fix, until the test passes.
3. Low: leave the behavior, note it under "Limits of this version" in the
   README, and say so in the report. A test that only documents a low finding
   may stay as a characterization test.
4. Write `docs/adversarial-reports/vX.Y.Z.md` with, for each finding: id,
   severity, status (fixed or accepted), the test name, how to reproduce it,
   and the fix or the reason it was kept.
