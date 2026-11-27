---
paths:
  - "tests/**"
  - "src/**/*_tests.rs"
  - "src/**/test_support.rs"
---

# Test rules for this repo

## Keep the suite lean

- Before writing a test, search for one that already covers the behavior and add
  a row to its table (`const` array of cases, `{case:?}` in every assert message).
- A new test must reproduce a bug or kill a mutant that no other test kills.
  Otherwise it is not added.
- Do not test std, serde, git itself, a derived impl, or a constant against its
  own literal. Do not assert pixel positions or mesh vertices unless that exact
  geometry is the product (then extract a pure function and test it).
- "One assertion per test" from the rust-best-practices skill does not apply here:
  table-driven tests with many rows are the house style.
- A refactor must not grow net test lines.

## Never

- Weaken an assertion, or `#[ignore]` a failing test, to make it pass. A failing
  security test is a finding: fix the code.
- Delete a test that is the only guard of a security invariant (token reaches
  only `api.github.com`; token never in logs/errors/settings/accessibility tree;
  `GIT_*` scrubbing; repository config never runs on reads; argv hardening
  prefix; timeouts kill the process group; output and body size caps; settings
  never corrupted or written through a symlink). Merge it instead.
- Touch the real keychain, the network, `Settings::default_path()`, or the
  process environment / working directory. Inject them: settings path, transport
  (`FakeTransport`), credential store, `DiscoverInputs`, git command environment.

## Mechanics

- `unsafe` is forbidden in tests too, so `env::set_var` is unavailable. When a
  case truly needs a hostile inherited environment, re-exec the test binary
  (`--exact` with the full module path, `--include-ignored`).
- Fake `git` executables: one shared script published with
  `common::shared_script` (unique temp name, closed, renamed into place), plus
  a per-case sidecar. Never write a script in place that another process may
  be executing (ETXTBSY under nextest).
- Anything that can hang uses `recv_timeout` and a fake that sleeps briefly.
  Never assert on wall-clock speed.
- Proptest: `prop_assert!`, default 64 cases, override with `PROPTEST_CASES=N`.
- egui_kittest: build harnesses with `step_dt` 1/60; prefer
  `query_all_by_role_and_label` and `click_accesskit`; `run()` stops after 4
  steps, use `step()` while a spinner is up.
