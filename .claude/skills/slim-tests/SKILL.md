---
name: slim-tests
description: Prune or merge tests in one module safely, proving with coverage and mutation testing that no protection is lost. Use when asked to slim, dedupe or consolidate tests.
disable-model-invocation: true
---

# Slim tests safely

Goal: fewer test lines with the same protection. Evidence decides, not taste.

## 1. Baseline

```bash
cargo nextest run                       # must be green first
cargo llvm-cov --html --ignore-filename-regex '(_tests|test_support)\.rs$' \
  --output-dir target/cov-base
cargo mutants --list -f <file> | wc -l  # size the mutation run
cargo mutants -f <file> -j2 -o target/mut-base
```

`cargo mutants` reports per mutant (caught / missed / timeout), not which test
killed it, so compare whole runs.

cargo-mutants copies the tree to `$TMPDIR`. Keep that directory outside any
git repository (never under `target/`): the fixtures assume no repository
above their temp dirs, and git would otherwise find this one.

## 2. Classify each test in the module

- **Keep**: unique behavior or a security invariant (see `.claude/rules/testing.md`).
- **Merge**: same setup, different inputs → one table; every old case becomes a row.
- **Delete**: tests std / serde / git / a derive / a constant, is tautological, or
  is fully covered by another test (read both before deciding).
- **Move**: a full UI or process test whose logic fits a pure function → extract
  the function, test it directly.

## 3. Change, then prove

```bash
cargo nextest run
cargo llvm-cov --html ... --output-dir target/cov-new   # covered prod lines must not drop
cargo mutants -f <file> -j2 -o target/mut-new
comm -23 <(sort target/mut-base/mutants.out/caught.txt) \
         <(sort target/mut-new/mutants.out/caught.txt)        # must be empty
```

A non-empty `comm` output lists mutants nobody kills any more: restore the test
(or a row) that killed them. Mutants in deleted code are expected.

## 4. Commit

One commit per module. Put the before/after test count, line count and the
(empty) `comm` result in the message.
