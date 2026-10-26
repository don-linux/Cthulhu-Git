//! Extra cases for avatars, the hover cache and the GitHub Auth page, all
//! without the network or the real keychain.
//!
//! Nothing here constructs [`AvatarService::new`], [`UreqTransport`](cthulhu_git::github::UreqTransport),
//! or calls the keychain (`load_token`, `save_token`, `delete_token`,
//! `GitHubAuth::load`, `save_draft`, `remove`).

use std::cell::RefCell;
use std::io::Cursor;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use cthulhu_git::git::{
    Branch, CommitDetail, GraphCommit, GraphTip, Head, Person, RepoInfo, Upstream, layout_graph,
};
use cthulhu_git::github::ApiStatus;
use cthulhu_git::settings::HistoryColumns;
use eframe::egui::accesskit::{Role, Toggled};
use eframe::egui::output::OutputCommand;
use eframe::egui::{self, ColorImage};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use image::ImageFormat;
use proptest::prelude::*;
use proptest::string::string_regex;
use proptest::test_runner::FileFailurePersistence;

use super::avatars::{self, AvatarService};
use super::fonts::FontService;
use super::github_auth::{GitHubAuth, Stored};
use super::hover::{self, HoverCache, Segment};
use super::repo_view;
use super::settings::{self, AboutInfo, SettingsView};
use super::theme;
use super::{Action, ListedBranch, OpenedRepo};

const DAY_SECS: i64 = 24 * 60 * 60;
/// `ghp_` plus 36 characters: the shape of a classic personal access token.
const FAKE_TOKEN: &str = "ghp_agentnetworktesttokenvalueXXXXXXXXXX";
const TOKEN_LINK: &str = "Open GitHub's token form, already set to read-only";
const AVATAR_CHECKBOX: &str = "Show author avatars";
const HOVER_SUMMARY: &str = "Lanterns for the night expedition";
const HOVER_AUTHOR: &str = "Ada Voss";
const HOVER_COAUTHOR: &str = "Herbert West";
const POSSIBLE_LATER: [&str; 5] = [
    "Pull requests on each branch label: number, title, state and reviews",
    "CI status for each commit, beside its hash",
    "Titles of the issues and pull requests a message mentions, in the hover",
    "Releases and tags with their notes",
    "Who approved or merged a pull request",
];

fn proptest_config() -> ProptestConfig {
    ProptestConfig {
        cases: 64,
        failure_persistence: Some(Box::new(FileFailurePersistence::Off)),
        ..ProptestConfig::default()
    }
}

fn arb_text(max_chars: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..=max_chars)
        .prop_map(|chars| chars.into_iter().collect())
}

fn arb_issue_text() -> impl Strategy<Value = String> {
    prop_oneof![
        string_regex("\\PC{0,36}").expect("any-character regex"),
        string_regex("(GH-|#|[A-Za-z0-9_.]|ünïcode|✓| |/){0,20}").expect("marker regex"),
    ]
}

/// `image` is built with `png` and `jpeg` only. GIF is covered separately.
fn raster_formats() -> [ImageFormat; 2] {
    [ImageFormat::Png, ImageFormat::Jpeg]
}

fn encode_image(
    width: u32,
    height: u32,
    format: ImageFormat,
    pixel: impl Fn(u32, u32) -> [u8; 4],
) -> Vec<u8> {
    let image = image::RgbaImage::from_fn(width, height, |x, y| image::Rgba(pixel(x, y)));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut Cursor::new(&mut bytes), format)
        .unwrap_or_else(|error| panic!("encode {format:?} {width}x{height}: {error}"));
    bytes
}

fn solid(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
    encode_image(width, height, format, |_, _| [20, 40, 80, 255])
}

/// Left/top third red, middle third green, right/bottom third blue.
fn banded(width: u32, height: u32, format: ImageFormat, vertical: bool) -> Vec<u8> {
    encode_image(width, height, format, move |x, y| {
        let along = if vertical { y } else { x };
        let limit = if vertical { height } else { width };
        let third = limit / 3;
        if along < third {
            [220, 0, 0, 255]
        } else if along < third * 2 {
            [0, 200, 0, 255]
        } else {
            [0, 0, 220, 255]
        }
    })
}

fn assert_square(bytes: &[u8], side: usize, what: &str) {
    let image = avatars::decode(bytes).unwrap_or_else(|| panic!("{what} was refused"));
    assert_eq!(image.size, [side, side], "{what}");
    assert!(side <= 96, "{what}");
}

fn rgba_at(image: &ColorImage, x: usize, y: usize) -> [u8; 4] {
    image.pixels[y * image.size[0] + x].to_array()
}

