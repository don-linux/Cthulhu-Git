//! The optional GitHub token: read-only, kept in the system keychain, never
//! in `settings.json`, logs or the accessibility tree.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
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

/// `true` for every `cargo test` build and when `CTHULHU_KEYRING=sample`.
///
/// `keyring` 4.2 exposes `use_sample_store` only behind the `cli` feature,
/// which also links the native stores, and `Entry::new` installs the OS
/// store on first use. This process-local store is installed instead, before
/// any `Entry::new`, so tests never build the macOS keychain, Windows
/// Credential Manager, or Linux Secret Service.
fn sample_store_requested() -> bool {
    #[cfg(test)]
    {
        true
    }
    #[cfg(not(test))]
    {
        std::env::var_os("CTHULHU_KEYRING").is_some_and(|value| value == "sample")
    }
}

fn install_sample_store() {
    // The map is a static, so it exists before the flag. The flag is what a
    // test observes: `load_token` must have chosen this store, not `Entry::new`.
    SAMPLE_READY.store(true, Ordering::SeqCst);
}

fn native_entry() -> Result<keyring::Entry, TokenStoreError> {
    #[cfg(test)]
    {
        NATIVE_ENTRY_BUILDS.fetch_add(1, Ordering::SeqCst);
    }
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(store_error)
}

fn lock<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Blocks on the keychain; call it off the UI thread. `None` when nothing is stored.
/// A stored value that is not a token is an error, not "nothing stored", so
/// the settings keep pointing at the keychain.
pub fn load_token() -> Result<Option<Token>, TokenStoreError> {
    match dispatch(Kind::Load, keychain_timeout())? {
        KeychainReply::Loaded(result) => result,
        KeychainReply::Finished(_) => Err(TokenStoreError::Failed(
            "the keychain worker returned the wrong result".to_owned(),
        )),
    }
}

pub fn save_token(token: &Token) -> Result<(), TokenStoreError> {
    match dispatch(Kind::Save(token.clone()), keychain_timeout())? {
        KeychainReply::Finished(result) => result,
        KeychainReply::Loaded(_) => Err(TokenStoreError::Failed(
            "the keychain worker returned the wrong result".to_owned(),
        )),
    }
}

/// Removing a token that is not there succeeds.
pub fn delete_token() -> Result<(), TokenStoreError> {
    match dispatch(Kind::Delete, keychain_timeout())? {
        KeychainReply::Finished(result) => result,
        KeychainReply::Loaded(_) => Err(TokenStoreError::Failed(
            "the keychain worker returned the wrong result".to_owned(),
        )),
    }
}

fn keychain_timeout() -> Duration {
    #[cfg(test)]
    {
        if let Some(timeout) = *lock(&TIMEOUT_OVERRIDE) {
            return timeout;
        }
    }
    KEYCHAIN_TIMEOUT
}

/// One worker, one generation per call. A caller that hits the deadline marks
/// its generation cancelled. The worker is the only thread that talks to the
/// store, so a `set_password` that wakes up late cannot run after a newer
/// delete, and a cancelled save puts back whatever was stored before it.
fn dispatch(kind: Kind, timeout: Duration) -> Result<KeychainReply, TokenStoreError> {
    if sample_store_requested() {
        install_sample_store();
    }
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::SeqCst);
    let (sender, receiver) = mpsc::channel();
    enqueue(Job {
        generation,
        kind,
        reply: sender,
    })?;
    match receiver.recv_timeout(timeout) {
        Ok(reply) => {
            {
                let mut book = lock(&COMMIT);
                take_generation(&mut book.committed, generation);
            }
            Ok(reply)
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let committed = {
                let mut book = lock(&COMMIT);
                if take_generation(&mut book.committed, generation) {
                    true
                } else {
                    book.cancelled.push(generation);
                    false
                }
            };
            if committed {
                receiver.recv_timeout(Duration::from_secs(1)).map_err(|_| {
                    TokenStoreError::Unavailable("the keychain worker stopped".to_owned())
                })
            } else {
                Err(TokenStoreError::Locked(
                    "the keychain did not answer in time".to_owned(),
                ))
            }
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(TokenStoreError::Unavailable(
            "the keychain worker stopped".to_owned(),
        )),
    }
}

