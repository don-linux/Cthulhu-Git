//! Fixtures shared by the lib's unit tests.

use proptest::test_runner::Config;

/// The hash of the empty tree, for records that need a well-formed oid.
pub const OID_A: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// The case count comes from `PROPTEST_CASES`: 64 through
/// `.cargo/config.toml`, more from the shell for a deeper run. A failing seed
/// is not written into the tree; the failure becomes a row in a table test.
pub fn proptest_config() -> Config {
    Config {
        failure_persistence: None,
        ..Config::default()
    }
}