/// Center-crop keeps the middle band. A stretch would paint the ends red and blue.
fn assert_middle_band(image: &ColorImage, vertical: bool, what: &str) {
    let (width, height) = (image.size[0], image.size[1]);
    let samples = if vertical {
        [
            (width / 2, height / 4),
            (width / 2, height / 2),
            (width / 2, 3 * height / 4),
        ]
    } else {
        [
            (width / 4, height / 2),
            (width / 2, height / 2),
            (3 * width / 4, height / 2),
        ]
    };
    for (x, y) in samples {
        let [red, green, blue, alpha] = rgba_at(image, x, y);
        assert!(
            green > 180 && red < 40 && blue < 40 && alpha > 200,
            "{what} pixel {x},{y} is {red},{green},{blue},{alpha}; expected the middle band"
        );
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

/// Signature plus one IHDR. No pixel buffer, so a huge size must be refused
/// from the header.
fn png_ihdr(width: u32, height: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(17);
    body.extend(b"IHDR");
    body.extend(width.to_be_bytes());
    body.extend(height.to_be_bytes());
    body.extend([8, 6, 0, 0, 0]);
    let crc = crc32(&body);
    let mut file = vec![137, 80, 78, 71, 13, 10, 26, 10];
    let data_len = u32::try_from(body.len() - 4).expect("ihdr length");
    file.extend(data_len.to_be_bytes());
    file.extend(&body);
    file.extend(crc.to_be_bytes());
    file
}

fn assert_crc_matches_real_png() {
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926, "crc32 test vector");
    let encoded = solid(1, 1, ImageFormat::Png);
    assert!(encoded.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]));
    assert_eq!(&encoded[12..16], b"IHDR");
    let stored = u32::from_be_bytes(encoded[29..33].try_into().expect("crc bytes"));
    assert_eq!(stored, crc32(&encoded[12..29]), "IHDR crc");
}

#[test]
fn decode_squares_png_and_jpeg() {
    for format in raster_formats() {
        let name = format!("{format:?}");
        assert_square(&solid(48, 48, format), 48, &format!("{name} 48"));
        assert_square(&solid(96, 96, format), 96, &format!("{name} 96"));
        assert_square(&solid(97, 97, format), 96, &format!("{name} 97"));
        assert_square(&solid(180, 90, format), 90, &format!("{name} 180x90"));
        assert_square(&solid(1024, 16, format), 16, &format!("{name} 1024x16"));
        assert_square(&solid(16, 1024, format), 16, &format!("{name} 16x1024"));
        let wide = banded(120, 40, format, false);
        assert_square(&wide, 40, &format!("{name} wide"));
        let tall = banded(40, 120, format, true);
        assert_square(&tall, 40, &format!("{name} tall"));
        if format != ImageFormat::Jpeg {
            assert_middle_band(
                &avatars::decode(&wide).expect("wide"),
                false,
                &format!("{name} wide crop"),
            );
            assert_middle_band(
                &avatars::decode(&tall).expect("tall"),
                true,
                &format!("{name} tall crop"),
            );
        }
    }
}

#[test]
fn decode_refuses_oversize_corrupt_and_empty() {
    for format in raster_formats() {
        let name = format!("{format:?}");
        assert!(
            avatars::decode(&solid(1025, 16, format)).is_none(),
            "{name} wide"
        );
        assert!(
            avatars::decode(&solid(16, 1025, format)).is_none(),
            "{name} tall"
        );
        let bytes = solid(32, 24, format);
        assert!(
            avatars::decode(&bytes[..bytes.len() / 3]).is_none(),
            "{name} truncated"
        );
        // A one-byte JPEG change can still decode. Wipe the payload after the
        // header so the container is no longer a picture.
        let mut corrupt = bytes.clone();
        let keep = 4.min(corrupt.len());
        for byte in &mut corrupt[keep..] {
            *byte = 0;
        }
        assert!(avatars::decode(&corrupt).is_none(), "{name} corrupt");
    }
    assert!(avatars::decode(b"").is_none(), "empty");
    assert!(avatars::decode(b"not an image").is_none());
    assert!(avatars::decode(b"GIF89a").is_none(), "gif header only");
    assert!(
        avatars::decode(b"\xff\xd8\xff").is_none(),
        "jpeg header only"
    );
    assert!(
        avatars::decode(&[137, 80, 78, 71, 13, 10, 26, 10]).is_none(),
        "png signature only"
    );
    assert!(avatars::decode(&[0, 1, 2, 3, 4, 5, 6, 7]).is_none());
}

#[test]
fn decode_refuses_a_huge_png_header_quickly() {
    assert_crc_matches_real_png();
    let header = png_ihdr(100_000, 100_000);
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let started = Instant::now();
        let refused = avatars::decode(&header).is_none();
        let _ = sender.send((refused, started.elapsed()));
    });
    match receiver.recv_timeout(Duration::from_secs(2)) {
        Ok((true, elapsed)) => assert!(
            elapsed < Duration::from_secs(1),
            "100000x100000 png header took {elapsed:?}"
        ),
        Ok((false, elapsed)) => panic!("huge png header decoded in {elapsed:?}"),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("huge png header decode did not return within 2s")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("huge png header decode panicked"),
    }
}

/// 1×1 white GIF89a. LZW minimum code size 2: clear (4), index 0, end (5),
/// packed little-endian into `44 01`.
fn gif_1x1() -> Vec<u8> {
    vec![
        b'G', b'I', b'F', b'8', b'9', b'a', 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0xff, 0xff,
        0xff, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02,
        0x02, 0x44, 0x01, 0x00, 0x3b,
    ]
}

