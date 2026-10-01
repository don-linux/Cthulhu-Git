use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::exec::{Git, GitError};

/// Environment variable that pins the git executable and skips the search.
pub const OVERRIDE_VAR: &str = "CTHULHU_GIT";

/// Never `git.cmd` / `git.bat` on Windows: those need a shell to run.
const EXE_NAME: &str = if cfg!(windows) { "git.exe" } else { "git" };

/// Everything the search depends on, so it can be exercised without touching
/// the process environment.
#[derive(Debug, Clone)]
pub struct DiscoverInputs {
    pub override_path: Option<OsString>,
    pub path_var: Option<OsString>,
    pub exe_name: &'static str,
    pub fallbacks: Vec<PathBuf>,
}

/// Executables found on disk, in priority order, plus every location looked at.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Candidates {
    pub existing: Vec<PathBuf>,
    pub searched: Vec<PathBuf>,
}

impl DiscoverInputs {
    pub fn from_env() -> Self {
        Self {
            override_path: env::var_os(OVERRIDE_VAR).filter(|value| !value.is_empty()),
            path_var: env::var_os("PATH"),
            exe_name: EXE_NAME,
            fallbacks: platform_fallbacks(),
        }
    }
}

impl Git {
    /// Finds the system git: `CTHULHU_GIT`, then `PATH`, then well-known install locations.
    pub fn discover() -> Result<Self, GitError> {
        discover_with(&DiscoverInputs::from_env())
    }
}

/// The first candidate that runs and reports a supported version wins; a
/// candidate that fails `--version` does not stop the search.
pub fn discover_with(inputs: &DiscoverInputs) -> Result<Git, GitError> {
    if let Some(raw) = &inputs.override_path {
        let requested = PathBuf::from(raw);
        if !is_executable(&requested) {
            return Err(GitError::BadOverride(requested));
        }
        // A bare name such as `git` passes the cwd metadata check, but
        // `Command` would search PATH and might run a different executable.
        // Spawn `./git` and still report the path the user set.
        let mut git = Git::probe(&override_spawn_path(&requested))?;
        git.path = requested;
        return supported(git);
    }

    let candidates = find_git_in(inputs);
    let mut too_old = None;

    for candidate in &candidates.existing {
        match Git::probe(candidate) {
            Ok(git) if git.version.is_supported() => return Ok(git),
            Ok(git) => {
                too_old.get_or_insert(git.version);
            }
            Err(_) => {}
        }
    }

    Err(match too_old {
        Some(version) => GitError::TooOld(version),
        None => GitError::NotFound {
            tried: candidates.searched,
        },
    })
}

/// `git` with no slash is one relative component. `./git` already has a slash.
fn override_spawn_path(path: &Path) -> PathBuf {
    let bare = path.is_relative() && path.components().count() == 1;
    if bare {
        Path::new(".").join(path)
    } else {
        path.to_path_buf()
    }
}

/// Some Windows launchers leave the quotes around a PATH element that contains
/// spaces. A quoted absolute directory would otherwise look relative and be skipped.
fn unquote_path_entry(dir: PathBuf) -> PathBuf {
    let Some(text) = dir.to_str() else {
        return dir;
    };
    if text.len() >= 2 && text.starts_with('"') && text.ends_with('"') {
        PathBuf::from(&text[1..text.len() - 1])
    } else {
        dir
    }
}

fn supported(git: Git) -> Result<Git, GitError> {
    if git.version.is_supported() {
        Ok(git)
    } else {
        Err(GitError::TooOld(git.version))
    }
}

