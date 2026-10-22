//! Author avatars: a few download threads, a disk cache, GPU textures, and
//! initials when there is no picture.
//!
//! The URLs come from [`AvatarResolver`]; this module only fetches, decodes
//! and keeps the results. Nothing is requested while avatars are off.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cthulhu_git::git::Person;
use cthulhu_git::github::{
    ApiStatus, AvatarQuery, AvatarResolver, FetchError, HttpError, HttpResponse, MAX_BODY,
    RepoContext, Token, Transport, UreqTransport, fetch_image, sha256_hex,
};
use eframe::egui::epaint::RectShape;
use eframe::egui::{
    Align2, Color32, ColorImage, Context, CornerRadius, FontId, Painter, Pos2, Rect, TextureHandle,
    TextureOptions, Vec2,
};

use super::theme::Palette;

const WORKERS: usize = 4;
const IMAGE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Short, so a token added later or a new Gravatar shows up within a day.
const MISS_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Decoder limits: avatars are 96 px, nothing legitimate is near these.
const MAX_SIDE: u32 = 1024;
const MAX_DECODE_BYTES: u64 = 16 * 1024 * 1024;
const TEXTURE_SIDE: u32 = 96;

/// Wait before asking again when the API failed for a passing reason.
const RETRY_AFTER: Duration = Duration::from_secs(120);

enum Entry {
    Pending,
    Ready(TextureHandle),
    Missing,
    /// Nothing yet, but the API may answer later (it was paused or failed).
    Retry(Instant),
}

struct Job {
    key: String,
    emails: Vec<String>,
    repo: Option<RepoContext>,
    authored: Option<String>,
    generation: u64,
}

enum Loaded {
    Image(ColorImage),
    Missing,
    RetryAt(Instant),
}

struct Done {
    key: String,
    loaded: Loaded,
    /// The token generation the lookup ran with.
    generation: u64,
}

/// Pictures kept on the GPU. Older ones are dropped and reloaded from the
/// disk cache if they scroll back into view.
const MAX_TEXTURES: usize = 512;

pub struct AvatarService {
    enabled: bool,
    cache: Option<DiskCache>,
    /// Held by a worker for a whole API round trip. The UI thread never locks it.
    resolver: Arc<Mutex<AvatarResolver>>,
    /// Copied out of the resolver after each lookup, for the settings page.
    status: Arc<Mutex<ApiStatus>>,
    /// Bumped by [`Self::set_token`], so a lookup that ran with the old token
    /// does not leave a stale "no avatar".
    generation: Arc<AtomicU64>,
    entries: HashMap<String, Entry>,
    ready: VecDeque<String>,
    jobs: Option<Sender<Job>>,
    done_tx: Sender<Done>,
    done_rx: Receiver<Done>,
}

impl AvatarService {
    /// `cache_dir` is the per-user cache folder; `None` keeps avatars in memory only.
    pub fn new(cache_dir: Option<PathBuf>, enabled: bool) -> Self {
        Self::with_transport(
            Arc::new(UreqTransport::default()),
            cache_dir.map(|dir| DiskCache { dir }),
            enabled,
        )
    }

    /// Never fetches and draws initials only. For tests and when avatars are off.
    pub fn disabled() -> Self {
        Self::with_transport(Arc::new(Offline), None, false)
    }