#[test]
fn decode_refuses_gif() {
    // `decode` keeps only PNG and JPEG, even for a well-formed GIF89a.
    let bytes = gif_1x1();
    assert!(bytes.starts_with(b"GIF89a"));
    assert!(avatars::decode(&bytes).is_none());
}

/// Combining marks do not start their own grapheme. Enough for the uppercase
/// expansions `initials` actually produces.
fn grapheme_len(text: &str) -> usize {
    let mut count = 0;
    for c in text.chars() {
        if is_combining(c) {
            if count == 0 {
                count = 1;
            }
        } else {
            count += 1;
        }
    }
    count
}

fn is_combining(c: char) -> bool {
    matches!(
        c,
        '\u{0300}'..='\u{036F}'
            | '\u{1AB0}'..='\u{1AFF}'
            | '\u{1DC0}'..='\u{1DFF}'
            | '\u{20D0}'..='\u{20FF}'
            | '\u{3099}'..='\u{309A}'
            | '\u{FE20}'..='\u{FE2F}'
    )
}

#[test]
fn grapheme_counter_keeps_a_mark_on_its_base() {
    assert_eq!(grapheme_len("AV"), 2);
    assert_eq!(grapheme_len("A\u{0301}"), 1);
    assert_eq!(grapheme_len("FFI"), 3);
    assert_eq!(grapheme_len(""), 0);
}

#[test]
fn initials_keep_one_scalar_when_uppercase_would_expand() {
    // `single_upper` leaves ß, ﬁ, ﬀ, ﬃ and ﬄ as written, so two words stay
    // two graphemes instead of SS / FFI / FFL.
    assert_eq!(avatars::initials(&Person::new("Ada Voss", "a@b")), "AV");
    assert_eq!(avatars::initials(&Person::new("ß", "a@b")), "ß");
    assert_eq!(avatars::initials(&Person::new("ﬁ", "a@b")), "ﬁ");
    assert_eq!(avatars::initials(&Person::new("ﬀ", "a@b")), "ﬀ");
    assert_eq!(avatars::initials(&Person::new("ŉ", "")), "ŉ");
    assert_eq!(avatars::initials(&Person::new("ﬃ", "")), "ﬃ");
    assert_eq!(avatars::initials(&Person::new("ﬄ", "")), "ﬄ");
    assert_eq!(avatars::initials(&Person::new("ß ß", "")), "ßß");
    assert_eq!(avatars::initials(&Person::new("ßeta Smith", "")), "ßS");
    assert_eq!(avatars::initials(&Person::new("ﬁ ﬂ", "")), "ﬁﬂ");
    assert_eq!(
        avatars::initials(&Person::new("!!!", "ﬃ@example.test")),
        "ﬃ"
    );
    assert_eq!(avatars::initials(&Person::new("", "")), "?");
    for name in ["Ada Voss", "ß ß", "ﬃ", "ﬁ ﬂ", ""] {
        let text = avatars::initials(&Person::new(name, "a@b.test"));
        let graphemes = grapheme_len(&text);
        assert!(
            (1..=2).contains(&graphemes) && text.chars().count() <= 2,
            "{name:?} -> {text:?} ({graphemes} graphemes)"
        );
    }
}

proptest! {
    #![proptest_config(proptest_config())]

    #[test]
    fn initials_are_never_empty_and_at_most_two_graphemes(
        name in arb_text(24),
        email in arb_text(24),
    ) {
        let text = avatars::initials(&Person::new(&name, &email));
        prop_assert!(!text.is_empty(), "{name:?} {email:?}");
        prop_assert!(text.chars().count() <= 2, "{name:?} {email:?} -> {text:?}");
        prop_assert!(
            grapheme_len(&text) <= 2,
            "{name:?} {email:?} -> {text:?} ({} graphemes)",
            grapheme_len(&text)
        );
    }
}

fn is_github_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn assert_issue_segments(text: &str) {
    let segments = hover::issue_segments(text);
    let rebuilt: String = segments
        .iter()
        .map(|segment| match segment {
            Segment::Text(part) | Segment::Issue { label: part, .. } => *part,
        })
        .collect();
    assert_eq!(rebuilt, text, "{segments:?}");
    for segment in segments {
        let Segment::Issue {
            label,
            owner,
            repo,
            number,
        } = segment
        else {
            continue;
        };
        assert!(number >= 1, "{text:?} {label:?}");
        let digits = number.to_string();
        assert!(
            label.ends_with(&digits),
            "{label:?} does not end with {digits}"
        );
        let trailing: String = label
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        assert_eq!(trailing, digits, "{text:?} {label:?}");
        match (owner, repo) {
            (None, None) => {}
            (Some(owner), Some(repo)) => {
                assert!(is_github_name(owner), "{owner:?}");
                assert!(is_github_name(repo), "{repo:?}");
                assert!(
                    label.contains(&format!("{owner}/{repo}")),
                    "{label:?} missing {owner}/{repo}"
                );
            }
            other => panic!("owner/repo pair {other:?} in {text:?}"),
        }
    }
}

