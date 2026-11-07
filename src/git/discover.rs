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
    #[cfg(target_os = "macos")]
    {
        vec![PathBuf::from(MACOS_SHIM_GIT)]
    }
    #[cfg(not(target_os = "macos"))]
    {
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
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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

#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
fn linux_fallback_paths() -> Vec<PathBuf> {
    ["/usr/bin/git", "/usr/local/bin/git"]
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

#[cfg(target_os = "macos")]
fn platform_fallbacks() -> Vec<PathBuf> {
    macos_fallback_paths(|path| path.is_file())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_fallbacks() -> Vec<PathBuf> {
    linux_fallback_paths()
}

/// Git for Windows (machine and per-user installs), Scoop, the 64-bit
/// `ProgramW6432` view, and Chocolatey. GUI apps often start before the
/// installer has updated `PATH`. A relative base is ignored.
#[cfg(windows)]
fn platform_fallbacks() -> Vec<PathBuf> {
    windows_fallback_paths(|name| env::var_os(name))
}

/// Joins each `(env var, relative tail)` whose base is absolute.
///
/// `var` is injected so Linux tests can lock the table, including
/// `ProgramW6432` and Chocolatey, without a Windows `PATH` or a real Git.
#[cfg(any(windows, test))]
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
            skip_paths: Vec::new(),
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
        // `/usr/bin` is not absolute on Windows, so the one absolute entry has
        // to be a path this platform actually treats as absolute.
        let absolute = if cfg!(windows) {
            PathBuf::from(r"C:\Program Files\Git\cmd")
        } else {
            PathBuf::from("/usr/bin")
        };
        let inputs = DiscoverInputs {
            override_path: None,
            path_var: Some(
                env::join_paths([
                    Path::new("."),
                    Path::new(""),
                    Path::new("bin"),
                    absolute.as_path(),
                ])
                .expect("PATH"),
            ),
            exe_name: "git",
            fallbacks: Vec::new(),
            skip_paths: Vec::new(),
        };
        let searched = find_git_in(&inputs).searched;
        assert_eq!(searched, vec![absolute.join("git")]);
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

    #[test]
    fn linux_fallbacks_are_usr_then_usr_local() {
        assert_eq!(
            linux_fallback_paths(),
            vec![
                PathBuf::from("/usr/bin/git"),
                PathBuf::from("/usr/local/bin/git")
            ]
        );
    }

    #[test]
    fn macos_fallbacks_skip_the_xcode_stub_without_a_developer_git() {
        let paths = macos_fallback_paths(|_| false);
        assert_eq!(
            paths,
            vec![
                PathBuf::from(MACOS_HOMEBREW_GIT),
                PathBuf::from(MACOS_USR_LOCAL_GIT),
                PathBuf::from(MACOS_MACPORTS_GIT),
            ]
        );
        assert!(!paths.iter().any(|path| path == Path::new(MACOS_SHIM_GIT)));
    }

    #[test]
    fn macos_fallbacks_keep_homebrew_ahead_of_the_stub_when_clt_exists() {
        let clt = Path::new(MACOS_CLT_GIT);
        let paths = macos_fallback_paths(|path| path == clt);
        assert_eq!(
            paths,
            vec![
                PathBuf::from(MACOS_HOMEBREW_GIT),
                PathBuf::from(MACOS_USR_LOCAL_GIT),
                PathBuf::from(MACOS_MACPORTS_GIT),
                PathBuf::from(MACOS_CLT_GIT),
                PathBuf::from(MACOS_SHIM_GIT),
            ]
        );
    }

    #[test]
    fn macos_fallbacks_include_xcode_git_only_when_that_file_exists() {
        let xcode = Path::new(MACOS_XCODE_GIT);
        let paths = macos_fallback_paths(|path| path == xcode);
        assert_eq!(
            paths,
            vec![
                PathBuf::from(MACOS_HOMEBREW_GIT),
                PathBuf::from(MACOS_USR_LOCAL_GIT),
                PathBuf::from(MACOS_MACPORTS_GIT),
                PathBuf::from(MACOS_XCODE_GIT),
                PathBuf::from(MACOS_SHIM_GIT),
            ]
        );
    }

    #[test]
    fn from_env_skips_usr_bin_git_only_on_macos() {
        let skips = DiscoverInputs::from_env().skip_paths;
        if cfg!(target_os = "macos") {
            assert_eq!(skips, vec![PathBuf::from(MACOS_SHIM_GIT)]);
        } else {
            assert!(skips.is_empty(), "{skips:?}");
        }
    }

    fn touch_executable(dir: &Path, name: &str) -> PathBuf {
        use std::fs;
        fs::create_dir_all(dir).expect("dir");
        let path = dir.join(name);
        fs::write(&path, b"").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        path
    }

    #[test]
    fn skip_paths_drop_a_path_candidate_and_keep_fallbacks() {
        let tmp = TempDir::with_prefix("cthulhu-skip-").expect("temp dir");
        let shim_dir = tmp.path().join("usr").join("bin");
        let shim = touch_executable(&shim_dir, "git");
        let fallback_dir = tmp.path().join("opt").join("homebrew").join("bin");
        let fallback = touch_executable(&fallback_dir, "git");

        let mut skipped = inputs(&[&shim_dir], vec![fallback.clone()]);
        skipped.skip_paths = vec![shim.clone()];
        let found = find_git_in(&skipped);
        assert_eq!(found.existing, vec![fallback.clone()]);
        assert!(
            !found.searched.iter().any(|path| path == &shim),
            "skipped PATH entry was still searched: {:?}",
            found.searched
        );

        let kept = DiscoverInputs {
            override_path: None,
            path_var: None,
            exe_name: "git",
            fallbacks: vec![shim.clone()],
            skip_paths: vec![shim.clone()],
        };
        assert_eq!(
            find_git_in(&kept).existing,
            vec![shim],
            "fallbacks must still be able to name the shim"
        );
    }

    #[cfg(unix)]
    #[test]
    fn skip_paths_match_a_symlink_to_the_canonical_file() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::with_prefix("cthulhu-skip-link-").expect("temp dir");
        let real_dir = tmp.path().join("real");
        let real = touch_executable(&real_dir, "git");
        let link_dir = tmp.path().join("link");
        std::fs::create_dir(&link_dir).expect("link dir");
        let link = link_dir.join("git");
        symlink(&real, &link).expect("symlink");

        let mut inputs = inputs(&[&link_dir], Vec::new());
        inputs.skip_paths = vec![real];
        let found = find_git_in(&inputs);
        assert!(
            found.existing.is_empty(),
            "symlink to a skipped git was a candidate: {:?}",
            found.existing
        );
        assert!(found.searched.is_empty(), "{:?}", found.searched);
    }

    #[test]
    fn quoted_absolute_path_entry_is_unquoted_and_searched() {
        let tmp = TempDir::with_prefix("cthulhu-quoted-").expect("temp dir");
        let dir = tmp.path().join("Program Files");
        let git = touch_executable(&dir, "git");
        let quoted = format!("\"{}\"", dir.display());
        assert!(!Path::new(&quoted).is_absolute());

        let inputs = DiscoverInputs {
            override_path: None,
            path_var: Some(OsString::from(quoted)),
            exe_name: "git",
            fallbacks: Vec::new(),
            skip_paths: Vec::new(),
        };
        assert_eq!(find_git_in(&inputs).existing, vec![git]);
    }

    #[test]
    fn quoted_relative_path_entry_is_not_searched() {
        let inputs = DiscoverInputs {
            override_path: None,
            path_var: Some(OsString::from("\"bin\"")),
            exe_name: "git",
            fallbacks: Vec::new(),
            skip_paths: Vec::new(),
        };
        assert!(find_git_in(&inputs).searched.is_empty());
    }

    #[test]
    fn windows_fallbacks_add_program_w6432_and_chocolatey_for_absolute_bases() {
        let base = |name: &str| {
            if cfg!(windows) {
                PathBuf::from(r"C:\").join(name)
            } else {
                PathBuf::from("/").join(name)
            }
        };
        let program_w6432 = base("ProgramW6432");
        let program_data = base("ProgramData");
        let program_files = base("ProgramFiles");
        let paths = windows_fallback_paths(|key| match key {
            "ProgramW6432" => Some(program_w6432.clone().into()),
            "ProgramData" => Some(program_data.clone().into()),
            "ProgramFiles" => Some(OsString::from("not-absolute")),
            _ => None,
        });
        assert_eq!(
            paths,
            vec![
                program_w6432.join(r"Git\cmd\git.exe"),
                program_data.join(r"chocolatey\bin\git.exe"),
            ]
        );

        let full = windows_fallback_paths(|key| Some(base(key).into_os_string()));
        assert_eq!(
            full,
            [
                ("ProgramFiles", r"Git\cmd\git.exe"),
                ("ProgramFiles", r"Git\bin\git.exe"),
                ("ProgramFiles(x86)", r"Git\cmd\git.exe"),
                ("LOCALAPPDATA", r"Programs\Git\cmd\git.exe"),
                ("USERPROFILE", r"scoop\shims\git.exe"),
                ("ProgramW6432", r"Git\cmd\git.exe"),
                ("ProgramData", r"chocolatey\bin\git.exe"),
            ]
            .into_iter()
            .map(|(key, tail)| base(key).join(tail))
            .collect::<Vec<_>>()
        );
        assert!(
            full.iter().any(|path| path.starts_with(&program_files)),
            "ProgramFiles must stay in the table: {full:?}"
        );
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
