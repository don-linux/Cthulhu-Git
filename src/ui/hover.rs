//! The card shown while the pointer rests on a commit, modeled on VS Code's
//! Source Control Graph hover: author, date, co-authors, the full message,
//! branches, the short stat, the hash and a link to GitHub.
//!
//! The details come from one `git show` per commit, run on a worker thread
//! the first time that commit's card opens.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use cthulhu_git::git::{self, CommitHover, Git, GraphRow, Person};
use cthulhu_git::github::{GitHubRepo, RepoContext};
use eframe::egui::{self, Button, Context, CursorIcon, RichText, Sense, Ui, Vec2};

use super::avatars::{self, AvatarService};
use super::theme::Palette;

const CARD_WIDTH: f32 = 440.0;
const AUTHOR_AVATAR: f32 = 40.0;
const CO_AUTHOR_AVATAR: f32 = 22.0;
/// Hovered commits whose details are kept, so the cache cannot grow without bound.
const MAX_ENTRIES: usize = 256;

enum State {
    Loading,
    Ready(Box<CommitHover>),
    Failed(String),
}

/// Details already read, and the one `git show` in flight.
#[derive(Default)]
pub struct HoverCache {
    entries: HashMap<String, State>,
    order: Vec<String>,
    inflight: Option<(String, Receiver<Result<CommitHover, String>>)>,
    /// Asked for while another read was running. Only the newest is kept.
    wanted: Option<String>,
}

impl HoverCache {
    /// Collects a finished read and starts the next one.
    pub fn poll(&mut self, ctx: &Context, root: &Path) {
        if let Some((oid, receiver)) = &self.inflight {
            let result = match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("The commit details stopped unexpectedly.".to_owned()))
                }
            };
            if let Some(result) = result {
                let state = match result {
                    Ok(hover) => State::Ready(Box::new(hover)),
                    Err(message) => State::Failed(message),
                };
                let oid = oid.clone();
                self.inflight = None;
                self.insert(oid, state);
            }
        }
        if self.inflight.is_none()
            && let Some(oid) = self.wanted.take()
        {
            self.start(ctx, root, oid);
        }
    }

    fn insert(&mut self, oid: String, state: State) {
        if !self.entries.contains_key(&oid) {
            self.order.push(oid.clone());
        }
        self.entries.insert(oid, state);
        while self.order.len() > MAX_ENTRIES {
            let old = self.order.remove(0);
            self.entries.remove(&old);
        }
    }

    fn request(&mut self, oid: &str) {
        let busy_with_it = self
            .inflight
            .as_ref()
            .is_some_and(|(inflight, _)| inflight == oid);
        if self.entries.contains_key(oid) || busy_with_it {
            return;
        }
        self.wanted = Some(oid.to_owned());
    }

    fn start(&mut self, ctx: &Context, root: &Path, oid: String) {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        let root: PathBuf = root.to_path_buf();
        let worker_oid = oid.clone();
        thread::spawn(move || {
            let result = Git::discover()
                .and_then(|git| git::commit_hover(&git, &root, &worker_oid))
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
            ctx.request_repaint();
        });
        self.entries.insert(oid.clone(), State::Loading);
        self.inflight = Some((oid, receiver));
    }
}

/// What the card needs besides the row.
pub struct CardContext<'a> {
    pub palette: &'a Palette,
    pub github: Option<&'a GitHubRepo>,
    pub repo: Option<&'a RepoContext>,
}