    fn with_transport(
        transport: Arc<dyn Transport>,
        cache: Option<DiskCache>,
        enabled: bool,
    ) -> Self {
        let (done_tx, done_rx) = mpsc::channel();
        Self {
            enabled,
            cache,
            resolver: Arc::new(Mutex::new(AvatarResolver::new(transport))),
            status: Arc::new(Mutex::new(ApiStatus::default())),
            generation: Arc::new(AtomicU64::new(0)),
            entries: HashMap::new(),
            ready: VecDeque::new(),
            jobs: None,
            done_tx,
            done_rx,
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// A different token may reach more repositories: the API answers and the
    /// "no avatar" marks are forgotten, pictures already found are kept.
    /// Returns at once; the resolver takes the token on a thread of its own,
    /// because a worker may be holding it for a request.
    pub fn set_token(&mut self, token: Option<Token>) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.entries
            .retain(|_, entry| !matches!(entry, Entry::Missing | Entry::Retry(_)));
        let resolver = Arc::clone(&self.resolver);
        let status = Arc::clone(&self.status);
        let cache = self.cache.clone();
        thread::spawn(move || {
            let fresh = {
                let mut resolver = lock(&resolver);
                resolver.set_token(token);
                resolver.status().clone()
            };
            *lock(&status) = fresh;
            if let Some(cache) = cache {
                cache.clear_misses();
            }
        });
    }

    /// Never waits on a request in flight.
    pub fn api_status(&self) -> ApiStatus {
        lock(&self.status).clone()
    }

    /// Uploads the pictures that finished downloading. Call once per frame.
    pub fn poll(&mut self, ctx: &Context) {
        let generation = self.generation.load(Ordering::SeqCst);
        while let Ok(done) = self.done_rx.try_recv() {
            let entry = match done.loaded {
                Loaded::Image(image) => {
                    self.ready.push_back(done.key.clone());
                    Entry::Ready(ctx.load_texture(
                        format!("avatar-{}", sha256_hex(&done.key)),
                        image,
                        TextureOptions::LINEAR,
                    ))
                }
                // Asked with a token that has since changed: ask again.
                Loaded::Missing | Loaded::RetryAt(_) if done.generation != generation => {
                    self.entries.remove(&done.key);
                    continue;
                }
                Loaded::Missing => Entry::Missing,
                Loaded::RetryAt(at) => Entry::Retry(at),
            };
            self.entries.insert(done.key, entry);
        }
        while self.ready.len() > MAX_TEXTURES {
            if let Some(old) = self.ready.pop_front()
                && matches!(self.entries.get(&old), Some(Entry::Ready(_)))
            {
                self.entries.remove(&old);
            }
        }
    }

    /// The picture for `person`, once it has arrived. The first call queues
    /// the lookup. `authored` is a commit this person wrote, for the API.
    pub fn texture(
        &mut self,
        ctx: &Context,
        person: &Person,
        repo: Option<&RepoContext>,
        authored: Option<&str>,
    ) -> Option<TextureHandle> {
        if !self.enabled {
            return None;
        }
        let key = person.email_key();
        if key.is_empty() {
            return None;
        }
        match self.entries.get(&key) {
            Some(Entry::Ready(texture)) => return Some(texture.clone()),
            Some(Entry::Pending | Entry::Missing) => return None,
            Some(Entry::Retry(at)) if Instant::now() < *at => return None,
            Some(Entry::Retry(_)) | None => {}
        }
        self.entries.insert(key.clone(), Entry::Pending);
        let job = Job {
            key,
            emails: person.emails(),
            repo: repo.cloned(),
            authored: authored.map(str::to_owned),
            generation: self.generation.load(Ordering::SeqCst),
        };
        if self
            .sender(ctx)
            .is_none_or(|sender| sender.send(job).is_err())
        {
            self.jobs = None;
        }
        None
    }

    fn sender(&mut self, ctx: &Context) -> Option<&Sender<Job>> {
        if self.jobs.is_none() {
            let (sender, receiver) = mpsc::channel::<Job>();
            let receiver = Arc::new(Mutex::new(receiver));
            for index in 0..WORKERS {
                let receiver = Arc::clone(&receiver);
                let resolver = Arc::clone(&self.resolver);
                let status = Arc::clone(&self.status);
                let cache = self.cache.clone();
                let done = self.done_tx.clone();
                let ctx = ctx.clone();
                let spawned = thread::Builder::new()
                    .name(format!("avatar-{index}"))
                    .spawn(move || {
                        loop {
                            let job = match lock(&receiver).recv() {
                                Ok(job) => job,
                                Err(_) => return,
                            };
                            let loaded = load(&job, &resolver, &status, cache.as_ref());
                            if done
                                .send(Done {
                                    key: job.key,
                                    loaded,
                                    generation: job.generation,
                                })
                                .is_err()
                            {
                                return;
                            }
                            ctx.request_repaint();
                        }
                    });
                if spawned.is_err() {
                    return None;
                }
            }
            self.jobs = Some(sender);
        }
        self.jobs.as_ref()
    }
}

/// A poisoned lock only means a worker panicked mid-request; the state is
/// still usable.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn load(
    job: &Job,
    resolver: &Mutex<AvatarResolver>,
    status: &Mutex<ApiStatus>,
    cache: Option<&DiskCache>,
) -> Loaded {
    match cache.map(|cache| cache.lookup(&job.key)) {
        Some(Cached::Image(bytes)) => {
            if let Some(image) = decode(&bytes) {
                return Loaded::Image(image);
            }
        }
        Some(Cached::Miss) => return Loaded::Missing,
        Some(Cached::Unknown) | None => {}
    }
    let now = now_unix();
    let (answer, transport) = {
        let mut resolver = lock(resolver);
        let answer = resolver.resolve(
            &AvatarQuery {
                emails: &job.emails,
                repo: job.repo.as_ref(),
                authored: job.authored.as_deref(),
            },
            now,
        );
        *lock(status) = resolver.status().clone();
        (answer, resolver.transport())
    };
    let mut retryable = false;
    for url in &answer.urls {
        match fetch_image(transport.as_ref(), url) {
            Ok(bytes) => {
                if let Some(image) = decode(&bytes) {
                    if let Some(cache) = cache {
                        cache.store_image(&job.key, &bytes);
                    }
                    return Loaded::Image(image);
                }
            }
            Err(FetchError::Missing) | Err(FetchError::Failed(_)) => {}
            Err(FetchError::Unavailable) => retryable = true,
        }
    }
    if retryable {
        return Loaded::RetryAt(Instant::now() + RETRY_AFTER);
    }
    if answer.provisional {
        // The API may still find this person: do not remember a miss.
        let wait = answer
            .retry_at
            .map(|at| Duration::from_secs(at.saturating_sub(now)))
            .unwrap_or(RETRY_AFTER)
            .max(Duration::from_secs(1));
        return Loaded::RetryAt(Instant::now() + wait);
    }
    if let Some(cache) = cache {
        cache.store_miss(&job.key);
    }
    Loaded::Missing
}

