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
    /// Absolute executables removed from `PATH` candidates before fallbacks.
    ///
    /// [`DiscoverInputs::from_env`] sets this to `/usr/bin/git` on macOS only.
    /// Finder's `PATH` lists that path first, and without the Command Line
    /// Tools it is an `xcrun` shim that opens the installer and often never
    /// exits. Linux tests set the list explicitly. Matching uses the canonical
    /// path, so a symlink to the same file is dropped too. Fallbacks are not
    /// filtered: the macOS table still appends `/usr/bin/git` when a real
    /// developer git is installed.
    pub skip_paths: Vec<PathBuf>,
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
            skip_paths: path_skips_from_env(),
        }
    }
}

/// `/usr/bin/git` on macOS. Other platforms leave the list empty so a Linux
/// test can opt in by filling [`DiscoverInputs::skip_paths`] itself.
fn path_skips_from_env() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        vec![PathBuf::from(MACOS_SHIM_GIT)]
    } else {
        Vec::new()
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
/// [`DiscoverInputs::skip_paths`] drops PATH entries only. A fallback that
/// names the same file is still searched.
pub(crate) fn find_git_in(inputs: &DiscoverInputs) -> Candidates {
    let from_path = inputs
        .path_var
        .iter()
        .flat_map(env::split_paths)
        .map(unquote_path_entry)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(inputs.exe_name))
        .filter(|path| !is_skipped_path(path, &inputs.skip_paths));

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

fn is_skipped_path(path: &Path, skip: &[PathBuf]) -> bool {
    skip.iter().any(|skipped| same_absolute_path(path, skipped))
}

/// Literal equality, then the canonical path so a symlink and its target match.
/// A path that is not absolute, or that does not exist, matches only literally.
fn same_absolute_path(left: &Path, right: &Path) -> bool {
    if !left.is_absolute() || !right.is_absolute() {
        return false;
    }
    if left == right {
        return true;
    }
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
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

const MACOS_HOMEBREW_GIT: &str = "/opt/homebrew/bin/git";
const MACOS_USR_LOCAL_GIT: &str = "/usr/local/bin/git";
const MACOS_MACPORTS_GIT: &str = "/opt/local/bin/git";
const MACOS_CLT_GIT: &str = "/Library/Developer/CommandLineTools/usr/bin/git";
const MACOS_XCODE_GIT: &str = "/Applications/Xcode.app/Contents/Developer/usr/bin/git";
const MACOS_SHIM_GIT: &str = "/usr/bin/git";

/// Finder starts the app with a `PATH` that has no Homebrew. `/usr/bin/git`
/// on macOS is a stub that opens the Command Line Tools installer and often
/// never exits, so it is searched only when a real developer git is already
/// installed. MacPorts lives at `/opt/local/bin/git`, after Homebrew and
/// `/usr/local`. `exists` is injected so Linux tests can lock the table.
fn macos_fallback_paths(exists: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let clt = Path::new(MACOS_CLT_GIT);
    let xcode = Path::new(MACOS_XCODE_GIT);
    let mut paths = vec![
        PathBuf::from(MACOS_HOMEBREW_GIT),
        PathBuf::from(MACOS_USR_LOCAL_GIT),
        PathBuf::from(MACOS_MACPORTS_GIT),
    ];
    if exists(clt) {
        paths.push(clt.to_path_buf());
    }
    if exists(xcode) {
        paths.push(xcode.to_path_buf());
    }
    if exists(clt) || exists(xcode) {
        paths.push(PathBuf::from(MACOS_SHIM_GIT));
    }
    paths
}

fn linux_fallback_paths() -> Vec<PathBuf> {
    ["/usr/bin/git", "/usr/local/bin/git"]
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

/// This platform's table. Every table compiles everywhere, so the tests of
/// one run on all.
fn platform_fallbacks() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        macos_fallback_paths(|path| path.is_file())
    } else if cfg!(windows) {
        windows_fallback_paths(|name| env::var_os(name))
    } else if cfg!(unix) {
        linux_fallback_paths()
    } else {
        Vec::new()
    }
}

/// Git for Windows (machine and per-user installs), Scoop, the 64-bit
/// `ProgramW6432` view, and Chocolatey. GUI apps often start before the
/// installer has updated `PATH`. Joins each `(env var, relative tail)` whose
/// base is absolute; a relative base is ignored.
///
/// `var` is injected so Linux tests can lock the table, including
/// `ProgramW6432` and Chocolatey, without a Windows `PATH` or a real Git.
fn windows_fallback_paths(var: impl Fn(&str) -> Option<OsString>) -> Vec<PathBuf> {
    const TAILS: &[(&str, &str)] = &[
        ("ProgramFiles", r"Git\cmd\git.exe"),
        ("ProgramFiles", r"Git\bin\git.exe"),
        ("ProgramFiles(x86)", r"Git\cmd\git.exe"),
        ("LOCALAPPDATA", r"Programs\Git\cmd\git.exe"),
        ("USERPROFILE", r"scoop\shims\git.exe"),
        ("ProgramW6432", r"Git\cmd\git.exe"),
        ("ProgramData", r"chocolatey\bin\git.exe"),
    ];
    TAILS
        .iter()
        .filter_map(|(key, tail)| {
            let base = PathBuf::from(var(key)?);
            base.is_absolute().then(|| base.join(tail))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn inputs(path_var: Option<OsString>, fallbacks: Vec<PathBuf>) -> DiscoverInputs {
        DiscoverInputs {
            override_path: None,
            path_var,
            exe_name: "git",
            fallbacks,
            skip_paths: Vec::new(),
        }
    }

    fn joined(dirs: &[PathBuf]) -> Option<OsString> {
        Some(env::join_paths(dirs).expect("joinable PATH"))
    }

    fn touch_executable(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir).expect("dir");
        let path = dir.join("git");
        std::fs::write(&path, b"").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        path
    }

    /// An override that is missing or not a file is reported before any
    /// search.
    #[test]
    fn a_bad_override_is_reported_before_searching() {
        let dir = TempDir::with_prefix("cthulhu-override-").expect("temp dir");
        for requested in [Path::new("/no/such/cthulhu-git"), dir.path()] {
            let mut inputs = inputs(None, Vec::new());
            inputs.override_path = Some(requested.into());
            let error = discover_with(&inputs).expect_err("not a git");
            assert!(
                matches!(&error, GitError::BadOverride(path) if path == requested),
                "{requested:?}: {error:?}"
            );
        }
    }

    /// PATH entries and fallbacks, with what is searched and what is found.
    #[test]
    fn path_search_table() {
        let tmp = TempDir::with_prefix("cthulhu-search-").expect("temp dir");
        let root = tmp.path();
        let found = touch_executable(&root.join("found"));
        let spaced = touch_executable(&root.join("Program Files"));
        let fallback = touch_executable(&root.join("fallback"));
        let missing = root.join("missing").join("git");
        let dir = |path: &Path| path.parent().expect("dir").to_path_buf();

        let cases = vec![
            (
                "relative and empty entries would resolve in the repository",
                inputs(
                    joined(&[".".into(), "".into(), "bin".into(), dir(&missing)]),
                    Vec::new(),
                ),
                vec![missing.clone()],
                vec![],
            ),
            (
                "a location is searched once",
                inputs(joined(&[dir(&found), dir(&found)]), vec![found.clone()]),
                vec![found.clone()],
                vec![found.clone()],
            ),
            (
                "a quoted absolute entry is unquoted",
                inputs(
                    Some(format!("\"{}\"", dir(&spaced).display()).into()),
                    Vec::new(),
                ),
                vec![spaced.clone()],
                vec![spaced.clone()],
            ),
            (
                "a quoted relative entry is still relative",
                inputs(Some("\"bin\"".into()), Vec::new()),
                vec![],
                vec![],
            ),
            (
                "a skipped PATH entry is not searched, a fallback still is",
                DiscoverInputs {
                    skip_paths: vec![found.clone()],
                    ..inputs(joined(&[dir(&found)]), vec![fallback.clone()])
                },
                vec![fallback.clone()],
                vec![fallback.clone()],
            ),
            (
                "a fallback may name a skipped path",
                DiscoverInputs {
                    skip_paths: vec![found.clone()],
                    ..inputs(None, vec![found.clone()])
                },
                vec![found.clone()],
                vec![found.clone()],
            ),
        ];
        #[cfg(unix)]
        let cases = {
            use std::os::unix::fs::{PermissionsExt, symlink};

            let mut cases = cases;
            let plain = touch_executable(&root.join("plain"));
            std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644))
                .expect("chmod");
            cases.push((
                "a file without the execute bit is no candidate",
                inputs(joined(&[dir(&plain), dir(&found)]), Vec::new()),
                vec![plain, found.clone()],
                vec![found.clone()],
            ));
            let link = root.join("link");
            std::fs::create_dir(&link).expect("link dir");
            symlink(&found, link.join("git")).expect("symlink");
            cases.push((
                "a symlink to a skipped git is skipped",
                DiscoverInputs {
                    skip_paths: vec![found.clone()],
                    ..inputs(joined(&[link]), Vec::new())
                },
                vec![],
                vec![],
            ));
            cases
        };
        for (case, inputs, searched, existing) in cases {
            let candidates = find_git_in(&inputs);
            assert_eq!(candidates.searched, searched, "{case}");
            assert_eq!(candidates.existing, existing, "{case}");
        }
        let error = discover_with(&inputs(joined(&[dir(&missing)]), Vec::new()))
            .expect_err("nothing to find");
        assert!(
            matches!(&error, GitError::NotFound { tried } if *tried == [missing.clone()]),
            "{error:?}"
        );
    }

    #[test]
    fn fallback_order_per_platform() {
        let paths = |list: &[&str]| list.iter().map(PathBuf::from).collect::<Vec<_>>();
        let only = |wanted: &'static str| move |path: &Path| path == Path::new(wanted);
        let base = |name: &str| {
            let root = if cfg!(windows) { r"C:\" } else { "/" };
            PathBuf::from(root).join(name)
        };
        let windows = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(key, tail)| base(key).join(tail))
                .collect::<Vec<_>>()
        };
        let cases = [
            (
                "linux: /usr/bin first",
                linux_fallback_paths(),
                paths(&["/usr/bin/git", "/usr/local/bin/git"]),
            ),
            (
                "macos: no xcode stub without a developer git",
                macos_fallback_paths(|_| false),
                paths(&[MACOS_HOMEBREW_GIT, MACOS_USR_LOCAL_GIT, MACOS_MACPORTS_GIT]),
            ),
            (
                "macos: homebrew ahead of the command line tools and the stub",
                macos_fallback_paths(only(MACOS_CLT_GIT)),
                paths(&[
                    MACOS_HOMEBREW_GIT,
                    MACOS_USR_LOCAL_GIT,
                    MACOS_MACPORTS_GIT,
                    MACOS_CLT_GIT,
                    MACOS_SHIM_GIT,
                ]),
            ),
            (
                "macos: xcode's git only when that file exists",
                macos_fallback_paths(only(MACOS_XCODE_GIT)),
                paths(&[
                    MACOS_HOMEBREW_GIT,
                    MACOS_USR_LOCAL_GIT,
                    MACOS_MACPORTS_GIT,
                    MACOS_XCODE_GIT,
                    MACOS_SHIM_GIT,
                ]),
            ),
            (
                "windows: every install location, in order",
                windows_fallback_paths(|key| Some(base(key).into_os_string())),
                windows(&[
                    ("ProgramFiles", r"Git\cmd\git.exe"),
                    ("ProgramFiles", r"Git\bin\git.exe"),
                    ("ProgramFiles(x86)", r"Git\cmd\git.exe"),
                    ("LOCALAPPDATA", r"Programs\Git\cmd\git.exe"),
                    ("USERPROFILE", r"scoop\shims\git.exe"),
                    ("ProgramW6432", r"Git\cmd\git.exe"),
                    ("ProgramData", r"chocolatey\bin\git.exe"),
                ]),
            ),
            (
                "windows: unset and relative bases are skipped",
                windows_fallback_paths(|key| match key {
                    "ProgramW6432" | "ProgramData" => Some(base(key).into_os_string()),
                    "ProgramFiles" => Some(OsString::from("not-absolute")),
                    _ => None,
                }),
                windows(&[
                    ("ProgramW6432", r"Git\cmd\git.exe"),
                    ("ProgramData", r"chocolatey\bin\git.exe"),
                ]),
            ),
        ];
        for (case, got, expected) in cases {
            assert_eq!(got, expected, "{case}");
        }
    }

    /// `/usr/bin/git` is skipped only on macOS, and each platform searches
    /// its own fallback table.
    #[test]
    fn from_env_uses_this_platforms_tables() {
        let inputs = DiscoverInputs::from_env();
        let expected = if cfg!(target_os = "macos") {
            (
                vec![PathBuf::from(MACOS_SHIM_GIT)],
                macos_fallback_paths(|path| path.is_file()),
            )
        } else if cfg!(windows) {
            (Vec::new(), windows_fallback_paths(|name| env::var_os(name)))
        } else {
            (Vec::new(), linux_fallback_paths())
        };
        assert_eq!((inputs.skip_paths, inputs.fallbacks), expected);
    }

    #[cfg(windows)]
    #[test]
    fn quoted_path_element_and_git_cmd_lose_to_git_exe() {
        use std::fs;

        let tmp = TempDir::with_prefix("cthulhu-win-git-").expect("temp dir");
        let dir = tmp.path().join("Program Files").join("Git").join("cmd");
        fs::create_dir_all(&dir).expect("dir");
        fs::write(dir.join("git.cmd"), b"@echo off\r\n").expect("git.cmd");
        fs::write(dir.join("git.exe"), b"").expect("git.exe");
        let quoted = format!("\"{}\"", dir.display());
        assert!(!Path::new(&quoted).is_absolute());

        let inputs = DiscoverInputs {
            override_path: None,
            path_var: Some(OsString::from(quoted)),
            exe_name: "git.exe",
            fallbacks: Vec::new(),
            skip_paths: Vec::new(),
        };
        let found = find_git_in(&inputs);
        assert_eq!(found.existing, vec![dir.join("git.exe")]);
        assert!(
            found.searched.iter().all(|path| path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))),
            "git.cmd was searched: {:?}",
            found.searched
        );
    }
}
