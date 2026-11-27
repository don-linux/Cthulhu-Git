//! The optional GitHub token: read-only, kept in the system keychain, never
//! in `settings.json`, logs or the accessibility tree.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use zeroize::{Zeroize, Zeroizing};

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

fn store_error(error: &keyring::Error) -> TokenStoreError {
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

fn lock<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The token's entry in a credential store, reached through one worker thread.
///
/// Every call runs on that thread in the order it was made, so a save that
/// wakes up late cannot land after a newer delete. A caller waits at most
/// `timeout`, then the call is cancelled: a save not written yet is skipped,
/// and one written late puts back the value it replaced, because its caller
/// was told the token is not saved.
pub struct Keychain {
    jobs: Result<Sender<Job>, TokenStoreError>,
    timeout: Duration,
}

impl Keychain {
    /// Starts the worker. `open` builds the entry on it, because opening a
    /// native store can block (the Secret Service answers over D-Bus).
    pub fn new(
        open: impl FnOnce() -> Result<keyring::Entry, TokenStoreError> + Send + 'static,
        timeout: Duration,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        let jobs = thread::Builder::new()
            .name("keychain".to_owned())
            .spawn(move || work(open(), &receiver))
            .map(|_| sender)
            .map_err(|error| {
                TokenStoreError::Unavailable(format!(
                    "could not start the keychain worker: {error}"
                ))
            });
        Self { jobs, timeout }
    }

    /// Blocks; call it off the UI thread. `None` when nothing is stored. A
    /// stored value that is not a token is an error, not "nothing stored", so
    /// the settings keep pointing at the keychain.
    pub fn load(&self) -> Result<Option<Token>, TokenStoreError> {
        self.call(Job::Load)
    }

    pub fn save(&self, token: &Token) -> Result<(), TokenStoreError> {
        let token = token.clone();
        self.call(|ticket| Job::Save(token, ticket)).map(|_| ())
    }

    /// Removing a token that is not there succeeds.
    pub fn delete(&self) -> Result<(), TokenStoreError> {
        self.call(Job::Delete).map(|_| ())
    }

    fn call(&self, job: impl FnOnce(Ticket) -> Job) -> Result<Option<Token>, TokenStoreError> {
        let jobs = self.jobs.as_ref().map_err(Clone::clone)?;
        let state = Arc::new(Mutex::new(State::Waiting));
        let (reply, answer) = mpsc::channel();
        jobs.send(job(Ticket {
            state: Arc::clone(&state),
            reply,
        }))
        .map_err(|_| worker_stopped())?;
        match answer.recv_timeout(self.timeout) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                let mut state = lock(&state);
                if *state == State::Answered {
                    // Answered as the deadline passed; the reply is in the channel.
                    drop(state);
                    return answer.try_recv().map_err(|_| worker_stopped())?;
                }
                *state = State::Cancelled;
                Err(TokenStoreError::Locked(
                    "the keychain did not answer in time".to_owned(),
                ))
            }
            Err(RecvTimeoutError::Disconnected) => Err(worker_stopped()),
        }
    }
}

fn worker_stopped() -> TokenStoreError {
    TokenStoreError::Unavailable("the keychain worker stopped".to_owned())
}

type Reply = Result<Option<Token>, TokenStoreError>;

enum Job {
    Load(Ticket),
    Save(Token, Ticket),
    Delete(Ticket),
}

/// What the caller and the worker share for one call.
struct Ticket {
    state: Arc<Mutex<State>>,
    reply: Sender<Reply>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Waiting,
    Answered,
    Cancelled,
}

impl Ticket {
    fn cancelled(&self) -> bool {
        *lock(&self.state) == State::Cancelled
    }

    /// Sends `result` unless the caller gave up. `false` when it did.
    fn answer(&self, result: Reply) -> bool {
        let mut state = lock(&self.state);
        if *state == State::Cancelled {
            return false;
        }
        *state = State::Answered;
        let _ = self.reply.send(result);
        true
    }
}