/// PNG or JPEG within [`MAX_SIDE`], cropped square and scaled to
/// [`TEXTURE_SIDE`] when larger.
pub(crate) fn decode(bytes: &[u8]) -> Option<ColorImage> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    // Whatever other crates enable in `image`, only these two are decoded.
    if !matches!(
        reader.format(),
        Some(image::ImageFormat::Png | image::ImageFormat::Jpeg)
    ) {
        return None;
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let mut picture = reader.decode().ok()?;
    if picture.width() == 0 || picture.height() == 0 {
        return None;
    }
    if picture.width() != picture.height() || picture.width() > TEXTURE_SIDE {
        let side = picture.width().min(picture.height()).min(TEXTURE_SIDE);
        picture = picture.resize_to_fill(side, side, image::imageops::FilterType::Triangle);
    }
    let rgba = picture.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Some(ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}

enum Cached {
    Image(Vec<u8>),
    Miss,
    Unknown,
}

/// `<sha256(address)>.img` for a picture, `.miss` for "nothing found".
/// Best effort: a write that fails only means the lookup runs again.
#[derive(Clone)]
struct DiskCache {
    dir: PathBuf,
}

impl DiskCache {
    fn path(&self, key: &str, extension: &str) -> PathBuf {
        self.dir.join(format!("{}.{extension}", sha256_hex(key)))
    }

    fn lookup(&self, key: &str) -> Cached {
        let image = self.path(key, "img");
        // Plain files no larger than a download, so a planted symlink or a
        // huge file is never read.
        let small_file = fs::symlink_metadata(&image)
            .is_ok_and(|meta| meta.file_type().is_file() && meta.len() <= MAX_BODY);
        if small_file
            && fresh(&image, IMAGE_TTL)
            && let Ok(bytes) = fs::read(&image)
        {
            return Cached::Image(bytes);
        }
        if fresh(&self.path(key, "miss"), MISS_TTL) {
            return Cached::Miss;
        }
        Cached::Unknown
    }

    fn store_image(&self, key: &str, bytes: &[u8]) {
        let _ = self.write(&self.path(key, "img"), bytes);
        let _ = fs::remove_file(self.path(key, "miss"));
    }

    fn store_miss(&self, key: &str) {
        let _ = self.write(&self.path(key, "miss"), b"");
    }

    fn write(&self, path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, path).inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })
    }

    fn clear_misses(&self) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "miss")
            {
                let _ = fs::remove_file(path);
            }
        }
    }
}

fn fresh(path: &std::path::Path, ttl: Duration) -> bool {
    fs::symlink_metadata(path)
        .ok()
        .filter(|meta| meta.file_type().is_file())
        .and_then(|meta| meta.modified().ok())
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age < ttl)
}

/// Refuses every request.
struct Offline;

impl Transport for Offline {
    fn get_once(&self, url: &str, _: &[(&str, String)]) -> Result<HttpResponse, HttpError> {
        Err(HttpError::NotAllowed(url.to_owned()))
    }
}