#[test]
fn issue_segments_cover_markers_unicode_and_slashes() {
    for text in [
        "",
        "#",
        "GH-",
        "#1",
        "GH-2",
        "#0",
        "#012",
        "GH-0",
        "C#",
        "##",
        "ünïcode #5 ✓",
        "Fix #12 and GH-7, see octo/repo#3.",
        "a/b/c#1",
        "owner_name/repo.name#10",
        "octo/repoGH-3",
        "GH-GH-1",
        "foo#12bar",
        "#12_3",
        "ü#9",
        "/#4",
        "#99999999999999999999999",
        "end #8",
    ] {
        assert_issue_segments(text);
    }
}

proptest! {
    #![proptest_config(proptest_config())]

    #[test]
    fn issue_segments_cover_every_string(text in arb_issue_text()) {
        assert_issue_segments(&text);
    }
}

fn time_rank(text: &str) -> (u8, u64) {
    if text == "just now" {
        return (0, 0);
    }
    let mut words = text.split(' ');
    let count: u64 = words
        .next()
        .and_then(|word| word.parse().ok())
        .unwrap_or_else(|| panic!("no count in {text:?}"));
    let unit = words
        .next()
        .unwrap_or_else(|| panic!("no unit in {text:?}"));
    let ago = words.next().unwrap_or_else(|| panic!("no ago in {text:?}"));
    assert_eq!(ago, "ago", "{text:?}");
    assert!(words.next().is_none(), "trailing words in {text:?}");
    let rank = match unit {
        "minute" | "minutes" => 1,
        "hour" | "hours" => 2,
        "day" | "days" => 3,
        "week" | "weeks" => 4,
        "month" | "months" => 5,
        "year" | "years" => 6,
        other => panic!("unknown unit {other} in {text:?}"),
    };
    if count == 1 {
        assert!(!unit.ends_with('s'), "plural for one: {text:?}");
    } else {
        assert!(unit.ends_with('s'), "singular for {count}: {text:?}");
    }
    (rank, count)
}

fn assert_monotonic_seconds(start: i64, end: i64) {
    assert!(start <= end);
    let mut previous = time_rank(&hover::relative_time(start, 0));
    let mut seconds = start;
    while seconds < end {
        seconds += 1;
        let rank = time_rank(&hover::relative_time(seconds, 0));
        assert!(
            previous <= rank,
            "at {seconds}s, {} {previous:?} then {} {rank:?}",
            hover::relative_time(seconds - 1, 0),
            hover::relative_time(seconds, 0),
        );
        previous = rank;
    }
}

#[test]
fn relative_time_larger_differences_do_not_shrink() {
    assert_eq!(hover::relative_time(0, 0), "just now");
    assert_eq!(hover::relative_time(10, 10_000), "just now");
    assert_eq!(hover::relative_time(i64::MIN, i64::MAX), "just now");
    let huge = time_rank(&hover::relative_time(i64::MAX, 0));
    assert_eq!(huge.0, 6, "i64::MAX should be years, got {huge:?}");
    assert!(huge.1 >= 1);
    assert_eq!(time_rank(&hover::relative_time(i64::MAX, i64::MIN)), huge);

    // Every second across the minute/hour/day/week boundaries, the month
    // boundary (30 days) and the year boundary (365 days). Unit rank is the
    // primary key, so "1 week" is after "6 days" and "1 year" is after
    // "12 months".
    assert_monotonic_seconds(0, 8 * DAY_SECS);
    assert_monotonic_seconds(28 * DAY_SECS, 32 * DAY_SECS);
    assert_monotonic_seconds(360 * DAY_SECS, 370 * DAY_SECS);

    let mut previous = time_rank(&hover::relative_time(0, 0));
    for day in 1..=30 * 365 {
        let rank = time_rank(&hover::relative_time(day * DAY_SECS, 0));
        assert!(
            previous <= rank,
            "day {day}: {previous:?} then {rank:?} {}",
            hover::relative_time(day * DAY_SECS, 0)
        );
        previous = rank;
    }
}

proptest! {
    #![proptest_config(proptest_config())]

    #[test]
    fn relative_time_orders_any_two_gaps(now in any::<i64>(), left in any::<i64>(), right in any::<i64>()) {
        let left = left.saturating_abs();
        let right = right.saturating_abs();
        let (small, large) = if left <= right { (left, right) } else { (right, left) };
        let earlier = time_rank(&hover::relative_time(now, now.saturating_sub(small)));
        let later = time_rank(&hover::relative_time(now, now.saturating_sub(large)));
        prop_assert!(
            earlier <= later,
            "now={now} gap {small} -> {} {earlier:?}, gap {large} -> {} {later:?}",
            hover::relative_time(now, now.saturating_sub(small)),
            hover::relative_time(now, now.saturating_sub(large)),
        );
    }
}

fn for_each_node(harness: &Harness<'_, ()>, mut visit: impl FnMut(&egui_kittest::Node<'_>)) {
    let root = harness.root();
    visit(&root);
    for node in root.children_recursive() {
        visit(&node);
    }
}

fn node_strings(node: &egui_kittest::Node<'_>) -> Vec<String> {
    let access = node.accesskit_node();
    let mut texts = Vec::new();
    if let Some(label) = access.label() {
        texts.push(label);
    }
    if let Some(value) = access.value() {
        texts.push(value);
    }
    if let Some(description) = access.description() {
        texts.push(description);
    }
    if let Some(url) = access.url() {
        texts.push(url.to_owned());
    }
    if let Some(placeholder) = access.placeholder() {
        texts.push(placeholder.to_owned());
    }
    texts
}