fn enqueue(job: Job) -> Result<(), TokenStoreError> {
    {
        let mut status = lock(&WORKER_STATUS);
        status.queued += 1;
        WORKER_STATUS_CV.notify_all();
    }
    if worker_sender()?.send(job).is_err() {
        let mut status = lock(&WORKER_STATUS);
        status.queued = status.queued.saturating_sub(1);
        return Err(TokenStoreError::Unavailable(
            "the keychain worker stopped".to_owned(),
        ));
    }
    Ok(())
}

fn worker_sender() -> Result<&'static Sender<Job>, TokenStoreError> {
    static SENDER: OnceLock<Sender<Job>> = OnceLock::new();
    if let Some(sender) = SENDER.get() {
        return Ok(sender);
    }
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("keychain".to_owned())
        .spawn(move || worker_loop(receiver))
        .map_err(|error| {
            TokenStoreError::Unavailable(format!("could not start the keychain worker: {error}"))
        })?;
    match SENDER.set(sender) {
        Ok(()) => SENDER.get().ok_or_else(|| {
            TokenStoreError::Unavailable("the keychain worker is missing".to_owned())
        }),
        Err(extra) => {
            // Another thread published the sender first. Dropping this one
            // disconnects the spare worker so it exits.
            drop(extra);
            SENDER.get().ok_or_else(|| {
                TokenStoreError::Unavailable("the keychain worker is missing".to_owned())
            })
        }
    }
}

fn worker_loop(incoming: Receiver<Job>) {
    while let Ok(job) = incoming.recv() {
        {
            let mut status = lock(&WORKER_STATUS);
            status.queued = status.queued.saturating_sub(1);
            status.running = true;
        }
        let Job {
            generation,
            kind,
            reply,
        } = job;
        let performed = match &kind {
            Kind::Load => Performed::Load(perform_load()),
            Kind::Save(token) => perform_save(generation, token),
            Kind::Delete => Performed::Delete(delete_secret()),
        };
        let answer = settle(generation, performed);
        {
            let mut status = lock(&WORKER_STATUS);
            status.running = false;
            WORKER_STATUS_CV.notify_all();
        }
        let _ = reply.send(answer);
    }
}

fn perform_load() -> Result<Option<Token>, TokenStoreError> {
    match read_secret()? {
        Some(value) => Token::parse(&value)
            .map(Some)
            .ok_or_else(|| TokenStoreError::Failed("the saved value is not a token".to_owned())),
        None => Ok(None),
    }
}

fn perform_save(generation: u64, token: &Token) -> Performed {
    let previous = match read_secret() {
        Ok(value) => value,
        Err(error) => {
            return Performed::Save {
                result: Err(error),
                previous: None,
            };
        }
    };
    // The deadline already fired and nothing has been written. Skipping the
    // write leaves the previous secret where it was.
    if !still_current(generation) {
        return Performed::Save {
            result: Err(discarded_error()),
            previous: None,
        };
    }
    Performed::Save {
        result: write_secret(token.expose()),
        previous,
    }
}

fn still_current(generation: u64) -> bool {
    !lock(&COMMIT).cancelled.contains(&generation)
}