/// Up to two letters: the first of the first and last words of the name, or
/// the first letter of the address.
pub fn initials(person: &Person) -> String {
    let words: Vec<&str> = person
        .name
        .split_whitespace()
        .filter(|word| word.chars().next().is_some_and(char::is_alphanumeric))
        .collect();
    let mut text: String = match words.as_slice() {
        [] => String::new(),
        [only] => first_letter(only).into_iter().collect(),
        [head, .., tail] => first_letter(head)
            .into_iter()
            .chain(first_letter(tail))
            .collect(),
    };
    if text.is_empty() {
        text = person
            .email
            .chars()
            .find(|c| c.is_alphanumeric())
            .map(single_upper)
            .into_iter()
            .collect();
    }
    if text.is_empty() {
        text.push('?');
    }
    text
}

fn first_letter(word: &str) -> Option<char> {
    word.chars().next().map(single_upper)
}

/// The uppercase letter, unless uppercasing makes more than one (`ß` is
/// `SS`, `ﬃ` is `FFI`): then the letter as written, so initials stay two.
fn single_upper(letter: char) -> char {
    let mut upper = letter.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(only), None) => only,
        _ => letter,
    }
}

/// A lane color picked from the address, so a person keeps one color.
fn initials_color(person: &Person) -> usize {
    person
        .email_key()
        .bytes()
        .fold(0x811c_9dc5_u32, |hash, byte| {
            (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
        }) as usize
}

/// A round avatar, or initials on a faded lane color. `ring` draws a border
/// in that color, so stacked avatars stay apart.
pub fn paint_avatar(
    painter: &Painter,
    center: Pos2,
    diameter: f32,
    person: &Person,
    texture: Option<&TextureHandle>,
    palette: &Palette,
    ring: Option<Color32>,
) {
    let radius = diameter / 2.0;
    if let Some(ring) = ring {
        painter.circle_filled(center, radius + 1.5, ring);
    }
    let rect = Rect::from_center_size(center, Vec2::splat(diameter));
    if let Some(texture) = texture {
        let corner = CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8);
        painter.add(
            RectShape::filled(rect, corner, Color32::WHITE).with_texture(
                texture.id(),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            ),
        );
        return;
    }
    let color = initials_color(person);
    painter.circle_filled(center, radius, palette.background);
    painter.circle_filled(center, radius, palette.graph_chip(color, true));
    painter.text(
        center,
        Align2::CENTER_CENTER,
        initials(person),
        FontId::proportional(diameter * 0.42),
        palette.graph_color(color),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(name: &str, email: &str) -> Person {
        Person::new(name, email)
    }

    #[test]
    fn initials_use_the_first_and_last_words() {
        assert_eq!(initials(&person("Ada Voss", "a@b")), "AV");
        assert_eq!(initials(&person("Herbert West Jr", "a@b")), "HJ");
        assert_eq!(initials(&person("cthulhu", "a@b")), "C");
        assert_eq!(initials(&person("  ", "zed@b")), "Z");
        assert_eq!(initials(&person("", "")), "?");
        assert_eq!(initials(&person("ñandú ébano", "a@b")), "ÑÉ");
        assert_eq!(initials(&person("- Cursor Agent", "a@b")), "CA");
        assert_eq!(initials(&person("ß ß", "a@b")), "ßß");
        assert_eq!(initials(&person("ßeta Smith", "a@b")), "ßS");
        assert_eq!(initials(&person("", "\u{fb03}x@b")), "\u{fb03}");
    }

    #[test]
    fn a_person_keeps_one_color() {
        let a = person("Ada", "Ada@Miskatonic.test");
        let b = person("Someone else", " ada@miskatonic.test ");
        assert_eq!(initials_color(&a), initials_color(&b));
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .expect("encode");
        bytes
    }

    #[test]
    fn decodes_and_squares_pictures_within_the_limits() {
        let square = decode(&png(48, 48)).expect("small png");
        assert_eq!(square.size, [48, 48]);
        let wide = decode(&png(300, 200)).expect("wide png");
        assert_eq!(wide.size, [96, 96]);
        assert!(decode(&png(2000, 10)).is_none(), "wider than MAX_SIDE");
        assert!(decode(b"not an image").is_none());
        assert!(decode(b"").is_none());
    }

    #[test]
    fn the_disk_cache_keeps_pictures_and_misses_apart() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cache = DiskCache {
            dir: dir.path().join("avatars"),
        };
        assert!(matches!(
            cache.lookup("ada@miskatonic.test"),
            Cached::Unknown
        ));
        cache.store_miss("ada@miskatonic.test");
        assert!(matches!(cache.lookup("ada@miskatonic.test"), Cached::Miss));
        cache.store_image("ada@miskatonic.test", b"bytes");
        assert!(
            matches!(cache.lookup("ada@miskatonic.test"), Cached::Image(bytes) if bytes == b"bytes")
        );
        cache.store_miss("west@arkham.test");
        cache.clear_misses();
        assert!(matches!(cache.lookup("west@arkham.test"), Cached::Unknown));
        let names: Vec<String> = fs::read_dir(dir.path().join("avatars"))
            .expect("dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        assert!(
            !names[0].contains("ada"),
            "file names are hashed: {names:?}"
        );
    }

    #[test]
    fn a_stale_entry_is_unknown() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cache = DiskCache {
            dir: dir.path().to_path_buf(),
        };
        cache.store_miss("old@example.test");
        let path = cache.path("old@example.test", "miss");
        let old = SystemTime::now() - MISS_TTL - Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(&path)
            .and_then(|file| file.set_modified(old))
            .expect("age the file");
        assert!(matches!(cache.lookup("old@example.test"), Cached::Unknown));
    }

    #[test]
    fn a_disabled_service_never_queues_work() {
        let ctx = Context::default();
        let mut service = AvatarService::disabled();
        let ada = person("Ada", "ada@miskatonic.test");
        assert!(service.texture(&ctx, &ada, None, None).is_none());
        assert!(service.jobs.is_none());
        assert!(service.entries.is_empty());
    }

    #[test]
    fn an_offline_service_ends_with_initials() {
        let ctx = Context::default();
        let mut service = AvatarService::with_transport(Arc::new(Offline), None, true);
        let ada = person("Ada", "ada@miskatonic.test");
        assert!(service.texture(&ctx, &ada, None, None).is_none());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while matches!(
            service.entries.get("ada@miskatonic.test"),
            Some(Entry::Pending)
        ) && std::time::Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(10));
            service.poll(&ctx);
        }
        assert!(matches!(
            service.entries.get("ada@miskatonic.test"),
            Some(Entry::Missing)
        ));
        assert!(service.texture(&ctx, &ada, None, None).is_none());
    }

    struct FirstTimeout {
        direct: String,
        gravatar: String,
        picture: Vec<u8>,
    }

    impl Transport for FirstTimeout {
        fn get_once(&self, url: &str, _: &[(&str, String)]) -> Result<HttpResponse, HttpError> {
            if url == self.direct {
                return Err(HttpError::TimedOut);
            }
            if url == self.gravatar {
                return Ok(HttpResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: self.picture.clone(),
                });
            }
            Err(HttpError::NotAllowed(url.to_owned()))
        }
    }

    #[test]
    fn a_timeout_tries_the_next_avatar_url() {
        let email = "1+ada@users.noreply.github.com";
        let direct = cthulhu_git::github::direct_avatar_url(email).expect("direct");
        let gravatar = cthulhu_git::github::gravatar_url(email);
        let transport = Arc::new(FirstTimeout {
            direct,
            gravatar,
            picture: png(8, 8),
        });
        let ctx = Context::default();
        let mut service = AvatarService::with_transport(transport, None, true);
        let ada = person("Ada", email);
        assert!(service.texture(&ctx, &ada, None, None).is_none());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !matches!(
            service.entries.get(email),
            Some(Entry::Ready(_)) | Some(Entry::Missing) | Some(Entry::Retry(_))
        ) && std::time::Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(10));
            service.poll(&ctx);
        }
        assert!(
            matches!(service.entries.get(email), Some(Entry::Ready(_))),
            "the gravatar should be used after the github avatar times out, got {:?}",
            service.entries.get(email).map(std::mem::discriminant)
        );
    }

    struct AlwaysTimeout;

    impl Transport for AlwaysTimeout {
        fn get_once(&self, _: &str, _: &[(&str, String)]) -> Result<HttpResponse, HttpError> {
            Err(HttpError::TimedOut)
        }
    }

    #[test]
    fn every_timeout_retries_without_a_miss_file() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let ctx = Context::default();
        let mut service = AvatarService::with_transport(
            Arc::new(AlwaysTimeout),
            Some(DiskCache {
                dir: dir.path().to_path_buf(),
            }),
            true,
        );
        let email = "1+ada@users.noreply.github.com";
        let ada = person("Ada", email);
        assert!(service.texture(&ctx, &ada, None, None).is_none());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !matches!(service.entries.get(email), Some(Entry::Retry(_)))
            && std::time::Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(10));
            service.poll(&ctx);
        }
        assert!(
            matches!(service.entries.get(email), Some(Entry::Retry(_))),
            "got {:?}",
            service.entries.get(email).map(std::mem::discriminant)
        );
        let files: Vec<_> = fs::read_dir(dir.path())
            .expect("dir")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert!(
            files.is_empty(),
            "a timeout must not be stored as a miss: {files:?}"
        );
    }
}