fn tree_contains(harness: &Harness<'_, ()>, needle: &str) -> bool {
    let mut found = false;
    for_each_node(harness, |node| {
        if node_strings(node).iter().any(|text| text.contains(needle)) {
            found = true;
        }
    });
    found
}

fn label_dump(harness: &Harness<'_, ()>) -> String {
    let mut lines = Vec::new();
    for_each_node(harness, |node| {
        let role = node.accesskit_node().role();
        for text in node_strings(node) {
            lines.push(format!("{role:?} {text}"));
        }
    });
    lines.join("\n")
}

fn click_label(harness: &mut Harness<'_, ()>, label: &str) {
    {
        harness.get_by_label(label).click();
    }
    harness.step();
}

fn password_field<'a>(harness: &'a Harness<'_, ()>) -> egui_kittest::Node<'a> {
    harness
        .query_by_role(Role::PasswordInput)
        .unwrap_or_else(|| harness.get_by_role(Role::TextInput))
}

fn password_display(harness: &Harness<'_, ()>) -> String {
    let field = password_field(harness);
    let own = field.value().unwrap_or_default();
    if !own.is_empty() {
        return own;
    }
    field.children().filter_map(|child| child.value()).collect()
}

fn action_label(action: &Action) -> String {
    match action {
        Action::Browse => "Browse".to_owned(),
        Action::Open(path) => format!("Open({})", path.display()),
        Action::Home => "Home".to_owned(),
        Action::OpenSettings => "OpenSettings".to_owned(),
        Action::CloseSettings => "CloseSettings".to_owned(),
        Action::ToggleBranchesSidebar => "ToggleBranchesSidebar".to_owned(),
        Action::ToggleDetailSidebar => "ToggleDetailSidebar".to_owned(),
        Action::ToggleTerminal => "ToggleTerminal".to_owned(),
        Action::SetTerminalFont(name) => format!("SetTerminalFont({name:?})"),
        Action::EnsureFontCatalog => "EnsureFontCatalog".to_owned(),
        Action::Fetch(upstream) => format!("Fetch({upstream})"),
        Action::Pull => "Pull".to_owned(),
        Action::Checkout(name) => format!("Checkout({name})"),
        Action::SetHistoryColumns(_) => "SetHistoryColumns".to_owned(),
        Action::SetAuthorAvatars(on) => format!("SetAuthorAvatars({on})"),
    }
}

/// The settings harness, the GitHub Auth state it edits, and the actions it returned.
type SettingsHarness = (
    Harness<'static, ()>,
    Rc<RefCell<GitHubAuth>>,
    Rc<RefCell<Vec<Action>>>,
);

fn open_settings_with(about: AboutInfo) -> SettingsHarness {
    let github = Rc::new(RefCell::new(GitHubAuth::default()));
    let github_ui = Rc::clone(&github);
    let actions = Rc::new(RefCell::new(Vec::new()));
    let actions_ui = Rc::clone(&actions);
    let fonts = FontService::default();
    let mut preview = None;
    let view = SettingsView {
        author_avatars: true,
        api_status: ApiStatus::default(),
        busy: false,
        about,
    };
    let harness = Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 1600.0))
        .with_pixels_per_point(1.0)
        .build_ui(move |ui| {
            theme::apply(ui.ctx(), theme::DEFAULT_THEME);
            let produced = settings::show(
                ui,
                None,
                &fonts,
                &mut preview,
                None,
                &view,
                &mut github_ui.borrow_mut(),
            );
            *actions_ui.borrow_mut() = produced;
        });
    (harness, github, actions)
}

fn open_settings() -> SettingsHarness {
    open_settings_with(AboutInfo::current("2.43.0"))
}

/// egui appends [`OutputCommand`]s across passes. A click is the suffix when
/// the previous commands are still in front; otherwise the frame replaced them.
fn commands_since<'a>(
    previous: &[OutputCommand],
    harness: &'a Harness<'_, ()>,
) -> &'a [OutputCommand] {
    let commands = &harness.output().platform_output.commands;
    commands.strip_prefix(previous).unwrap_or(commands)
}

fn opened_url<'a>(commands: &'a [OutputCommand], url: &str) -> Option<&'a str> {
    commands.iter().find_map(|command| match command {
        OutputCommand::OpenUrl(open) if open.url == url => Some(open.url.as_str()),
        _ => None,
    })
}

fn assert_no_token_material(text: &str) {
    for needle in ["?", "@", "ghp_", "github_pat_", FAKE_TOKEN] {
        assert!(!text.contains(needle), "{text:?} contains {needle}");
    }
}