fn settle(generation: u64, performed: Performed) -> KeychainReply {
    let cancelled = {
        let mut book = lock(&COMMIT);
        if take_generation(&mut book.cancelled, generation) {
            true
        } else {
            book.committed.push(generation);
            false
        }
    };
    match performed {
        Performed::Load(result) => {
            if cancelled {
                KeychainReply::Loaded(Err(discarded_error()))
            } else {
                KeychainReply::Loaded(result)
            }
        }
        Performed::Save { result, previous } => {
            if cancelled {
                // The password may already have been replaced while the
                // prompt was up. Put the previous value back before any
                // newer save or delete runs.
                if result.is_ok() {
                    revert_save(previous);
                }
                KeychainReply::Finished(Err(discarded_error()))
            } else {
                KeychainReply::Finished(result)
            }
        }
        Performed::Delete(result) => {
            if cancelled {
                KeychainReply::Finished(Err(discarded_error()))
            } else {
                KeychainReply::Finished(result)
            }
        }
    }
}

fn discarded_error() -> TokenStoreError {
    TokenStoreError::Locked("the keychain result belonged to an older request".to_owned())
}

fn revert_save(previous: Option<Zeroizing<String>>) {
    let reverted = match &previous {
        Some(old) => write_secret(old),
        None => delete_secret(),
    };
    // The caller has already moved on. A newer queued request still runs
    // after this and is the value that sticks.
    let _ = reverted;
}

fn read_secret() -> Result<Option<Zeroizing<String>>, TokenStoreError> {
    if sample_store_requested() {
        install_sample_store();
        return sample_get();
    }
    match native_entry()?.get_password() {
        Ok(value) => Ok(Some(Zeroizing::new(value))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(store_error(error)),
    }
}

fn write_secret(password: &str) -> Result<(), TokenStoreError> {
    if sample_store_requested() {
        install_sample_store();
        #[cfg(test)]
        wait_if_set_gate_closed();
        sample_set(password);
        return Ok(());
    }
    native_entry()?.set_password(password).map_err(store_error)
}

fn delete_secret() -> Result<(), TokenStoreError> {
    if sample_store_requested() {
        install_sample_store();
        sample_delete();
        return Ok(());
    }
    match native_entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(store_error(error)),
    }
}

fn sample_get() -> Result<Option<Zeroizing<String>>, TokenStoreError> {
    let store = lock(&SAMPLE);
    Ok(store.password.clone().map(Zeroizing::new))
}

fn sample_set(password: &str) {
    let mut store = lock(&SAMPLE);
    if let Some(mut old) = store.password.replace(password.to_owned()) {
        old.zeroize();
    }
    #[cfg(test)]
    store.mutations.push("set");
}

fn sample_delete() {
    let mut store = lock(&SAMPLE);
    if let Some(mut old) = store.password.take() {
        old.zeroize();
    }
    #[cfg(test)]
    store.mutations.push("delete");
}

struct SampleStore {
    password: Option<String>,
    #[cfg(test)]
    mutations: Vec<&'static str>,
}

static SAMPLE_READY: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static NATIVE_ENTRY_BUILDS: AtomicU64 = AtomicU64::new(0);
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

static SAMPLE: Mutex<SampleStore> = Mutex::new(SampleStore {
    password: None,
    #[cfg(test)]
    mutations: Vec::new(),
});

struct CommitBook {
    // A handful of in-flight generations. `Vec::new` is const, so this can live
    // in a static without a lazy initializer.
    cancelled: Vec<u64>,
    committed: Vec<u64>,
}

static COMMIT: Mutex<CommitBook> = Mutex::new(CommitBook {
    cancelled: Vec::new(),
    committed: Vec::new(),
});

fn take_generation(values: &mut Vec<u64>, generation: u64) -> bool {
    let Some(index) = values.iter().position(|value| *value == generation) else {
        return false;
    };
    values.swap_remove(index);
    true
}

struct WorkerStatus {
    queued: usize,
    running: bool,
}

static WORKER_STATUS: Mutex<WorkerStatus> = Mutex::new(WorkerStatus {
    queued: 0,
    running: false,
});
static WORKER_STATUS_CV: Condvar = Condvar::new();

#[cfg(test)]
static TIMEOUT_OVERRIDE: Mutex<Option<Duration>> = Mutex::new(None);

