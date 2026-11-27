# Testing

Unit tests live next to the code (`#[cfg(test)]`). Their shared fixtures are
`src/test_support.rs` for the library (`FakeTransport`, the proptest config)
and `src/ui/test_support.rs` for the window. The integration tests are one
crate, `tests/it/main.rs`, with a module per feature and the shared fixtures
in `tests/it/support.rs`. `discover` and `hardening` are Unix only. The window
tests in `src/ui/adversarial_tests.rs` include the same `support.rs` as
`fixtures`.

Run an adversarial pass before a release that changes the Git layer
(`src/git/`), the network layer (`src/github/`, `src/ui/avatars.rs`),
settings, startup, the window state machine, or the GitHub Actions
workflows. A small patch does not need one. The write-ups are in
[adversarial-reports/](adversarial-reports/).

The tests assert the behavior the app should have. A failing assertion is a
finding. Do not weaken the test to match a bug, and do not `#[ignore]` it
until the finding is triaged.

## Running the tests

```bash
cargo nextest run                 # every test in its own process
cargo nextest run --profile ci    # what Linux CI runs
cargo test --locked               # Windows and macOS CI; doctests everywhere
cargo llvm-cov --html             # coverage report
```

The nextest profiles are in [`.config/nextest.toml`](../.config/nextest.toml):

| Profile | Used by | Settings |
| ------- | ------- | -------- |
| `default` | `cargo nextest run` | A test still running after 60 s is reported, and killed after three such periods |
| `ci` | Linux CI | Runs every test after a failure and prints each failure at once and again at the end |
| `mutants` | `cargo mutants` | Stops at the first failure, which already proves the mutant is caught; kills a test after two 30 s periods |

nextest skips doctests, so Linux CI also runs `cargo test --doc`.
Property tests run `PROPTEST_CASES` cases, 64 from `[env]` in
[`.cargo/config.toml`](../.cargo/config.toml); a value already in the shell
wins, so `PROPTEST_CASES=1000 cargo nextest run` runs a deeper pass.
`proptest_config()` in both `test_support.rs` files turns failure
persistence off: a failure found this way becomes a named row of a table.

### Code that kills processes

The Git timeouts (`src/git/exec.rs`, `src/git/reap.rs`) and the terminal's
session kill signal whole process trees. A bug there can reach every process
of the user, so on Linux run each test binary in its own PID namespace when
you touch them:

```bash
CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER="bwrap --die-with-parent \
  --unshare-pid --new-session --dev-bind / / --proc /proc" cargo nextest run
```

`--new-session` gives the binary a process group inside the namespace;
without it `getpgrp()` is 0 and the reap tests fail.

### Mutation testing

[`.cargo/mutants.toml`](../.cargo/mutants.toml) makes `cargo mutants` run the
tests through nextest with the `mutants` profile. It never mutates
`src/git/reap.rs` or the terminal's session kill (`should_signal`,
`stop_session`, `signal_pid`, `kill_process_tree` and their helpers): a
mutant such as `is_descendant -> true` widens the kill to every process the
user owns and ends the desktop session. Their unit tests still run. It also
skips `src/main.rs`, `src/ui/theme/`, `src/ui/icons.rs`, `src/ui/fonts.rs`,
test code, and `Debug` and `Display` impls. Keep any new kill path out of
mutation the same way, and run mutants under the PID-namespace runner above.
cargo-mutants copies the tree to `$TMPDIR`; keep that outside any Git
repository, or the fixtures find this one. The `/slim-tests` skill holds the
coverage and mutation workflow.

## Rules

- Tests do not change the process environment or the working directory. When a
  case needs a hostile inherited environment or another working directory,
  re-run the test binary with `support::run_child` (`--exact
  <module>::<test>`, `--include-ignored`), as `hardening.rs` and
  `discover.rs` do.
- Fake `git` executables are symlinks to the one fake script
  (`support::fake_git`). `support::shared_script` publishes it once under
  `CARGO_TARGET_TMPDIR`: written under a unique temporary name, closed, then
  renamed into place. Each link has its own sidecars. A script written in
  place while another process runs it fails with `ETXTBSY`.
- A case that can hang runs under `support::within` and a fake that sleeps
  only a few seconds. Do not join the worker. Never assert on elapsed time.
- A hostile fixture is armed: plain git must run what the app must not.
- Never read or write `Settings::default_path()`. Every settings file lives
  under `tempfile`; the window gets its own through a `Sandbox` and
  `Startup`.
- Never touch the network or the real keychain. Requests go through
  `FakeTransport`, the window gets `AvatarService::disabled()`, and the
  keychain tests hand `Keychain` an entry from keyring-core's mock store.