/// Lists executable candidates from `PATH` and the fallbacks, without spawning anything.
///
/// Relative and empty `PATH` entries are skipped: they resolve against the
/// current directory, which would let a repository plant its own `git.exe`.
pub(crate) fn find_git_in(inputs: &DiscoverInputs) -> Candidates {
    let from_path = inputs
        .path_var
        .iter()
        .flat_map(env::split_paths)
        .map(unquote_path_entry)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(inputs.exe_name));

    let mut candidates = Candidates::default();
    for path in from_path.chain(inputs.fallbacks.iter().cloned()) {
        if candidates.searched.contains(&path) {
            continue;
        }
        if is_executable(&path) {
            candidates.existing.push(path.clone());
        }
        candidates.searched.push(path);
    }
    candidates
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Apps started from Finder get a minimal `PATH` without Homebrew.
/// `/usr/bin/git` is the Xcode shim: without the Command Line Tools it fails
/// `--version` and the search moves on.
#[cfg(target_os = "macos")]
fn platform_fallbacks() -> Vec<PathBuf> {
    [
        "/opt/homebrew/bin/git",
        "/usr/local/bin/git",
        "/usr/bin/git",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_fallbacks() -> Vec<PathBuf> {
    ["/usr/bin/git", "/usr/local/bin/git"]
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

/// Git for Windows (machine and per-user installs) and Scoop, which are often
/// missing from `PATH` for GUI apps launched before the installer updated it.
#[cfg(windows)]
fn platform_fallbacks() -> Vec<PathBuf> {
    [
        ("ProgramFiles", r"Git\cmd\git.exe"),
        ("ProgramFiles", r"Git\bin\git.exe"),
        ("ProgramFiles(x86)", r"Git\cmd\git.exe"),
        ("LOCALAPPDATA", r"Programs\Git\cmd\git.exe"),
        ("USERPROFILE", r"scoop\shims\git.exe"),
    ]
    .into_iter()
    .filter_map(|(var, tail)| {
        let base = PathBuf::from(env::var_os(var)?);
        base.is_absolute().then(|| base.join(tail))
    })
    .collect()
}

#[cfg(not(any(unix, windows)))]
fn platform_fallbacks() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn inputs(path_dirs: &[&Path], fallbacks: Vec<PathBuf>) -> DiscoverInputs {
        DiscoverInputs {
            override_path: None,
            path_var: Some(env::join_paths(path_dirs).expect("joinable PATH")),
            exe_name: "git",
            fallbacks,
        }
    }

    #[test]
    fn missing_override_is_reported_before_searching() {
        let mut inputs = inputs(&[], Vec::new());
        inputs.override_path = Some("/no/such/cthulhu-git".into());
        let error = discover_with(&inputs).expect_err("missing override");
        assert!(
            matches!(error, GitError::BadOverride(path) if path == Path::new("/no/such/cthulhu-git"))
        );
    }

    #[test]
    fn override_pointing_to_a_directory_is_rejected() {
        let dir = TempDir::with_prefix("cthulhu-override-").expect("temp dir");
        let mut inputs = inputs(&[], Vec::new());
        inputs.override_path = Some(dir.path().into());
        let error = discover_with(&inputs).expect_err("directory is not git");
        assert!(matches!(error, GitError::BadOverride(path) if path == dir.path()));
    }

    #[test]
    fn relative_and_empty_path_entries_are_not_searched() {
        let inputs = DiscoverInputs {
            override_path: None,
            path_var: Some(env::join_paths([".", "", "bin", "/usr/bin"]).expect("PATH")),
            exe_name: "git",
            fallbacks: Vec::new(),
        };
        let searched = find_git_in(&inputs).searched;
        assert_eq!(searched, vec![PathBuf::from("/usr/bin/git")]);
    }

    #[test]
    fn duplicate_locations_are_searched_once() {
        let inputs = inputs(
            &[Path::new("/opt/a"), Path::new("/opt/a")],
            vec![PathBuf::from("/opt/a/git")],
        );
        assert_eq!(
            find_git_in(&inputs).searched,
            vec![PathBuf::from("/opt/a/git")]
        );
    }

    #[test]
    fn not_found_lists_every_searched_location() {
        let empty = TempDir::with_prefix("cthulhu-empty-").expect("temp dir");
        let fallback = empty.path().join("fallback").join("git");
        let error = discover_with(&inputs(&[empty.path()], vec![fallback.clone()]))
            .expect_err("nothing to find");
        let GitError::NotFound { tried } = error else {
            panic!("expected NotFound, got {error:?}");
        };
        assert_eq!(tried, vec![empty.path().join("git"), fallback]);
    }

    #[cfg(unix)]
    #[test]
    fn file_without_execute_bit_is_not_a_candidate() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::with_prefix("cthulhu-exec-bit-").expect("temp dir");
        let plain = tmp.path().join("plain");
        let executable = tmp.path().join("executable");
        for (dir, mode) in [(&plain, 0o644), (&executable, 0o755)] {
            fs::create_dir(dir).expect("dir");
            fs::write(dir.join("git"), "").expect("write");
            fs::set_permissions(dir.join("git"), fs::Permissions::from_mode(mode)).expect("chmod");
        }

        let found = find_git_in(&inputs(&[&plain, &executable], Vec::new()));
        assert_eq!(found.existing, vec![executable.join("git")]);
        assert_eq!(found.searched.len(), 2);
    }
}