enum Kind {
    Load,
    Save(Token),
    Delete,
}

struct Job {
    generation: u64,
    kind: Kind,
    reply: Sender<KeychainReply>,
}

enum Performed {
    Load(Result<Option<Token>, TokenStoreError>),
    Save {
        result: Result<(), TokenStoreError>,
        previous: Option<Zeroizing<String>>,
    },
    Delete(Result<(), TokenStoreError>),
}

enum KeychainReply {
    Loaded(Result<Option<Token>, TokenStoreError>),
    Finished(Result<(), TokenStoreError>),
}

#[cfg(test)]
struct SetGate {
    closed: bool,
    waiters: usize,
}

#[cfg(test)]
static SET_GATE: Mutex<SetGate> = Mutex::new(SetGate {
    closed: false,
    waiters: 0,
});
#[cfg(test)]
static SET_GATE_CV: Condvar = Condvar::new();

#[cfg(test)]
fn wait_if_set_gate_closed() {
    let mut gate = lock(&SET_GATE);
    if !gate.closed {
        return;
    }
    gate.waiters += 1;
    SET_GATE_CV.notify_all();
    while gate.closed {
        gate = SET_GATE_CV
            .wait(gate)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
    gate.waiters -= 1;
    SET_GATE_CV.notify_all();
}

/// Runs `work` on a side thread. The keychain calls do not accept a deadline,
/// so a hung prompt must not pin the caller. Keychain mutations themselves go
/// through [`dispatch`], which keeps one worker and drops stale generations.
#[cfg(test)]
fn close_set_gate() {
    lock(&SET_GATE).closed = true;
}

#[cfg(test)]
fn open_set_gate() {
    let mut gate = lock(&SET_GATE);
    gate.closed = false;
    SET_GATE_CV.notify_all();
}

#[cfg(test)]
fn set_keychain_timeout(timeout: Option<Duration>) {
    *lock(&TIMEOUT_OVERRIDE) = timeout;
}

#[cfg(test)]
fn clear_sample_store() {
    let mut store = lock(&SAMPLE);
    if let Some(mut old) = store.password.take() {
        old.zeroize();
    }
    store.mutations.clear();
}

#[cfg(test)]
fn seed_sample_store(password: &str) {
    let mut store = lock(&SAMPLE);
    if let Some(mut old) = store.password.replace(password.to_owned()) {
        old.zeroize();
    }
    store.mutations.clear();
}

#[cfg(test)]
fn sample_mutations() -> Vec<&'static str> {
    lock(&SAMPLE).mutations.clone()
}

/// `queued`, `running`, and how many `set_password` calls are inside the gate.
#[cfg(test)]
fn wait_for_keychain(pred: impl Fn(usize, bool, usize) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut status = lock(&WORKER_STATUS);
    loop {
        let waiters = lock(&SET_GATE).waiters;
        if pred(status.queued, status.running, waiters) {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for the keychain worker (queued={}, running={})",
                status.queued, status.running
            );
        }
        let (next, _) = WORKER_STATUS_CV
            .wait_timeout(status, Duration::from_millis(50))
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        status = next;
    }
}

