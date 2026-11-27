//! Fixtures shared by the UI tests.

use std::path::PathBuf;
use std::time::Duration;

use cthulhu_git::git::{Git, GitVersion};

/// A git that is installed nowhere. Every command it runs fails at once to
/// start, so a fixture repository never reaches the real git.
pub fn absent_git() -> Git {
    Git {
        path: PathBuf::from("/nonexistent/cthulhu-test-git"),
        version: GitVersion::parse("git version 2.43.0").expect("version"),
        command_timeout: Duration::from_secs(5),
    }
}