#[test]
fn settings_hides_the_token_and_history_toggles_avatars() {
    assert_eq!(FAKE_TOKEN.len(), 40);
    assert_eq!(&FAKE_TOKEN[..4], "ghp_");
    assert_eq!(FAKE_TOKEN.len() - 4, 36);

    let (mut harness, github, actions) = open_settings();
    // Save, Verify, Remove, Import and Use GH_TOKEN all leave the process.
    // This test only types into the masked field.
    click_label(&mut harness, "GitHub Auth");
    assert!(
        harness.query_by_label_contains("Paste it here").is_some(),
        "GitHub Auth did not open\n{}",
        label_dump(&harness)
    );

    // The readable name is the requirement. egui exposes this hyperlink as a
    // label node rather than Role::Link.
    harness.get_by_label(TOKEN_LINK);
    for feature in POSSIBLE_LATER {
        assert!(
            harness.query_by_label_contains(feature).is_some(),
            "missing {feature}\n{}",
            label_dump(&harness)
        );
    }
    let bullets = harness
        .query_all_by(|node: &egui_kittest::kittest::AccessKitNode<'_>| {
            node.role() == Role::Label && node.value().is_some_and(|text| text.starts_with("• "))
        })
        .count();
    assert_eq!(bullets, 5, "possible-later items\n{}", label_dump(&harness));

    assert!(
        harness.query_by_role(Role::PasswordInput).is_some(),
        "token field is not a password input\n{}",
        label_dump(&harness)
    );
    {
        password_field(&harness).click();
    }
    harness.step();
    {
        password_field(&harness).type_text(FAKE_TOKEN);
    }
    harness.step();

    {
        let auth = github.borrow();
        assert_eq!(auth.draft, FAKE_TOKEN, "the field did not take the token");
        assert!(!auth.busy(), "typing started a keychain or network task");
        assert!(!auth.has_token(), "typing stored a token");
        assert_eq!(auth.stored(), Stored::None);
        assert!(auth.notice().is_none(), "typing produced a notice");
    }
    let shown = password_display(&harness);
    assert_eq!(
        shown.chars().count(),
        FAKE_TOKEN.chars().count(),
        "password display {shown:?}"
    );
    assert!(
        shown.chars().all(|c| c == '\u{2022}'),
        "password display {shown:?}"
    );
    assert!(
        !tree_contains(&harness, FAKE_TOKEN),
        "token leaked into the accessibility tree\n{}",
        label_dump(&harness)
    );

    click_label(&mut harness, "History");
    assert!(
        !tree_contains(&harness, FAKE_TOKEN),
        "token leaked on the History page\n{}",
        label_dump(&harness)
    );
    {
        let checkbox = harness.get_by_role_and_label(Role::CheckBox, AVATAR_CHECKBOX);
        assert_eq!(checkbox.accesskit_node().toggled(), Some(Toggled::True));
        checkbox.click();
    }
    harness.step();
    let got: Vec<_> = actions.borrow().iter().map(action_label).collect();
    assert_eq!(got, vec!["SetAuthorAvatars(false)".to_owned()], "{got:?}");
}

#[test]
fn about_shows_the_commit_and_a_repository_url_without_a_token() {
    // Fixed strings. AboutInfo::current would embed the real build commit.
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    let version = env!("CARGO_PKG_VERSION");
    let version_date = "2026-10-24";
    let license = env!("CARGO_PKG_LICENSE");
    let git_version = "2.43.0";
    let repository = env!("CARGO_PKG_REPOSITORY");
    let releases = format!("{repository}/releases");
    assert_eq!(repository, "https://github.com/don-linux/Cthulhu-Git");
    assert_eq!(
        releases,
        "https://github.com/don-linux/Cthulhu-Git/releases"
    );
    assert_no_token_material(repository);
    assert_no_token_material(&releases);

    let (mut harness, github, _actions) = open_settings_with(AboutInfo {
        version: version.to_owned(),
        version_date: version_date.to_owned(),
        commit: COMMIT.to_owned(),
        license: license.to_owned(),
        description: env!("CARGO_PKG_DESCRIPTION").to_owned(),
        repository: repository.to_owned(),
        git_version: git_version.to_owned(),
    });

    click_label(&mut harness, "GitHub Auth");
    assert!(
        harness.query_by_label_contains("Paste it here").is_some(),
        "GitHub Auth did not open\n{}",
        label_dump(&harness)
    );
    {
        password_field(&harness).click();
    }
    harness.step();
    {
        password_field(&harness).type_text(FAKE_TOKEN);
    }
    harness.step();
    {
        let auth = github.borrow();
        assert_eq!(auth.draft, FAKE_TOKEN, "the field did not take the token");
        assert!(!auth.busy(), "typing started a keychain or network task");
        assert!(!auth.has_token(), "typing stored a token");
        assert_eq!(auth.stored(), Stored::None);
        assert!(auth.notice().is_none(), "typing produced a notice");
    }

    click_label(&mut harness, "About");
    assert_eq!(
        github.borrow().draft,
        FAKE_TOKEN,
        "leaving GitHub Auth dropped the typed token"
    );

    let mut tops = Vec::new();
    for label in ["Terminal", "History", "GitHub Auth", "About"] {
        tops.push(harness.get_by_label(label).rect().top());
    }
    let about_top = tops[3];
    assert!(
        tops[..3].iter().all(|top| about_top > *top),
        "About top {about_top}, nav tops {tops:?}"
    );

    for label in [
        version,
        version_date,
        COMMIT,
        license,
        git_version,
        repository,
    ] {
        harness.get_by_label(label);
    }

    // node_strings reads access.url() while walking every node. A link target
    // that shows up there has to be the repository or the releases page.
    let mut urls = Vec::new();
    for_each_node(&harness, |node| {
        if let Some(url) = node.accesskit_node().url() {
            urls.push(url.to_owned());
        }
    });
    for url in &urls {
        assert!(
            url == repository || url == &releases,
            "access url {url:?} is not the repository or the releases page\n{urls:?}"
        );
        assert_no_token_material(url);
    }

    let previous = harness.output().platform_output.commands.clone();
    click_label(&mut harness, repository);
    let fresh = commands_since(&previous, &harness);
    let opened = opened_url(fresh, repository).unwrap_or_else(|| {
        panic!(
            "no OpenUrl for the repository\nfresh: {fresh:?}\nall: {:?}",
            harness.output().platform_output.commands
        )
    });
    assert_no_token_material(opened);

    let previous = harness.output().platform_output.commands.clone();
    click_label(&mut harness, &releases);
    let fresh = commands_since(&previous, &harness);
    let opened = opened_url(fresh, &releases).unwrap_or_else(|| {
        panic!(
            "no new OpenUrl for releases\nfresh: {fresh:?}\nall: {:?}",
            harness.output().platform_output.commands
        )
    });
    assert_no_token_material(opened);

    assert!(
        !tree_contains(&harness, FAKE_TOKEN),
        "token leaked on the About page\n{}",
        label_dump(&harness)
    );

    let previous = harness.output().platform_output.commands.clone();
    click_label(&mut harness, "Copy version info");
    let fresh = commands_since(&previous, &harness);
    let copied = fresh.iter().find_map(|command| match command {
        OutputCommand::CopyText(text)
            if text.contains(version)
                && text.contains(COMMIT)
                && text.contains("https://github.com/don-linux/Cthulhu-Git") =>
        {
            Some(text.as_str())
        }
        _ => None,
    });
    let copied = copied
        .unwrap_or_else(|| panic!("CopyText missing version, commit, or repository\n{fresh:?}"));
    assert_no_token_material(copied);
}

