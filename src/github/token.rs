//! The optional GitHub token: read-only, kept in the system keychain, never
//! in `settings.json`, logs or the accessibility tree.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use zeroize::Zeroize;

const KEYRING_SERVICE: &str = "io.github.don-linux.cthulhu-git";
const KEYRING_USER: &str = "github.com";
const GH_TIMEOUT: Duration = Duration::from_secs(5);
const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Fine-grained token form with Contents and Metadata read-only already
/// chosen (GitHub's documented template parameters).
pub const TOKEN_TEMPLATE_URL: &str = "https://github.com/settings/personal-access-tokens/new?name=Cthulhu+Git&description=Read-only+access+for+Cthulhu+Git&expires_in=90&contents=read";

/// A GitHub token. `Debug` does not print it.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Trims pasted text. Refuses anything that cannot be a token: too short
    /// or long, or with spaces, control or non-ASCII characters inside.
    pub fn parse(input: &str) -> Option<Self> {
        let value = input.trim();
        let plausible = (20..=255).contains(&value.len())
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
        plausible.then(|| Self(value.to_owned()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(…)")
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenStoreError {
    /// No keychain or Secret Service to talk to. The token can still be used
    /// for this session.
    Unavailable(String),
    /// The keychain exists but did not answer: it is locked, or the call
    /// passed the deadline. A token already stored is left in place.
    Locked(String),
    Failed(String),
}

impl fmt::Display for TokenStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(detail) => write!(f, "No system keychain is available ({detail})."),
            Self::Locked(detail) => write!(f, "The system keychain did not answer ({detail})."),
            Self::Failed(detail) => {
                write!(f, "The system keychain refused the request ({detail}).")
            }
        }
    }
}

fn store_error(error: keyring::Error) -> TokenStoreError {
    match error {
        keyring::Error::NoDefaultStore | keyring::Error::NotSupportedByStore(_) => {
            TokenStoreError::Unavailable(error.to_string())
        }
        keyring::Error::NoStorageAccess(_) | keyring::Error::PlatformFailure(_) => {
            TokenStoreError::Locked(error.to_string())
        }
        other => TokenStoreError::Failed(other.to_string()),
    }
}

fn entry() -> Result<keyring::Entry, TokenStoreError> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(store_error)
}

/// Blocks on the keychain; call it off the UI thread. `None` when nothing is stored.
/// A stored value that is not a token is an error, not "nothing stored", so
/// the settings keep pointing at the keychain.
pub fn load_token() -> Result<Option<Token>, TokenStoreError> {
    wait_for(
        || match entry()?.get_password() {
            Ok(value) => Token::parse(&value).map(Some).ok_or_else(|| {
                TokenStoreError::Failed("the saved value is not a token".to_owned())
            }),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(store_error(error)),
        },
        KEYCHAIN_TIMEOUT,
    )?
}

pub fn save_token(token: &Token) -> Result<(), TokenStoreError> {
    let token = token.clone();
    wait_for(
        move || entry()?.set_password(token.expose()).map_err(store_error),
        KEYCHAIN_TIMEOUT,
    )?
}

/// Removing a token that is not there succeeds.
pub fn delete_token() -> Result<(), TokenStoreError> {
    wait_for(
        || match entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(store_error(error)),
        },
        KEYCHAIN_TIMEOUT,
    )?
}

/// Runs `work` on a side thread. The keychain calls do not accept a deadline,
/// so a hung prompt must not pin the worker that the UI is waiting on.
fn wait_for<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    timeout: Duration,
) -> Result<T, TokenStoreError> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(work());
    });
    receiver
        .recv_timeout(timeout)
        .map_err(|_| TokenStoreError::Locked("the keychain did not answer in time".to_owned()))
}

/// `GH_TOKEN`, then `GITHUB_TOKEN`, the names GitHub CLI reads.
pub fn token_from_env() -> Option<(&'static str, Token)> {
    ["GH_TOKEN", "GITHUB_TOKEN"].into_iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .and_then(|value| Token::parse(&value))
            .map(|token| (name, token))
    })
}

/// Asks GitHub CLI for its github.com token (`gh auth token`). Only absolute
/// `PATH` entries and the usual install folders are searched.
pub fn gh_cli_token() -> Result<Token, String> {
    let path_var = std::env::var_os("PATH");
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let gh = find_gh(path_var.as_deref(), home.as_deref())
        .ok_or_else(|| "GitHub CLI (gh) was not found.".to_owned())?;
    let output = run_with_timeout(
        Command::new(&gh)
            .args(["auth", "token", "--hostname", "github.com"])
            .env("GH_PROMPT_DISABLED", "1")
            .env("NO_COLOR", "1"),
        GH_TIMEOUT,
    )?;
    let mut bytes = output.1;
    let signed_in = output.0;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    let token = Token::parse(&text);
    text.zeroize();
    bytes.zeroize();
    if !signed_in {
        return Err("GitHub CLI is not signed in to github.com (run `gh auth login`).".to_owned());
    }
    token.ok_or_else(|| "GitHub CLI did not print a token.".to_owned())
}

fn gh_name() -> &'static str {
    if cfg!(windows) { "gh.exe" } else { "gh" }
}

