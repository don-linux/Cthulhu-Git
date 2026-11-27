//! Author avatars: a few download threads, a disk cache, GPU textures, and
//! initials when there is no picture.
//!
//! The URLs come from [`AvatarResolver`]; this module only fetches, decodes
//! and keeps the results. Nothing is requested while avatars are off.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use cthulhu_git::git::Person;
use cthulhu_git::github::{
    ApiStatus, AvatarQuery, AvatarResolver, FetchError, MAX_BODY, RepoContext, Token, Transport,
    UreqTransport, fetch_image, sha256_hex,
};
use cthulhu_git::settings::write_atomic;
use eframe::egui::epaint::RectShape;
use eframe::egui::{
    Align2, Color32, ColorImage, Context, FontId, Painter, Pos2, Rect, TextureHandle,
    TextureOptions, Vec2,
};

use super::clock::now_unix;
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
    /// does not leave a stale "no avatar". Only this thread reads it; a job
    /// carries its copy to the worker and back.
    generation: u64,
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
            generation: 0,
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
        self.generation += 1;
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
                Loaded::Missing | Loaded::RetryAt(_) if done.generation != self.generation => {
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
            generation: self.generation,
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
            Err(FetchError::Missing) => {}
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
        let _ = write_atomic(&self.path(key, "img"), bytes);
        let _ = fs::remove_file(self.path(key, "miss"));
    }

    fn store_miss(&self, key: &str) {
        let _ = write_atomic(&self.path(key, "miss"), b"");
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

fn fresh(path: &Path, ttl: Duration) -> bool {
    fs::symlink_metadata(path)
        .ok()
        .filter(|meta| meta.file_type().is_file())
        .and_then(|meta| meta.modified().ok())
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age < ttl)
}

/// An avatar service for tests anywhere in the app.
#[cfg(test)]
mod offline {
    use std::sync::Arc;

    use cthulhu_git::github::{HttpError, HttpResponse, Transport};

    use super::AvatarService;

    /// Refuses every request.
    pub(super) struct Offline;

    impl Transport for Offline {
        fn get_once(&self, _: &str, _: &[(&str, String)]) -> Result<HttpResponse, HttpError> {
            Err(HttpError::NotAllowed)
        }
    }