#[cfg(test)]
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
    use std::sync::{Mutex, MutexGuard};

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

    struct KeychainGuard {
        _lock: MutexGuard<'static, ()>,
    }

    fn keychain_guard() -> KeychainGuard {
        static LOCK: Mutex<()> = Mutex::new(());
        let guard = KeychainGuard { _lock: lock(&LOCK) };
        open_set_gate();
        set_keychain_timeout(None);
        clear_sample_store();
        guard
    }

    impl Drop for KeychainGuard {
        fn drop(&mut self) {
            open_set_gate();
            set_keychain_timeout(None);
            wait_for_keychain(|queued, running, _| queued == 0 && !running);
        }
    }

    fn parsed_token(value: &str) -> Token {
        Token::parse(value).expect("plausible token")
    }

    #[test]
    fn load_token_reads_the_sample_store_without_a_keychain() {
        let _guard = keychain_guard();
        let loaded = load_token().expect("sample store answers");
        assert!(
            loaded.is_none(),
            "an empty sample store is not-found, not a keychain error"
        );
        assert!(
            SAMPLE_READY.load(Ordering::SeqCst),
            "the sample store must be installed before any credential read"
        );
        assert_eq!(
            NATIVE_ENTRY_BUILDS.load(Ordering::SeqCst),
            0,
            "cargo test must not construct the OS keychain entry"
        );

        let token = parsed_token("ghp_abcdefghijklmnopqrstuvwxyz");
        save_token(&token).expect("save");
        let stored = load_token().expect("load").expect("stored");
        assert_eq!(stored.expose(), token.expose());
        delete_token().expect("delete");
        assert!(load_token().expect("load after delete").is_none());
        assert_eq!(NATIVE_ENTRY_BUILDS.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_timed_out_save_discards_the_late_write() {
        let _guard = keychain_guard();
        let previous = parsed_token("ghp_oldtokenvalue_abcdefghij");
        seed_sample_store(previous.expose());
        load_token().expect("start the keychain worker");
        close_set_gate();
        set_keychain_timeout(Some(Duration::from_millis(500)));
        let late = parsed_token("ghp_newtokenvalue_abcdefghij");
        let started = Instant::now();
        let save = thread::spawn(move || save_token(&late));
        wait_for_keychain(|_, _, waiters| waiters >= 1);
        let result = save.join().expect("save thread");
        assert!(
            matches!(result, Err(TokenStoreError::Locked(_))),
            "a blocked set_password must surface the locked timeout, got {result:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        open_set_gate();
        set_keychain_timeout(None);
        wait_for_keychain(|queued, running, _| queued == 0 && !running);
        let sets = sample_mutations().iter().filter(|op| **op == "set").count();
        assert!(
            sets >= 2,
            "the late set_password must run and then be reverted, mutations={:?}",
            sample_mutations()
        );
        let stored = load_token().expect("load");
        assert_eq!(
            stored.as_ref().map(Token::expose),
            Some(previous.expose()),
            "the generation that timed out must not keep the password it wrote late"
        );
    }

    #[test]
    fn a_late_save_does_not_overwrite_a_newer_delete() {
        let _guard = keychain_guard();
        load_token().expect("start the keychain worker");
        close_set_gate();
        set_keychain_timeout(Some(Duration::from_millis(500)));
        let late = parsed_token("ghp_newtokenvalue_abcdefghij");
        let save = thread::spawn(move || save_token(&late));
        wait_for_keychain(|_, _, waiters| waiters >= 1);
        let save_result = save.join().expect("save thread");
        assert!(
            matches!(save_result, Err(TokenStoreError::Locked(_))),
            "got {save_result:?}"
        );

        set_keychain_timeout(Some(Duration::from_secs(3)));
        let delete = thread::spawn(delete_token);
        wait_for_keychain(|queued, _, waiters| queued >= 1 && waiters >= 1);
        open_set_gate();
        let delete_result = delete.join().expect("delete thread");
        assert!(
            delete_result.is_ok(),
            "the delete queued behind the blocked save should finish, got {delete_result:?}"
        );
        wait_for_keychain(|queued, running, _| queued == 0 && !running);
        assert!(
            load_token().expect("load").is_none(),
            "the late set_password must not resurrect the token after delete"
        );
        let log = sample_mutations();
        let last_set = log.iter().rposition(|op| *op == "set");
        let last_delete = log.iter().rposition(|op| *op == "delete");
        assert!(
            last_set.is_some_and(|set| last_delete.is_some_and(|delete| delete > set)),
            "a set that lands after the delete would resurrect the secret, log={log:?}"
        );
    }
}
