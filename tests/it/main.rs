//! Integration tests: the library against the system git, fake gits, and real
//! files. One crate, so the library links into one test binary.
//!
//! A test that needs a hostile inherited environment or another working
//! directory re-runs this binary with `--exact <module>::<test>`, because the
//! process environment is never changed here.

mod settings;
mod support;
