//! Fixtures shared by the lib's unit tests.

use proptest::test_runner::Config;

/// Hashes of real objects, for records that need a well-formed oid.
pub const OID_A: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
pub const OID_B: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
pub const OID_C: &str = "8b137891791fe96927ad78e64b0aad7bded08eec";

/// The case count comes from `PROPTEST_CASES`: 64 through
/// `.cargo/config.toml`, more from the shell for a deeper run. A failing seed
/// is not written into the tree; the failure becomes a row in a table test.
pub fn proptest_config() -> Config {
    Config {
        failure_persistence: None,
        ..Config::default()
    }
}