- `cargo fmt` and `cargo clippy --all-targets -- -D warnings` stay clean.
- One area owns its files. Two passes must not edit the same file. The
  shared fixtures in `tests/it/support.rs` are no area's: change them in a
  commit of their own.

## Security invariants

Each invariant has at least one test that fails when it breaks. Merge such a
test into a table if you must, never delete it.

| Invariant | Tests |
| --------- | ----- |
| The token reaches only `api.github.com` | `only_https_on_the_allowed_hosts_is_requested`, `redirects_stay_on_the_allowed_hosts_and_keep_the_token_on_the_api` (`src/github/http.rs`); `hostile_author_avatar_urls_never_leave_the_allowlist`, `get_does_not_authorize_a_random_url` (`src/github/adversarial_tests.rs`); `images_are_missing_on_404_and_never_carry_the_token` (`src/github/client.rs`) |
| The token is never in an error, `settings.json`, the accessibility tree or the painted screen | `errors_never_show_the_token` (`src/github/adversarial_tests.rs`); `debug_never_prints_the_token` (`src/github/token.rs`); the exact key list in `to_json_normalizes_and_round_trips` (`src/settings.rs`); `a_typed_token_stays_masked_and_out_of_links_and_copied_text` (`src/ui/settings.rs`). Nothing installs a logger; the comment on `UreqTransport` says what a future one must keep below TRACE |
| Inherited `GIT_*` variables are scrubbed, and `PATH` has no empty or relative entry | `git_env_drops_inherited_git_vars_and_relative_path_entries` (`src/git/exec.rs`); `hostile_inherited_environment_is_ignored` (`tests/it/hardening.rs`); for the terminal, `shell_argv_runs_an_absolute_shell_without_the_unset_names` and `unset_names_ignore_values_and_names_that_are_not_unicode` (`src/ui/terminal.rs`) |
| Repository config never runs code on reads | `hostile_repository_config_never_runs_on_reads` (`tests/it/hardening.rs`) |
| Every git call goes behind the hardening argv prefix | `git_runs_behind_the_hardening_prefix` (`src/git/exec.rs`); `fetch_args_table`, `switch_request_table` (`src/git/sync.rs`); `pull_ff_only_never_rebases` (`tests/it/write.rs`) |
| A timeout kills the process group, and helpers that left it | `timeout_kills_the_process_group`, `a_hung_version_probe_is_killed_and_skipped`, `background_writer_does_not_pin_a_finished_git`, `a_gh_grandchild_holding_stdout_does_not_pin_the_token_import` (`tests/it/process.rs`); `killing_the_group_after_the_leader_has_exited_returns` (`src/git/reap.rs`); `run_gh_kills_a_hung_child_and_reads_only_a_signed_in_token` (`src/github/token.rs`); `stop_session_kills_a_background_job_that_ignores_hup`, `dropped_shell_kills_a_job_that_left_the_session`, `should_signal_refuses_init_and_this_process` (`src/ui/terminal.rs`) |
| Git output and network bodies are size-capped | `output_past_the_cap_is_refused` (`tests/it/process.rs`); `read_capped_table` (`src/git/exec.rs`); `bodies_are_capped_after_decompression`, `body_read_errors_are_classified_like_request_errors` (`src/github/http.rs`); `decode_refuses_other_formats_and_broken_pictures` (`src/ui/avatars.rs`) |
| Settings are never corrupted, overwritten after a failed load, or written through a symlink | `save_never_writes_through_a_planted_entry`, `concurrent_saves_leave_valid_json`, `invalid_files_are_parse_errors_and_stay_untouched`, `unwritable_parent_directory_is_a_write_error` (`tests/it/settings.rs`); `a_directory_at_the_settings_path_is_a_read_and_a_write_error` (`src/settings.rs`); `settings_are_written_only_after_a_good_load` (`src/ui/mod.rs`); `opening_a_folder_ends_in_the_repository_or_a_home_error` (`src/ui/adversarial_tests.rs`); `avatar_cache_does_not_write_through_a_planted_symlink` (`src/ui/avatars.rs`) |
| The terminal never runs a program planted in the repository | `relative_shell_does_not_execute_a_planted_binary`, `non_executable_absolute_shell_falls_through`, `windows_shell_prefers_pwsh_then_windows_powershell` (the quoted ConPTY path) (`src/ui/terminal.rs`) |

## Areas