    impl AvatarService {
        /// Never fetches and draws initials only, even if a test turns avatars on.
        pub fn disabled() -> Self {
            Self::with_transport(Arc::new(Offline), None, false)
        }
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
        painter.add(
            RectShape::filled(rect, radius, Color32::WHITE).with_texture(
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
    use cthulhu_git::github::{HttpError, HttpResponse, direct_avatar_url, gravatar_url};
    use image::ImageFormat;
    use proptest::prelude::*;

    use super::offline::Offline;
    use super::*;
    use crate::ui::test_support::proptest_config;

    #[test]
    fn initials_are_the_first_and_last_words_or_the_address() {
        // (name, email, initials). A letter whose uppercase is several (ß is
        // SS, ﬃ is FFI) stays as written, so initials stay two letters.
        const CASES: [(&str, &str, &str); 18] = [
            ("Ada Voss", "a@b", "AV"),
            ("Herbert West Jr", "a@b", "HJ"),
            ("cthulhu", "a@b", "C"),
            ("  ", "zed@b", "Z"),
            ("", "", "?"),
            ("ñandú ébano", "a@b", "ÑÉ"),
            ("- Cursor Agent", "a@b", "CA"),
            ("ß", "a@b", "ß"),
            ("ß ß", "a@b", "ßß"),
            ("ßeta Smith", "a@b", "ßS"),
            ("ﬁ", "a@b", "ﬁ"),
            ("ﬀ", "a@b", "ﬀ"),
            ("ŉ", "", "ŉ"),
            ("ﬃ", "", "ﬃ"),
            ("ﬄ", "", "ﬄ"),
            ("ﬁ ﬂ", "", "ﬁﬂ"),
            ("", "\u{fb03}x@b", "\u{fb03}"),
            ("!!!", "ﬃ@example.test", "ﬃ"),
        ];
        for (name, email, expected) in CASES {
            let person = Person::new(name, email);
            assert_eq!(initials(&person), expected, "{name:?} {email:?}");
        }
    }

    proptest! {
        #![proptest_config(proptest_config())]

        #[test]
        fn initials_are_one_or_two_letters(
            name in prop::collection::vec(any::<char>(), 0..=24),
            email in prop::collection::vec(any::<char>(), 0..=24),
        ) {
            let person = Person::new(String::from_iter(name), String::from_iter(email));
            let text = initials(&person);
            prop_assert!((1..=2).contains(&text.chars().count()), "{person:?} -> {text:?}");
        }
    }

    #[test]
    fn a_person_keeps_one_color() {
        let a = Person::new("Ada", "Ada@Miskatonic.test");
        let b = Person::new("Someone else", " ada@miskatonic.test ");
        assert_eq!(initials_color(&a), initials_color(&b));
    }

    fn encode(
        width: u32,
        height: u32,
        format: ImageFormat,
        pixel: impl Fn(u32, u32) -> [u8; 4],
    ) -> Vec<u8> {
        let picture = image::RgbaImage::from_fn(width, height, |x, y| image::Rgba(pixel(x, y)));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(picture)
            .write_to(&mut Cursor::new(&mut bytes), format)
            .unwrap_or_else(|error| panic!("encode {format:?} {width}x{height}: {error}"));
        bytes
    }

    fn solid(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
        encode(width, height, format, |_, _| [20, 40, 80, 255])
    }

    #[test]
    fn decode_squares_png_and_jpeg_within_the_limits() {
        // (width, height, side of the square, or refused). Each size runs both
        // ways round, as PNG and as JPEG.
        const SIZES: [(u32, u32, Option<usize>); 6] = [
            (48, 48, Some(48)),
            (97, 97, Some(96)),
            (180, 90, Some(90)),
            (1024, 16, Some(16)),
            (300, 200, Some(96)),
            (1025, 16, None),
        ];
        for format in [ImageFormat::Png, ImageFormat::Jpeg] {
            for (long, short, side) in SIZES {
                for (width, height) in [(long, short), (short, long)] {
                    let size = decode(&solid(width, height, format)).map(|image| image.size);
                    let expected = side.map(|side| [side, side]);
                    assert_eq!(size, expected, "{format:?} {width}x{height}");
                }
            }
        }
        // Thirds of red, green and blue. The center crop keeps green only; a
        // stretch would bring red and blue in at the ends.
        for vertical in [false, true] {
            let (width, height) = if vertical { (40, 120) } else { (120, 40) };
            let bytes = encode(width, height, ImageFormat::Png, |x, y| {
                let along = if vertical { y } else { x };
                match along / 40 {
                    0 => [220, 0, 0, 255],
                    1 => [0, 200, 0, 255],
                    _ => [0, 0, 220, 255],
                }
            });
            let picture = decode(&bytes).expect("banded png");
            assert_eq!(picture.size, [40, 40], "vertical {vertical}");
            for along in [10, 20, 30] {
                let (x, y) = if vertical { (20, along) } else { (along, 20) };
                let [red, green, blue, alpha] = picture.pixels[y * 40 + x].to_array();
                assert!(
                    green > 180 && red < 40 && blue < 40 && alpha > 200,
                    "vertical {vertical}: pixel {x},{y} is {red},{green},{blue},{alpha}"
                );
            }
        }
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                let mix = if crc & 1 == 1 { 0xEDB8_8320 } else { 0 };
                crc = (crc >> 1) ^ mix;
            }
        }
        !crc
    }

    const PNG_SIGNATURE: &[u8] = &[137, 80, 78, 71, 13, 10, 26, 10];

    /// The signature and one IHDR chunk: a size and no pixels.
    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut chunk = b"IHDR".to_vec();
        chunk.extend(width.to_be_bytes());
        chunk.extend(height.to_be_bytes());
        chunk.extend([8, 6, 0, 0, 0]);
        let mut file = PNG_SIGNATURE.to_vec();
        file.extend(13u32.to_be_bytes());
        file.extend(&chunk);
        file.extend(crc32(&chunk).to_be_bytes());
        file
    }