fn gh_fallbacks(home: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = if cfg!(windows) {
        std::env::var_os("ProgramFiles")
            .map(|dir| vec![PathBuf::from(dir).join("GitHub CLI").join("gh.exe")])
            .unwrap_or_default()
    } else {
        ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh"]
            .into_iter()
            .map(PathBuf::from)
            .collect()
    };
    if let Some(home) = home {
        paths.push(home.join(".local").join("bin").join(gh_name()));
    }
    paths
}

/// Relative `PATH` entries resolve against the current directory, so a
/// repository could plant its own `gh`; they are skipped. A file without the
/// execute bit is skipped too, so an earlier non-executable does not hide
/// `/usr/bin/gh`.
pub(crate) fn find_gh(path_var: Option<&std::ffi::OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    path_var
        .into_iter()
        .flat_map(std::env::split_paths)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(gh_name()))
        .chain(gh_fallbacks(home))
        .find(|path| is_executable_file(path))
}

fn is_executable_file(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// `(success, stdout)`. The child is killed when it runs past `timeout`.
fn run_with_timeout(command: &mut Command, timeout: Duration) -> Result<(bool, Vec<u8>), String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start GitHub CLI: {error}"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "GitHub CLI output was not captured.".to_owned())?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = (&mut stdout).take(64 * 1024).read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    let success = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("GitHub CLI did not answer in time.".to_owned());
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => return Err(format!("GitHub CLI failed: {error}")),
        }
    };
    Ok((success, reader.join().unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_the_token() {
        let token = Token::new("ghp_supersecretvalue1234567890");
        assert_eq!(format!("{token:?}"), "Token(…)");
        assert!(!format!("{:?}", Some(token)).contains("supersecret"));
    }

    #[test]
    fn parse_trims_and_refuses_what_cannot_be_a_token() {
        assert_eq!(
            Token::parse("  github_pat_11ABCDEFG0123456789_abcdef \n")
                .map(|token| token.expose().to_owned())
                .as_deref(),
            Some("github_pat_11ABCDEFG0123456789_abcdef")
        );
        for refused in [
            "",
            "short",
            "has space inside the token value",
            "ghp_\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}",
            "ghp_line\nbreak_inside_the_value",
        ] {
            assert!(Token::parse(refused).is_none(), "{refused:?}");
        }
        assert!(Token::parse(&"a".repeat(256)).is_none());
    }

    #[test]
    fn the_template_link_asks_only_for_read_access() {
        assert!(
            TOKEN_TEMPLATE_URL
                .starts_with("https://github.com/settings/personal-access-tokens/new?")
        );
        assert!(TOKEN_TEMPLATE_URL.contains("contents=read"));
        assert!(!TOKEN_TEMPLATE_URL.contains("write"));
        assert!(!TOKEN_TEMPLATE_URL.contains("admin"));
    }

    #[cfg(unix)]
    #[test]
    fn gh_is_found_only_on_absolute_path_entries() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().expect("temp dir");
        let gh = dir.path().join("gh");
        std::fs::write(&gh, "#!/bin/sh\n").expect("write");
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let path = std::env::join_paths([PathBuf::from("relative/bin"), dir.path().to_path_buf()])
            .expect("join");
        assert_eq!(find_gh(Some(&path), None), Some(gh.clone()));

        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let later = dir.path().join("later");
        std::fs::create_dir(&later).expect("dir");
        let next = later.join("gh");
        std::fs::write(&next, "#!/bin/sh\n").expect("write");
        std::fs::set_permissions(&next, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let path = std::env::join_paths([
            PathBuf::from("relative/bin"),
            dir.path().to_path_buf(),
            later,
        ])
        .expect("join");
        assert_eq!(find_gh(Some(&path), None), Some(next));

        let home = tempfile::TempDir::new().expect("home");
        let local = home.path().join(".local").join("bin").join("gh");
        std::fs::create_dir_all(local.parent().expect("parent")).expect("dir");
        std::fs::write(&local, "#!/bin/sh\n").expect("write");
        std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert!(
            gh_fallbacks(Some(home.path()))
                .iter()
                .any(|path| path == &local),
            "the user-local gh install must be on the fallback list"
        );
    }

    #[test]
    fn a_hung_keychain_call_returns_before_it_finishes() {
        let started = Instant::now();
        let result = wait_for(
            || {
                thread::sleep(Duration::from_secs(5));
                1
            },
            Duration::from_millis(100),
        );
        assert!(matches!(result, Err(TokenStoreError::Locked(_))));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn a_hung_child_is_killed_after_the_timeout() {
        let started = Instant::now();
        let result = run_with_timeout(
            Command::new("/bin/sh").args(["-c", "sleep 5"]),
            Duration::from_millis(100),
        );
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(3));

        let (ok, out) = run_with_timeout(
            Command::new("/bin/sh").args(["-c", "printf ghp_abcdefghijklmnopqrstuvwxyz"]),
            Duration::from_secs(5),
        )
        .expect("ran");
        assert!(ok);
        assert_eq!(out, b"ghp_abcdefghijklmnopqrstuvwxyz");
    }
}