fn work(entry: Result<keyring::Entry, TokenStoreError>, jobs: &Receiver<Job>) {
    for job in jobs {
        let entry = match &entry {
            Ok(entry) => entry,
            Err(error) => {
                let (Job::Load(ticket) | Job::Save(_, ticket) | Job::Delete(ticket)) = job;
                ticket.answer(Err(error.clone()));
                continue;
            }
        };
        match job {
            Job::Load(ticket) => {
                ticket.answer(read(entry).and_then(|stored| {
                    stored
                        .map(|value| {
                            Token::parse(&value).ok_or_else(|| {
                                TokenStoreError::Failed("the saved value is not a token".to_owned())
                            })
                        })
                        .transpose()
                }));
            }
            Job::Save(token, ticket) => save(entry, &token, &ticket),
            Job::Delete(ticket) => {
                ticket.answer(delete(entry).map(|()| None));
            }
        }
    }
}

fn save(entry: &keyring::Entry, token: &Token, ticket: &Ticket) {
    let previous = match read(entry) {
        Ok(previous) => previous,
        Err(error) => {
            ticket.answer(Err(error));
            return;
        }
    };
    // The deadline passed before anything was written: leave the entry as it is.
    if ticket.cancelled() {
        return;
    }
    let written = entry
        .set_password(token.expose())
        .map_err(|error| store_error(&error));
    let landed = written.is_ok();
    if !ticket.answer(written.map(|()| None)) && landed {
        undo_save(entry, previous.as_deref().map(String::as_str));
    }
}

/// Puts back what a cancelled save replaced. When that fails the new token is
/// deleted rather than kept, since its caller was told it is not saved.
fn undo_save(entry: &keyring::Entry, previous: Option<&str>) {
    let restored = match previous {
        Some(old) => entry.set_password(old).is_ok(),
        None => delete(entry).is_ok(),
    };
    if !restored {
        let _ = entry.delete_credential();
    }
}

fn read(entry: &keyring::Entry) -> Result<Option<Zeroizing<String>>, TokenStoreError> {
    match entry.get_password() {
        Ok(value) => Ok(Some(Zeroizing::new(value))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(store_error(&error)),
    }
}

fn delete(entry: &keyring::Entry) -> Result<(), TokenStoreError> {
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(store_error(&error)),
    }
}

/// The system keychain, opened the first time a token is loaded, saved or
/// removed.
fn system_keychain() -> &'static Keychain {
    static SYSTEM: OnceLock<Keychain> = OnceLock::new();
    SYSTEM.get_or_init(|| {
        // keyring opens the platform's store on the first entry: macOS
        // Keychain, Windows Credential Manager or the Secret Service.
        let open = || {
            keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(|error| store_error(&error))
        };
        Keychain::new(open, KEYCHAIN_TIMEOUT)
    })
}

/// [`Keychain::load`] on the system keychain.
pub fn load_token() -> Result<Option<Token>, TokenStoreError> {
    system_keychain().load()
}

/// [`Keychain::save`] on the system keychain.
pub fn save_token(token: &Token) -> Result<(), TokenStoreError> {
    system_keychain().save(token)
}

/// [`Keychain::delete`] on the system keychain.
pub fn delete_token() -> Result<(), TokenStoreError> {
    system_keychain().delete()
}

