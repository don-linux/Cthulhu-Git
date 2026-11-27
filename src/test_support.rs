//! Fixtures shared by the lib's unit tests.

use proptest::test_runner::Config;

/// The case count comes from `PROPTEST_CASES`: 64 through
/// `.cargo/config.toml`, more from the shell for a deeper run. A failing seed
/// is not written into the tree; the failure becomes a row in a table test.
pub fn proptest_config() -> Config {
    Config {
        failure_persistence: None,
        ..Config::default()
    }
}