| Area | Owns | Attacks |
| ---- | ---- | ------- |
| A. Git execution | `tests/it/hardening.rs`, `tests/it/process.rs` | Repository config that runs code: clean and process filters, `include.path`, textconv, `diff.external`, `gpg.program` with `log.showSignature`, fsmonitor, hooks. The inherited environment: `GIT_DIR`, `GIT_WORK_TREE`, every `GIT_CONFIG_*` source, `GIT_REPLACE_REF_BASE`, `GIT_SHALLOW_FILE`, `GIT_GRAFT_FILE`, `GIT_EXEC_PATH`, the trace variables. Shell quoting of `safe.directory`. A hung `--version`, a command past its limit (with a `setsid` grandchild), an output bomb, a grandchild holding stdout, a `gh` grandchild holding the token import. |
| B. Repository states | `tests/it/open.rs`, `tests/it/read.rs`, `tests/it/write.rs` | NUL, CR, bidi, and huge subjects. SHA-256. Worktrees, bare repos, a `.git` directory, a broken gitfile, a dangling or garbage `HEAD`, a branch named `HEAD`, shallow clones, packed-refs. Directory names ending in CR or LF, quotes, non-UTF-8. Graph limits, including `usize::MAX`. Switch, pull and fetch, refused and allowed, including an upstream through a remote whose name has a `/`. |
| C. Discovery | `tests/it/discover.rs` | Relative `CTHULHU_GIT`, an override with a space, too old, broken or garbage `--version`, fall-through between candidates, `git.exe` naming. PATH parsing (relative, empty and quoted entries, the xcrun shim, the fallback order of every platform) is in the unit tests of `src/git/discover.rs`. `cargo check` for `x86_64-pc-windows-gnu` and `x86_64-apple-darwin` (needs MinGW-w64 and an Apple clang toolchain for `ring`'s C code; see the README's Development section). A real Git 2.15 build is an experiment, not a committed test. |
| D. Settings | `tests/it/settings.rs` | Wrong types, a huge file. A settings symlink, a temp symlink pointing outside the config directory, and a leftover temp file. A read-only parent. Concurrent saves. Torn JSON. A directory at the settings path, the UTF-8 BOM and `null` fields are unit tests and rows of the `from_json` table in `src/settings.rs`. |
| E. Window | `src/ui/adversarial_tests.rs` (the real window through `egui_kittest`) and the unit tests in `src/ui/mod.rs` (`apply_action` and the poll functions, no harness) | Headless `egui_kittest`: the app is built with `HarnessBuilder::build_eframe` from a `Sandbox`, and settled with `step`, never `run` while a spinner is up. Corrupt settings must not be overwritten. Home must keep `last_repository`. A branch named like a tag still opens. Long names, bidi subjects, the minimum window. Every button and checkbox has an accessible name. Optional Xvfb screenshots of the release binary. |
| F. Actions | Report only, until the fix is reviewed | `actionlint` and `zizmor`. `${{ }}` inside `run:`. Actions pinned to a tag. Who can publish. Whether Windows and macOS run `cargo test`. |
| G. Parsers | The `proptest!` blocks and tables in the unit tests of `src/git/exec.rs`, `src/git/graph.rs`, `src/git/log.rs` and `src/settings.rs` | `proptest`, no spawned git. `GitVersion::parse`, the graph log with authors and trailers, `parse_co_authors` (also in linear time on a huge line), `classify_failure`, `short_oid`, the diff headers, settings round-trip. No panics. |
| H. Network and token | `src/github/http.rs` (host allowlist, redirects, body cap), `src/github/adversarial_tests.rs` (threats that cross modules), and the unit tests of `src/ui/avatars.rs` (image decoding), `src/ui/hover.rs`, `src/ui/settings.rs` (the token field) and `src/ui/github_auth.rs` | Everything from the network is untrusted: JSON, headers, redirects, images (size and dimension limits), rate-limit answers. The host allowlist (userinfo, ports, trailing dots, IDN, downgrades). The token reaches only `api.github.com`, never a log, an error string, `settings.json`, the accessibility tree or the painted screen. Through `FakeTransport`, `AvatarService::disabled()` and the mock keychain only: no test touches the network or the real keychain. |
| I. History table | `src/ui/history_tests.rs` and its `mod` line, plus the column layout proptest and tables in `src/ui/history.rs` | Titles start where their columns start, at every width and scroll offset. Resizing one column never changes another. Long names, summaries and co-author lists stay inside their cells. A thousand commits stay virtualized. |

`proptest`, `egui_kittest` (feature `eframe`) and `keyring-core` (its mock
store) are already dev-dependencies. Do not let a pass edit `Cargo.toml`
unless a new harness is required.

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