pub fn card(
    ui: &mut Ui,
    row: &GraphRow,
    cache: &mut HoverCache,
    avatars: &mut AvatarService,
    context: &CardContext<'_>,
) {
    cache.request(&row.oid);
    let palette = context.palette;
    ui.set_max_width(CARD_WIDTH);
    ui.spacing_mut().item_spacing.y = 6.0;
    let detail = match cache.entries.get(&row.oid) {
        Some(State::Ready(hover)) => Some(hover.as_ref()),
        _ => None,
    };
    let author = detail.map_or(&row.author, |hover| &hover.author);
    let co_authors = detail.map_or(row.co_authors.as_slice(), |hover| &hover.co_authors);

    ui.horizontal(|ui| {
        avatar(ui, avatars, author, AUTHOR_AVATAR, context, Some(&row.oid));
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(RichText::new(&author.name).strong().color(palette.text));
                email_link(ui, &author.email, palette);
            });
            match (detail, cache.entries.get(&row.oid)) {
                (Some(hover), _) => {
                    let when = relative_time(now_unix(), hover.authored_unix);
                    ui.label(
                        RichText::new(format!("{when} · {}", hover.authored_at))
                            .color(palette.text_muted),
                    );
                }
                (None, Some(State::Failed(message))) => {
                    ui.label(RichText::new(message).color(palette.error));
                }
                _ => {
                    ui.label(RichText::new("Loading…").color(palette.text_muted));
                }
            }
        });
    });

    for person in co_authors {
        ui.horizontal(|ui| {
            ui.add_space((AUTHOR_AVATAR - CO_AUTHOR_AVATAR) / 2.0);
            avatar(ui, avatars, person, CO_AUTHOR_AVATAR, context, None);
            ui.label(RichText::new(&person.name).strong().color(palette.text));
            email_link(ui, &person.email, palette);
            ui.label(
                RichText::new("Co-author")
                    .italics()
                    .color(palette.text_muted),
            );
        });
    }

    ui.separator();
    let message = detail.map_or(row.summary.as_str(), |hover| hover.message.as_str());
    message_with_links(ui, message, context.github, palette);

    if !row.branches.is_empty() {
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            for branch in &row.branches {
                let color = palette.graph_color(branch.color);
                let text = RichText::new(&branch.name).color(color);
                let text = if branch.current { text.strong() } else { text };
                egui::Frame::new()
                    .fill(palette.graph_chip(branch.color, branch.current))
                    .corner_radius(4)
                    .inner_margin(egui::Margin::symmetric(6, 2))
                    .show(ui, |ui| {
                        ui.label(text);
                    });
            }
        });
    }

    if let Some(stats) = detail.and_then(|hover| hover.stats) {
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let files = if stats.files == 1 {
                "1 file changed".to_owned()
            } else {
                format!("{} files changed", stats.files)
            };
            ui.label(RichText::new(files).color(palette.text));
            if stats.insertions > 0 {
                ui.label(RichText::new(", ").color(palette.text_muted));
                ui.label(
                    RichText::new(plural(stats.insertions, "insertion", "(+)"))
                        .color(palette.diff_added),
                );
            }
            if stats.deletions > 0 {
                ui.label(RichText::new(", ").color(palette.text_muted));
                ui.label(
                    RichText::new(plural(stats.deletions, "deletion", "(-)"))
                        .color(palette.diff_removed),
                );
            }
        });
    }

    ui.separator();
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(&row.oid)
                .monospace()
                .small()
                .color(palette.hash),
        );
        if ui
            .add(Button::new("Copy").small())
            .on_hover_text("Copy the full commit hash")
            .on_hover_cursor(CursorIcon::PointingHand)
            .clicked()
        {
            ui.ctx().copy_text(row.oid.clone());
        }
        if let Some(github) = context.github {
            ui.hyperlink_to("Open on GitHub", github.commit_url(&row.oid));
        }
    });
}

fn avatar(
    ui: &mut Ui,
    avatars: &mut AvatarService,
    person: &Person,
    diameter: f32,
    context: &CardContext<'_>,
    authored: Option<&str>,
) {
    let texture = avatars.texture(ui.ctx(), person, context.repo, authored);
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::hover());
    avatars::paint_avatar(
        ui.painter(),
        rect.center(),
        diameter,
        person,
        texture.as_ref(),
        context.palette,
        None,
    );
}

/// `<email>` as a mail link when it is a plain address, as text otherwise.
fn email_link(ui: &mut Ui, email: &str, palette: &Palette) {
    if email.is_empty() {
        return;
    }
    let text = format!("<{email}>");
    match mailto_url(email) {
        Some(url) => {
            ui.hyperlink_to(RichText::new(text).color(palette.accent), url);
        }
        None => {
            ui.label(RichText::new(text).color(palette.text_muted));
        }
    }
}

/// A commit's address is whatever its author typed. Only `local@domain`
/// made of letters, digits and `. _ + -` becomes a link, so an address like
/// `a@b.com?bcc=…` cannot add headers or a body to the message it opens.
pub(crate) fn mailto_url(email: &str) -> Option<String> {
    let (local, domain) = email.split_once('@')?;
    let plain = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-')
            })
    };
    (plain(local) && plain(domain) && domain.contains('.') && !domain.contains(".."))
        .then(|| format!("mailto:{local}@{domain}"))
}