#[test]
fn terminal_font_field_is_half_the_column_and_centered_beside_browse() {
    let (harness, _github, _actions) = open_settings();
    let title = harness.get_by_label("Terminal Font").rect();
    let field = harness.get_by_role(Role::TextInput).rect();
    let browse = harness.get_by_label("Browse fonts").rect();
    assert!(
        (field.width() - 390.0).abs() < 2.0,
        "font field width {:.1}\n{}",
        field.width(),
        label_dump(&harness)
    );
    let gap = browse.left() - field.right();
    assert!(
        (0.0..16.0).contains(&gap),
        "browse gap {gap:.1} (field {field:?}, browse {browse:?})"
    );
    let pair_center = (field.left() + browse.right()) / 2.0;
    assert!(
        (pair_center - title.center().x).abs() < 4.0,
        "pair center {pair_center:.1}, title center {:.1}",
        title.center().x
    );
}

#[test]
fn about_puts_the_copyright_opposite_copy_version_info() {
    let (mut harness, _github, _actions) = open_settings();
    click_label(&mut harness, "About");
    let title = harness.get_by_label("Cthulhu-Git").rect();
    let logo = harness.get_by_label("Cthulhu-Git logo").rect();
    let subtitle = harness.get_by_label(env!("CARGO_PKG_DESCRIPTION")).rect();
    assert!(
        logo.top() > title.bottom() && subtitle.top() > logo.bottom(),
        "logo should sit between the title and the description ({title:?}, {logo:?}, {subtitle:?})"
    );
    assert!(
        (logo.width() - 128.0).abs() < 2.0 && (logo.center().x - title.center().x).abs() < 4.0,
        "logo {logo:?} title {title:?}"
    );
    let notice = harness.get_by_label("© 2026 Don Linux").rect();
    let copy = harness.get_by_label("Copy version info").rect();
    let version = harness.get_by_label("Version").rect();
    let releases = format!("{}/releases", env!("CARGO_PKG_REPOSITORY"));
    let releases_link = harness.get_by_label(&releases).rect();
    let below_links = notice.top() - releases_link.bottom();
    assert!(
        (0.0..48.0).contains(&below_links),
        "copyright sits {below_links:.1}px under the releases link ({notice:?}, {releases_link:?})"
    );
    assert!(
        (notice.left() - version.left()).abs() < 2.0,
        "copyright {:?} version {:?}",
        notice,
        version
    );
    assert!(
        copy.left() > notice.right() + 100.0,
        "copy {:?} should sit across from {:?}",
        copy,
        notice
    );
    assert!(
        (copy.center().y - notice.center().y).abs() < 16.0,
        "copy and copyright are not on the same row ({copy:?}, {notice:?})"
    );
}