/// `GH_TOKEN`, then `GITHUB_TOKEN`, the names GitHub CLI reads.
pub fn token_from_env() -> Option<(&'static str, Token)> {
    ["GH_TOKEN", "GITHUB_TOKEN"].into_iter().find_map(|name| {
        let Ok(raw) = std::env::var(name) else {
            return None;
        };
        // `Zeroizing` clears the environment value on both the parse failure
        // and the success path. `Token` keeps its own copy.
        let value = Zeroizing::new(raw);
        Token::parse(&value).map(|token| (name, token))
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
    // `Zeroizing` clears the buffer if the caller times out and drops the
    // receiver, and if `send` fails because that receiver is already gone.
    // Joining the reader would wait on a grandchild that still holds the pipe.
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Zeroizing::new(Vec::new());
        let _ = (&mut stdout).take(64 * 1024).read_to_end(&mut bytes);
        let _ = sender.send(bytes);
    });
    let started = Instant::now();
    let success = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                drop(receiver);
                return Err("GitHub CLI did not answer in time.".to_owned());
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => return Err(format!("GitHub CLI failed: {error}")),
        }
    };
    let mut stdout = receiver
        .recv()
        .unwrap_or_else(|_| Zeroizing::new(Vec::new()));
    // Move the bytes out. `stdout`'s drop still wipes the wrapper; `gh_cli_token`
    // wipes this copy after parsing.
    Ok((success, std::mem::take(&mut *stdout)))
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

    /// An interior newline is refused: the token becomes a header value.
    #[test]
    fn parse_trims_and_refuses_what_cannot_be_a_token() {
        let shortest = "a".repeat(20);
        let longest = "a".repeat(255);
        for (input, expected) in [
            (
                "  github_pat_11ABCDEFG0123456789_abcdef \n".to_owned(),
                Some("github_pat_11ABCDEFG0123456789_abcdef"),
            ),
            (format!(" \t{shortest}\n"), Some(shortest.as_str())),
            (longest.clone(), Some(longest.as_str())),
            ("a".repeat(19), None),
            ("a".repeat(256), None),
            (String::new(), None),
            ("short".to_owned(), None),
            ("has space inside the token value".to_owned(), None),
            ("ghp_line\nbreak_inside_the_value".to_owned(), None),
            (format!("ghp_{}", "\u{e9}".repeat(10)), None),
        ] {
            let parsed = Token::parse(&input);
            assert_eq!(parsed.as_ref().map(Token::expose), expected, "{input:?}");
        }
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

    fn parsed_token(value: &str) -> Token {
        Token::parse(value).expect("plausible token")
    }

    /// The token's entry in a fresh in-memory store. Entries built from one
    /// mock store share the credential, so a test can reach the worker's.
    fn mock_entry() -> (keyring::Entry, keyring_core::Entry) {
        let store: Arc<keyring_core::CredentialStore> =
            keyring_core::mock::Store::new().expect("mock store");
        let entry = || keyring_core::Entry::new_in_store(&store, KEYRING_SERVICE, KEYRING_USER);
        let worker = keyring::Entry {
            inner: entry().expect("entry"),
        };
        (worker, entry().expect("entry"))
    }

    fn mock_keychain() -> (Keychain, keyring_core::Entry) {
        let (worker, test) = mock_entry();
        (
            Keychain::new(move || Ok(worker), Duration::from_secs(5)),
            test,
        )
    }

    #[test]
    fn keychain_saves_loads_and_deletes_in_its_store() {
        let (keychain, shared) = mock_keychain();
        assert!(keychain.load().expect("empty store").is_none());
        let token = parsed_token("ghp_abcdefghijklmnopqrstuvwxyz");
        keychain.save(&token).expect("save");
        let stored = keychain.load().expect("load").expect("stored");
        assert_eq!(stored.expose(), token.expose());
        keychain.delete().expect("delete");
        assert!(keychain.load().expect("load after delete").is_none());
        keychain.delete().expect("deleting nothing succeeds");

        shared
            .set_password("not a token")
            .expect("plant a bad value");
        assert!(matches!(keychain.load(), Err(TokenStoreError::Failed(_))));
    }

    #[test]
    fn store_errors_say_whether_the_keychain_is_missing_locked_or_refusing() {
        let (keychain, shared) = mock_keychain();
        let cred: &keyring_core::mock::Cred = shared.as_any().downcast_ref().expect("mock cred");
        let failure = || -> keyring_core::error::PlatformError { "denied".into() };
        for (error, expected) in [
            (keyring_core::Error::NoStorageAccess(failure()), "Locked"),
            (keyring_core::Error::PlatformFailure(failure()), "Locked"),
            (
                keyring_core::Error::NotSupportedByStore("x".to_owned()),
                "Unavailable",
            ),
            (
                keyring_core::Error::Invalid("a".to_owned(), "b".to_owned()),
                "Failed",
            ),
        ] {
            let case = format!("{error:?}");
            cred.set_error(error);
            let got = match keychain.load() {
                Err(TokenStoreError::Locked(_)) => "Locked",
                Err(TokenStoreError::Unavailable(_)) => "Unavailable",
                Err(TokenStoreError::Failed(_)) => "Failed",
                Ok(_) => "Ok",
            };
            assert_eq!(got, expected, "{case}");
        }
    }

    /// Blocks writes while closed, and reads too with `holds_reads`, and
    /// records every write, so a test can hold a save past its deadline at
    /// either step and then see what landed. With `refuse_rewrite`, every
    /// write after the first fails.
    #[derive(Default)]
    struct Gate {
        closed: Mutex<bool>,
        opened: std::sync::Condvar,
        writes: Mutex<Vec<String>>,
        holds_reads: bool,
        refuse_rewrite: bool,
    }

    impl Gate {
        fn set_closed(&self, closed: bool) {
            *lock(&self.closed) = closed;
            self.opened.notify_all();
        }

        fn pass(&self) {
            let mut closed = lock(&self.closed);
            while *closed {
                closed = self
                    .opened
                    .wait(closed)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        }
    }

    struct GatedCred {
        inner: keyring_core::Entry,
        gate: Arc<Gate>,
    }

    impl keyring_core::api::CredentialApi for GatedCred {
        fn set_secret(&self, secret: &[u8]) -> keyring_core::Result<()> {
            self.gate.pass();
            let mut writes = lock(&self.gate.writes);
            if self.gate.refuse_rewrite && !writes.is_empty() {
                return Err(keyring_core::Error::PlatformFailure("refused".into()));
            }
            writes.push(String::from_utf8_lossy(secret).into_owned());
            drop(writes);
            self.inner.set_secret(secret)
        }

        fn get_secret(&self) -> keyring_core::Result<Vec<u8>> {
            if self.gate.holds_reads {
                self.gate.pass();
            }
            self.inner.get_secret()
        }

        fn delete_credential(&self) -> keyring_core::Result<()> {
            lock(&self.gate.writes).push("delete".to_owned());
            self.inner.delete_credential()
        }

        fn get_credential(&self) -> keyring_core::Result<Option<Arc<keyring_core::Credential>>> {
            Ok(None)
        }

        fn get_specifiers(&self) -> Option<(String, String)> {
            self.inner.get_specifiers()
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    /// A keychain whose saves block while the gate is closed, holding `old`.
    fn gated_keychain(
        old: &Token,
        holds_reads: bool,
        refuse_rewrite: bool,
    ) -> (Keychain, Arc<Gate>) {
        let gate = Arc::new(Gate {
            holds_reads,
            refuse_rewrite,
            ..Gate::default()
        });
        let (_, inner) = mock_entry();
        inner.set_password(old.expose()).expect("seed");
        let cred = GatedCred {
            inner,
            gate: Arc::clone(&gate),
        };
        let worker = keyring::Entry {
            inner: keyring_core::Entry::new_with_credential(Arc::new(cred)),
        };
        let keychain = Keychain::new(move || Ok(worker), Duration::from_millis(500));
        (keychain, gate)
    }

    /// Its caller was told the token is not saved, so the new token never
    /// stays: a save still reading is never written, a late write is replaced
    /// by the old value, or the entry is emptied when that is refused.
    #[test]
    fn a_save_past_its_deadline_never_stays() {
        let old = parsed_token("ghp_oldtokenvalue_abcdefghij");
        let new = parsed_token("ghp_newtokenvalue_abcdefghij");
        for (holds_reads, refuse_rewrite, kept, writes) in [
            (true, false, Some(old.expose()), vec![]),
            (
                false,
                false,
                Some(old.expose()),
                vec![new.expose(), old.expose()],
            ),
            (false, true, None, vec![new.expose(), "delete"]),
        ] {
            let case = (holds_reads, refuse_rewrite);
            let (keychain, gate) = gated_keychain(&old, holds_reads, refuse_rewrite);
            gate.set_closed(true);
            assert!(
                matches!(keychain.save(&new), Err(TokenStoreError::Locked(_))),
                "a save held past its deadline must report the keychain as not answering"
            );
            gate.set_closed(false);
            // Calls run in order, so this load waits for the late write and its undo.
            let stored = keychain.load().expect("load");
            assert_eq!(stored.as_ref().map(Token::expose), kept, "{case:?}");
            assert_eq!(*lock(&gate.writes), writes, "{case:?}");
        }
    }

    #[test]
    fn a_late_save_does_not_come_back_after_a_newer_delete() {
        let old = parsed_token("ghp_oldtokenvalue_abcdefghij");
        let new = parsed_token("ghp_newtokenvalue_abcdefghij");
        let (keychain, gate) = gated_keychain(&old, false, false);
        gate.set_closed(true);
        assert!(matches!(
            keychain.save(&new),
            Err(TokenStoreError::Locked(_))
        ));
        // Queued behind the held save before the gate opens.
        assert!(matches!(keychain.delete(), Err(TokenStoreError::Locked(_))));
        gate.set_closed(false);
        assert!(
            keychain.load().expect("load").is_none(),
            "the delete must run after the late save, not before it"
        );
        assert_eq!(
            lock(&gate.writes).last().map(String::as_str),
            Some("delete")
        );
    }
}