    /// 1×1 white GIF89a: LZW minimum code size 2, then clear, index 0, end.
    const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\xff\xff\xff\x00\x00\x00\
        \x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02\x44\x01\x00\x3b";
    /// 1×1 white BMP, 24 bits per pixel.
    const BMP: &[u8] = b"BM\x3a\x00\x00\x00\x00\x00\x00\x00\x36\x00\x00\x00\x28\x00\x00\x00\
        \x01\x00\x00\x00\x01\x00\x00\x00\x01\x00\x18\x00\x00\x00\x00\x00\x04\x00\x00\x00\
        \x13\x0b\x00\x00\x13\x0b\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\xff\xff\xff\x00";

    #[test]
    fn decode_refuses_other_formats_and_broken_pictures() {
        let png = solid(32, 24, ImageFormat::Png);
        let jpeg = solid(32, 24, ImageFormat::Jpeg);
        // The header row must fail on its size, not on a bad checksum.
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(png[29..33], crc32(&png[12..29]).to_be_bytes(), "IHDR crc");
        // A one-byte JPEG change can still decode, so all but the magic goes.
        let wiped = |bytes: &[u8]| {
            let mut bytes = bytes.to_vec();
            bytes[4..].fill(0);
            bytes
        };
        let cases = [
            ("empty", Vec::new()),
            ("text", b"not an image".to_vec()),
            ("unknown bytes", vec![0, 1, 2, 3, 4, 5, 6, 7]),
            ("png signature", PNG_SIGNATURE.to_vec()),
            ("jpeg header", b"\xff\xd8\xff".to_vec()),
            ("gif header", b"GIF89a".to_vec()),
            ("truncated png", png[..png.len() / 3].to_vec()),
            ("truncated jpeg", jpeg[..jpeg.len() / 3].to_vec()),
            ("wiped png", wiped(&png)),
            ("wiped jpeg", wiped(&jpeg)),
            ("100000x100000 png header", png_header(100_000, 100_000)),
            // Well formed, but not PNG or JPEG. Through arboard, Windows builds
            // decode BMP and macOS builds TIFF; no build decodes GIF.
            ("gif", GIF.to_vec()),
            ("bmp", BMP.to_vec()),
        ];
        for (case, bytes) in cases {
            assert!(decode(&bytes).is_none(), "{case}");
        }
    }

    #[test]
    fn the_disk_cache_keeps_pictures_and_misses_apart() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cache = DiskCache {
            dir: dir.path().join("avatars"),
        };
        let ada = "ada@miskatonic.test";
        assert!(matches!(cache.lookup(ada), Cached::Unknown));
        cache.store_miss(ada);
        assert!(matches!(cache.lookup(ada), Cached::Miss));
        cache.store_image(ada, b"bytes");
        assert!(matches!(cache.lookup(ada), Cached::Image(bytes) if bytes == b"bytes"));
        cache.store_miss("west@arkham.test");
        cache.clear_misses();
        assert!(matches!(cache.lookup("west@arkham.test"), Cached::Unknown));
        let names: Vec<String> = fs::read_dir(&cache.dir)
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

