# Cthulhu Git

Desktop Git client in Rust (edition 2024) with egui/eframe. It shells out to the
system `git` (no libgit2). Two crates in one package:

- **lib** (`src/lib.rs`): `git/` (process execution, parsing, graph layout),
  `github/` (HTTPS via ureq, keychain token), `settings.rs`. No egui here.
- **bin** (`src/main.rs`, `src/ui/`): the window, built on egui.

## Commands

```bash
cargo fmt --all
cargo clippy --locked --all-targets -- -D warnings   # CI gate on every OS
cargo nextest run                                    # CI on Linux; one process per test
cargo test --locked                                  # CI on Windows and macOS
cargo llvm-cov --html                                # coverage report
cargo deny check licenses bans sources               # CI; advisories run weekly
cargo shear                                          # unused dependencies, CI
```

Hooks in `.claude/settings.json` run `cargo fmt` after every Rust edit and
clippy before a turn ends.

## Code rules

- `unsafe` is forbidden (`[lints.rust]`). Edition 2024 makes `env::set_var`
  unsafe, so code takes its environment as an argument instead of mutating it.
- No `unwrap`/`expect` outside tests unless the failure is impossible; say why.
  Silence a lint with `#[expect(lint, reason = "…")]`, never a bare `allow`.
- Errors: one `thiserror` enum per module; keep messages stable, the UI shows them.
- Do not add a dependency without asking. Prefer std and what is already locked.
  Cargo skips versions under a week old (`.cargo/config.toml` says how to override).
- Prefer deleting or merging code over adding a layer. No test-only hooks inside
  production code: inject the dependency (settings path, transport, store, git).
- Security invariants live in `docs/TESTING.md`: the token only reaches
  `api.github.com`, inherited `GIT_*` variables are scrubbed, repository config
  never runs code on reads, network input is size-capped. Keep their tests.
- Comments explain *why*. Do not narrate history or past bugs.
- Touching settings, fonts or themes: update `docs/SETTINGS.md`, `docs/FONTS.md`,
  `docs/THEMES.md`.

## Tests

Details in `.claude/rules/testing.md` (loaded when editing tests).

- Search for an existing test first and add a row to its table instead of a new test.
- A new test must reproduce a bug or kill a mutant nothing else kills.
- A refactor must not grow net test lines. Never weaken an assertion to make it pass.
- The skill `/slim-tests` holds the coverage + mutation workflow for pruning.
