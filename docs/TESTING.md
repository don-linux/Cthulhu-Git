# Testing

Unit tests live next to the code (`#[cfg(test)]`). The integration tests are
one crate, `tests/it/main.rs`, with a module per feature and the shared
fixtures in `tests/it/support.rs`.

Run an adversarial pass before a release that changes the Git layer
(`src/git/`), the network layer (`src/github/`, `src/ui/avatars.rs`),
settings, startup, the window state machine, or the GitHub Actions
workflows. A small patch does not need one. The write-ups are in
[adversarial-reports/](adversarial-reports/).

The tests assert the behavior the app should have. A failing assertion is a
finding. Do not weaken the test to match a bug, and do not `#[ignore]` it
until the finding is triaged.

## Rules

- Tests do not change the process environment or the working directory. When a
  case needs a hostile inherited environment or another working directory,
  re-run the test binary with `support::run_child` (`--exact
  <module>::<test>`, `--include-ignored`), as `hardening.rs` and
  `discover.rs` do.
- Fake `git` executables are symlinks to the one fake script
  (`support::fake_git`), published once under `CARGO_TARGET_TMPDIR`. Each
  link has its own sidecars. A fresh executable per test races with
  `ETXTBSY`.
- A case that can hang runs under `support::within` and a fake that sleeps
  only a few seconds. Do not join the worker. Never assert on elapsed time.
- A hostile fixture is armed: plain git must run what the app must not.
- Never read or write `Settings::default_path()`. Every settings file lives
  under `tempfile`.
- `cargo fmt` and `cargo clippy --all-targets -- -D warnings` stay clean.
- One area owns its files. Two passes must not edit the same file. The
  shared fixtures in `tests/it/support.rs` are no area's: change them in a
  commit of their own.

## Areas

| Area | Owns | Attacks |
| ---- | ---- | ------- |
| A. Git execution | `tests/it/hardening.rs`, `tests/it/process.rs` | Repository config that runs code: clean and process filters, `include.path`, textconv, `diff.external`, `gpg.program` with `log.showSignature`, fsmonitor, hooks. The inherited environment: `GIT_DIR`, `GIT_WORK_TREE`, every `GIT_CONFIG_*` source, `GIT_REPLACE_REF_BASE`, `GIT_SHALLOW_FILE`, `GIT_GRAFT_FILE`, `GIT_EXEC_PATH`, the trace variables. Shell quoting of `safe.directory`. A hung `--version`, a command past its limit (with a `setsid` grandchild), an output bomb, a grandchild holding stdout. |
| B. Repository states | `tests/it/open.rs`, `tests/it/read.rs`, `tests/it/write.rs` | NUL, CR, bidi, and huge subjects. SHA-256. Worktrees, bare repos, a `.git` directory, a broken gitfile, a dangling or garbage `HEAD`, a branch named `HEAD`, shallow clones, packed-refs. Directory names ending in CR or LF, quotes, non-UTF-8. Graph limits, including `usize::MAX`. Switch, pull and fetch, refused and allowed. |
| C. Discovery | `tests/it/discover.rs` | Relative `CTHULHU_GIT`, an override with a space, too old, broken or garbage `--version`, fall-through between candidates, `git.exe` naming. PATH parsing (relative, empty and quoted entries, the xcrun shim, the fallback order) is in the unit tests of `src/git/discover.rs`. `cargo check` for `x86_64-pc-windows-gnu` and `x86_64-apple-darwin` (needs MinGW-w64 and an Apple clang toolchain for `ring`'s C code; see the README's Development section). A real Git 2.15 build is an experiment, not a committed test. |
| D. Settings | `tests/it/settings.rs` | Wrong types, a huge file. A settings symlink, a temp symlink pointing outside the config directory, and a leftover temp file. A directory at the settings path, a read-only parent. Concurrent saves. Torn JSON. The UTF-8 BOM and `null` fields are rows of the `from_json` table in `src/settings.rs`. |
| E. Window | `src/ui/adversarial_tests.rs` (the real window through `egui_kittest`) and the unit tests in `src/ui/mod.rs` (`apply_action` and the poll functions, no harness) | Headless `egui_kittest` (`Harness::new_eframe`, `run_steps`, never `run` while a spinner is up). Corrupt settings must not be overwritten. Home must keep `last_repository`. Long names, bidi subjects, the minimum window. Optional Xvfb screenshots of the release binary. |
| F. Actions | Report only, until the fix is reviewed | `actionlint` and `zizmor`. `${{ }}` inside `run:`. Actions pinned to a tag. Who can publish. Whether Windows and macOS run `cargo test`. |
| G. Parsers | The `proptest!` blocks and tables in the unit tests of `src/git/exec.rs`, `src/git/graph.rs`, `src/git/log.rs` and `src/settings.rs` | `proptest`, 64 cases, no spawned git. `GitVersion::parse`, the graph log with authors and trailers, `parse_co_authors`, `classify_failure`, `short_oid`, settings round-trip. No panics. |
| H. Network and token | `src/github/http.rs` (host allowlist, redirects, body cap), `src/github/adversarial_tests.rs` (threats that cross modules), and the unit tests of `src/ui/avatars.rs` (image decoding), `src/ui/hover.rs`, `src/ui/settings.rs` (the token field) and `src/ui/github_auth.rs` | Everything from the network is untrusted: JSON, headers, redirects, images (size and dimension limits), rate-limit answers. The host allowlist (userinfo, ports, trailing dots, IDN, downgrades). The token reaches only `api.github.com`, never a log, an error string, `settings.json` or the accessibility tree. Through `FakeTransport` and `AvatarService::disabled()` only: no test touches the network or the real keychain. |
| I. History table | `src/ui/history_tests.rs` and its `mod` line | Titles start where their columns start, at every width and scroll offset. Resizing one column never changes another. Long names, summaries and co-author lists stay inside their cells. A thousand commits stay virtualized. |

`proptest` and `egui_kittest` (feature `eframe`) are already dev-dependencies.
Do not let a pass edit `Cargo.toml` unless a new harness is required.

The one live check, `CTHULHU_LIVE_GITHUB=1 cargo test live_github -- --ignored`,
makes one API request and one avatar download. Without the variable it
returns at once, even under `--include-ignored`.

## After the pass

1. Reproduce each finding and drop duplicates.
2. Critical and high: fix the code, one commit per fix, until the test passes.
3. Low: leave the behavior, note it under "Limits of this version" in the
   README, and say so in the report. A test that only documents a low finding
   may stay as a characterization test.
4. Write `docs/adversarial-reports/vX.Y.Z.md` with, for each finding: id,
   severity, status (fixed or accepted), the test name, how to reproduce it,
   and the fix or the reason it was kept.