fn plural(count: u64, word: &str, sign: &str) -> String {
    if count == 1 {
        format!("1 {word}{sign}")
    } else {
        format!("{count} {word}s{sign}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Segment<'a> {
    Text(&'a str),
    Issue {
        label: &'a str,
        owner: Option<&'a str>,
        repo: Option<&'a str>,
        number: u64,
    },
}

/// Splits text at issue references, with VS Code's rule:
/// `(owner/repo)?(#|GH-)[1-9][0-9]*` ending at a word boundary.
pub(crate) fn issue_segments(text: &str) -> Vec<Segment<'_>> {
    let bytes = text.as_bytes();
    let is_name = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-');
    let mut segments = Vec::new();
    let mut plain_from = 0;
    let mut index = 0;
    while index < bytes.len() {
        let marker = if bytes[index] == b'#' {
            1
        } else if bytes[index..].starts_with(b"GH-") {
            3
        } else {
            index += 1;
            continue;
        };
        let digits_from = index + marker;
        let mut digits_to = digits_from;
        while digits_to < bytes.len() && bytes[digits_to].is_ascii_digit() {
            digits_to += 1;
        }
        let boundary = digits_to == bytes.len()
            || !bytes[digits_to].is_ascii_alphanumeric() && bytes[digits_to] != b'_';
        let valid = digits_to > digits_from && bytes[digits_from] != b'0' && boundary;
        let Some(number) = valid
            .then(|| text[digits_from..digits_to].parse::<u64>().ok())
            .flatten()
        else {
            index += 1;
            continue;
        };
        // Optional `owner/repo` right before the marker.
        let mut start = index;
        let (mut owner, mut repo) = (None, None);
        let mut repo_from = index;
        while repo_from > plain_from && is_name(bytes[repo_from - 1]) {
            repo_from -= 1;
        }
        if repo_from < index && repo_from > plain_from && bytes[repo_from - 1] == b'/' {
            let mut owner_from = repo_from - 1;
            while owner_from > plain_from && is_name(bytes[owner_from - 1]) {
                owner_from -= 1;
            }
            if owner_from < repo_from - 1 {
                owner = Some(&text[owner_from..repo_from - 1]);
                repo = Some(&text[repo_from..index]);
                start = owner_from;
            }
        }
        if start > plain_from {
            segments.push(Segment::Text(&text[plain_from..start]));
        }
        segments.push(Segment::Issue {
            label: &text[start..digits_to],
            owner,
            repo,
            number,
        });
        plain_from = digits_to;
        index = digits_to;
    }
    if plain_from < text.len() {
        segments.push(Segment::Text(&text[plain_from..]));
    }
    segments
}

fn message_with_links(ui: &mut Ui, message: &str, github: Option<&GitHubRepo>, palette: &Palette) {
    let mut lines = message.lines();
    if let Some(subject) = lines.next() {
        line_with_links(ui, subject, github, palette, true);
    }
    let body: Vec<&str> = lines.collect();
    let body_start = body.iter().position(|line| !line.trim().is_empty());
    if let Some(start) = body_start {
        for line in &body[start..] {
            if line.trim().is_empty() {
                ui.add_space(2.0);
            } else {
                line_with_links(ui, line, github, palette, false);
            }
        }
    }
}

fn line_with_links(
    ui: &mut Ui,
    line: &str,
    github: Option<&GitHubRepo>,
    palette: &Palette,
    strong: bool,
) {
    let text = |content: &str| {
        let rich = RichText::new(content).color(palette.text);
        if strong { rich.strong() } else { rich }
    };
    let Some(github) = github else {
        ui.label(text(line));
        return;
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for segment in issue_segments(line) {
            match segment {
                Segment::Text(content) => {
                    ui.label(text(content));
                }
                Segment::Issue {
                    label,
                    owner,
                    repo,
                    number,
                } => {
                    let url = match (owner, repo) {
                        (Some(owner), Some(name)) => GitHubRepo {
                            owner: owner.to_owned(),
                            name: name.to_owned(),
                        }
                        .issue_url(number),
                        _ => github.issue_url(number),
                    };
                    ui.hyperlink_to(RichText::new(label).color(palette.accent), url);
                }
            }
        }
    });
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// "just now", "5 minutes ago", "3 days ago", "2 years ago". A time in the
/// future (clock skew) reads as "just now".
pub(crate) fn relative_time(now: i64, then: i64) -> String {
    let seconds = now.saturating_sub(then).max(0);
    let units = [
        (365 * 24 * 60 * 60, "year"),
        (30 * 24 * 60 * 60, "month"),
        (7 * 24 * 60 * 60, "week"),
        (24 * 60 * 60, "day"),
        (60 * 60, "hour"),
        (60, "minute"),
    ];
    for (size, unit) in units {
        let count = seconds / size;
        if count >= 1 {
            return if count == 1 {
                format!("1 {unit} ago")
            } else {
                format!("{count} {unit}s ago")
            };
        }
    }
    "just now".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_times_read_naturally() {
        let now = 1_800_000_000;
        assert_eq!(relative_time(now, now), "just now");
        assert_eq!(relative_time(now, now + 500), "just now");
        assert_eq!(relative_time(now, now - 59), "just now");
        assert_eq!(relative_time(now, now - 60), "1 minute ago");
        assert_eq!(relative_time(now, now - 5 * 60), "5 minutes ago");
        assert_eq!(relative_time(now, now - 3 * 3600), "3 hours ago");
        assert_eq!(relative_time(now, now - 86_400), "1 day ago");
        assert_eq!(relative_time(now, now - 15 * 86_400), "2 weeks ago");
        assert_eq!(relative_time(now, now - 70 * 86_400), "2 months ago");
        assert_eq!(relative_time(now, now - 800 * 86_400), "2 years ago");
    }

    #[test]
    fn issue_references_follow_the_vs_code_rule() {
        assert_eq!(
            issue_segments("Fix #12 and GH-7, see octo/repo#3."),
            [
                Segment::Text("Fix "),
                Segment::Issue {
                    label: "#12",
                    owner: None,
                    repo: None,
                    number: 12,
                },
                Segment::Text(" and "),
                Segment::Issue {
                    label: "GH-7",
                    owner: None,
                    repo: None,
                    number: 7,
                },
                Segment::Text(", see "),
                Segment::Issue {
                    label: "octo/repo#3",
                    owner: Some("octo"),
                    repo: Some("repo"),
                    number: 3,
                },
                Segment::Text("."),
            ]
        );
        for plain in ["#0", "#012", "#12abc", "issue #", "C#", "##", ""] {
            assert!(
                issue_segments(plain)
                    .iter()
                    .all(|segment| matches!(segment, Segment::Text(_))),
                "{plain}"
            );
        }
        assert_eq!(
            issue_segments("#99999999999999999999999"),
            [Segment::Text("#99999999999999999999999")]
        );
        assert_eq!(
            issue_segments("ünïcode #5 ✓"),
            [
                Segment::Text("ünïcode "),
                Segment::Issue {
                    label: "#5",
                    owner: None,
                    repo: None,
                    number: 5,
                },
                Segment::Text(" ✓"),
            ]
        );
    }

    #[test]
    fn only_plain_addresses_become_mail_links() {
        assert_eq!(
            mailto_url("ada.voss+git@miskatonic.test").as_deref(),
            Some("mailto:ada.voss+git@miskatonic.test")
        );
        for refused in [
            "a@b.com?bcc=attacker@evil.test&body=see",
            "a@b.com%0Abcc:evil@x.test",
            "a b@c.test",
            "a@b",
            "@b.test",
            "a@",
            "a@@b.test",
            "a@b..test",
            "\"quoted\"@b.test",
            "a@b.test\n",
            "ñ@b.test",
            "javascript:alert(1)//@b.test",
        ] {
            assert_eq!(mailto_url(refused), None, "{refused}");
        }
    }

    #[test]
    fn the_cache_forgets_the_oldest_commits() {
        let mut cache = HoverCache::default();
        for index in 0..MAX_ENTRIES + 10 {
            cache.insert(format!("{index:040x}"), State::Failed(String::new()));
        }
        assert_eq!(cache.entries.len(), MAX_ENTRIES);
        assert!(!cache.entries.contains_key(&format!("{:040x}", 0)));
        assert!(
            cache
                .entries
                .contains_key(&format!("{:040x}", MAX_ENTRIES + 9))
        );
    }

    #[test]
    fn only_the_newest_request_waits_while_one_is_running() {
        let mut cache = HoverCache::default();
        let (_sender, receiver) = mpsc::channel();
        cache.inflight = Some(("a".repeat(40), receiver));
        cache.request(&"a".repeat(40));
        assert_eq!(cache.wanted, None);
        cache.request(&"b".repeat(40));
        cache.request(&"c".repeat(40));
        assert_eq!(cache.wanted, Some("c".repeat(40)));
    }
}