fn hover_repo() -> OpenedRepo {
    let oid = "ab".repeat(20);
    let author = Person::new(HOVER_AUTHOR, "ada@miskatonic.test");
    let coauthor = Person::new(HOVER_COAUTHOR, "west@arkham.test");
    let commits = [GraphCommit {
        oid: oid.clone(),
        summary: HOVER_SUMMARY.to_owned(),
        parents: Vec::new(),
        author: author.clone(),
        co_authors: vec![coauthor.clone()],
    }];
    let tips = vec![GraphTip {
        name: "main".to_owned(),
        oid: oid.clone(),
        color: 0,
        principal: true,
        current: true,
    }];
    let graph = layout_graph(&commits, &tips);
    OpenedRepo {
        info: RepoInfo {
            root: PathBuf::from("/cthulhu-agent-network-missing/repo"),
            name: "missing-repo".to_owned(),
            head: Head::Branch("main".to_owned()),
        },
        graph,
        graph_selection: tips.clone(),
        graph_requested: tips,
        detached_oid: None,
        branches: vec![ListedBranch {
            branch: Branch {
                name: "main".to_owned(),
                oid: Some(oid.clone()),
                committer_time: Some(1_700_000_000),
                commit_count: 1,
                upstream: Upstream::None,
                current: true,
            },
            checked: true,
        }],
        latest: Some(CommitDetail {
            oid,
            author,
            co_authors: vec![coauthor],
            authored_at: "2026-01-01 00:00".to_owned(),
            message: HOVER_SUMMARY.to_owned(),
        }),
        view_id: egui::Id::new("agent-network-hover"),
        terminal: None,
        github: None,
        hover: HoverCache::default(),
    }
}

fn hover_is_loading_or_error(text: &str) -> bool {
    text.contains("Loading…")
        || text.contains("Could not run git")
        || text.contains("Git was not found")
        || text.contains("is not a directory")
        || text.contains("is not inside a Git repository")
        || text.contains("No such file")
        || text.contains("failed (")
}

#[test]
fn history_hover_card_names_the_author_and_coauthor() {
    let mut repo = hover_repo();
    assert_eq!(repo.graph.rows.len(), 1);
    assert_eq!(repo.graph.rows[0].summary, HOVER_SUMMARY);
    assert_eq!(repo.graph.rows[0].author.name, HOVER_AUTHOR);
    assert_eq!(repo.graph.rows[0].co_authors.len(), 1);
    assert_eq!(repo.graph.rows[0].co_authors[0].name, HOVER_COAUTHOR);

    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(1500.0, 640.0))
        .with_pixels_per_point(1.0)
        .build_ui(move |ui| {
            theme::apply(ui.ctx(), theme::DEFAULT_THEME);
            let _ = repo_view::show(
                ui,
                &mut repo,
                &repo_view::ViewState {
                    error: None,
                    branches_open: false,
                    detail_open: false,
                    terminal_open: false,
                    syncing: None,
                    columns: HistoryColumns::default(),
                    history_loading: false,
                    busy: false,
                },
                &mut AvatarService::disabled(),
            );
        });

    let author_label = format!("{HOVER_AUTHOR}, {HOVER_COAUTHOR}");
    {
        let cells: Vec<_> = harness.query_all_by_label(HOVER_SUMMARY).collect();
        assert!(
            !cells.is_empty(),
            "message cell missing\n{}",
            label_dump(&harness)
        );
        let cell = cells
            .into_iter()
            .min_by(|left, right| left.rect().left().total_cmp(&right.rect().left()))
            .expect("message cell");
        cell.hover();
    }
    harness.run_steps(60);

    let message_hover = label_dump(&harness);
    assert!(
        !message_hover.contains("Co-author") && !message_hover.contains("Loading…"),
        "message hover opened the card\n{message_hover}"
    );

    {
        let cells: Vec<_> = harness.query_all_by_label(&author_label).collect();
        assert!(
            !cells.is_empty(),
            "author cell missing\n{}",
            label_dump(&harness)
        );
        let cell = cells
            .into_iter()
            .min_by(|left, right| left.rect().left().total_cmp(&right.rect().left()))
            .expect("author cell");
        cell.hover();
    }
    harness.run_steps(60);

    let text = label_dump(&harness);
    assert!(text.contains(HOVER_AUTHOR), "{text}");
    assert!(text.contains(HOVER_COAUTHOR), "{text}");
    assert!(text.contains("Co-author"), "{text}");
    assert!(
        hover_is_loading_or_error(&text),
        "card showed neither loading nor an error\n{text}"
    );
}

#[test]
fn disabled_avatar_service_returns_no_texture() {
    // `jobs` and `entries` are private, so a queued fetch can only be seen
    // from inside `avatars`. The public contract is that `texture` stays
    // `None` and `poll` never produces one.
    let ctx = egui::Context::default();
    let mut service = AvatarService::disabled();
    let people = [
        Person::new(HOVER_AUTHOR, "ada@miskatonic.test"),
        Person::new("", ""),
        Person::new("No Address", "   "),
        Person::new("", "only@email.test"),
        Person {
            name: "Alias".to_owned(),
            email: String::new(),
            aliases: vec!["alias@example.test".to_owned()],
        },
    ];
    for person in &people {
        assert!(service.texture(&ctx, person, None, None).is_none());
        assert!(
            service
                .texture(&ctx, person, None, Some(&"ab".repeat(20)))
                .is_none()
        );
    }
    service.poll(&ctx);
    for person in &people {
        assert!(
            service.texture(&ctx, person, None, None).is_none(),
            "{}",
            person.email
        );
    }
}