        // Older than its time to live, an entry is unknown again.
        cache.store_miss("old@example.test");
        let old = SystemTime::now() - MISS_TTL - Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(cache.path("old@example.test", "miss"))
            .and_then(|file| file.set_modified(old))
            .expect("age the file");
        assert!(matches!(cache.lookup("old@example.test"), Cached::Unknown));
    }

    #[test]
    fn a_disabled_service_never_queues_work() {
        let ctx = Context::default();
        let mut service = AvatarService::disabled();
        let ada = Person::new("Ada", "ada@miskatonic.test");
        assert!(service.texture(&ctx, &ada, None, None).is_none());
        assert!(service.jobs.is_none());
        assert!(service.entries.is_empty());
    }

    /// Times out on the GitHub avatar and serves a picture from Gravatar.
    struct FirstTimeout {
        direct: String,
        gravatar: String,
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
                    body: solid(8, 8, ImageFormat::Png),
                });
            }
            Err(HttpError::NotAllowed)
        }
    }

    struct AlwaysTimeout;

    impl Transport for AlwaysTimeout {
        fn get_once(&self, _: &str, _: &[(&str, String)]) -> Result<HttpResponse, HttpError> {
            Err(HttpError::TimedOut)
        }
    }

    fn state(entry: Option<&Entry>) -> &'static str {
        match entry {
            None => "none",
            Some(Entry::Pending) => "pending",
            Some(Entry::Ready(_)) => "ready",
            Some(Entry::Missing) => "missing",
            Some(Entry::Retry(_)) => "retry",
        }
    }

    #[test]
    fn a_lookup_ends_with_a_picture_a_miss_or_a_retry() {
        let noreply = "1+ada@users.noreply.github.com";
        let first_timeout = FirstTimeout {
            direct: direct_avatar_url(noreply).expect("direct"),
            gravatar: gravatar_url(noreply),
        };
        let offline: Arc<dyn Transport> = Arc::new(Offline);
        let gravatar: Arc<dyn Transport> = Arc::new(first_timeout);
        let timeouts: Arc<dyn Transport> = Arc::new(AlwaysTimeout);
        let plain = "ada@miskatonic.test";
        // (case, transport, address, disk cache, where the entry settles, the file left)
        #[rustfmt::skip]
        let cases = [
            ("offline", Arc::clone(&offline), plain, true, "missing", Some("miss")),
            ("offline in memory", offline, plain, false, "missing", None),
            ("gravatar after a timeout", Arc::clone(&gravatar), noreply, true, "ready", Some("img")),
            ("gravatar in memory", gravatar, noreply, false, "ready", None),
            // A timeout is not a miss: a later session asks again.
            ("every request times out", timeouts, noreply, true, "retry", None),
        ];
        let ctx = Context::default();
        for (case, transport, email, cached, settles, file) in cases {
            let dir = tempfile::TempDir::new().expect("temp dir");
            let cache = cached.then(|| DiskCache {
                dir: dir.path().to_path_buf(),
            });
            let mut service = AvatarService::with_transport(transport, cache, true);
            let ada = Person::new("Ada", email);
            assert!(service.texture(&ctx, &ada, None, None).is_none(), "{case}");
            let deadline = Instant::now() + Duration::from_secs(5);
            while state(service.entries.get(email)) == "pending" && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
                service.poll(&ctx);
            }
            assert_eq!(state(service.entries.get(email)), settles, "{case}");
            // A settled lookup is not queued again.
            let shown = service.texture(&ctx, &ada, None, None).is_some();
            assert_eq!(shown, settles == "ready", "{case}");
            assert_eq!(state(service.entries.get(email)), settles, "{case}");
            let files: Vec<String> = fs::read_dir(dir.path())
                .expect("dir")
                .filter_map(|entry| {
                    let path = entry.expect("entry").path();
                    path.extension()
                        .map(|kind| kind.to_string_lossy().into_owned())
                })
                .collect();
            assert_eq!(files, Vec::from_iter(file.map(str::to_owned)), "{case}");
        }
    }

    /// A symlink planted at the cache path or at a leftover temp name, pointing
    /// outside the cache, is never written through: the outside file keeps its
    /// bytes, the cache path holds the image, and no temp file remains.
    #[cfg(unix)]
    #[test]
    fn avatar_cache_does_not_write_through_a_planted_symlink() {
        let key = "ada@miskatonic.test";
        let image = format!("{}.img", sha256_hex(key));
        for planted in [image.clone(), format!("{image}.{}.tmp", std::process::id())] {
            let cache_dir = tempfile::TempDir::new().expect("cache dir");
            let outside = tempfile::TempDir::new().expect("outside dir");
            let cache = DiskCache {
                dir: cache_dir.path().to_path_buf(),
            };
            let target = outside.path().join("precious.txt");
            fs::write(&target, b"precious").expect("target");
            std::os::unix::fs::symlink(&target, cache_dir.path().join(&planted)).expect("symlink");

            cache.store_image(key, b"image-bytes");

            assert_eq!(fs::read(&target).expect("target"), b"precious", "{planted}");
            // `lookup` reads only a regular file, never a link.
            assert!(
                matches!(cache.lookup(key), Cached::Image(bytes) if bytes == b"image-bytes"),
                "{planted}"
            );
            let mut names: Vec<String> = fs::read_dir(cache_dir.path())
                .expect("dir")
                .map(|entry| {
                    entry
                        .expect("entry")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            names.sort();
            let mut expected = vec![image.clone(), planted.clone()];
            expected.dedup();
            assert_eq!(names, expected, "{planted}: no temp file remains");
        }
    }
}
