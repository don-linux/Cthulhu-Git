//! The card shown while the pointer rests on a commit, modeled on VS Code's
//! Source Control Graph hover: author, date, co-authors, the full message,
//! branches, tags, the short stat, the hash and a link to GitHub.
//!
//! The details come from one `git show` per commit, run on a worker thread
//! the first time that commit's card opens.

use std::path::Path;

use cthulhu_git::git::{
    self, CommitHover, CommitStats, Git, GitError, GraphBranch, GraphRow, Person,
};
use cthulhu_git::github::{GitHubRepo, RepoContext};
use eframe::egui::{self, CursorIcon, Label, RichText, Sense, Ui, Vec2};

use super::async_cache::{AsyncCache, Cached, GitRead};
use super::avatars::{self, AvatarService};
use super::theme::Palette;
use super::{clock, text, widgets};

const CARD_WIDTH: f32 = 440.0;
const AUTHOR_AVATAR: f32 = 40.0;
const CO_AUTHOR_AVATAR: f32 = 22.0;

/// Details already read, by commit hash, and the one `git show` in flight.
pub type HoverCache = AsyncCache<String, CommitHover>;

/// What [`HoverCache::cached`] found for one commit.
pub(super) type CachedCommit<'a> = Cached<'a, CommitHover>;

impl GitRead<CommitHover> for String {
    const LOST: &'static str = "The commit details stopped unexpectedly.";

    fn read(&self, git: &Git, root: &Path) -> Result<CommitHover, GitError> {
        git::commit_hover(git, root, self)
    }
}

/// What the card needs besides the row.
pub struct CardContext<'a> {
    pub palette: &'a Palette,
    pub github: Option<&'a GitHubRepo>,
    pub repo: Option<&'a RepoContext>,
}

/// Opens the author card while the pointer rests on `response` or on the card.
///
/// A hover tooltip never becomes clickable here, so the hash could not be
/// copied. This popup takes clicks, and it stays up when the pointer moves
/// from the author cell onto the hash.
pub fn show_card(
    response: &egui::Response,
    row: &GraphRow,
    cache: &mut HoverCache,
    avatars: &mut AvatarService,
    context: &CardContext<'_>,
) {
    let rect_id = response.id.with("author-card-rect");
    let ctx = &response.ctx;
    let last_rect = ctx.data(|data| data.get_temp::<egui::Rect>(rect_id));
    let pointer = ctx.input(|input| input.pointer.hover_pos());
    let over_card = pointer.is_some_and(|pos| last_rect.is_some_and(|rect| rect.contains(pos)));
    if !(response.hovered() || over_card) {
        if last_rect.is_some() {
            ctx.data_mut(|data| data.remove::<egui::Rect>(rect_id));
        }
        return;
    }

    let width = ctx.global_style().spacing.tooltip_width;
    let shown = egui::Popup::from_response(response)
        .kind(egui::PopupKind::Tooltip)
        .gap(4.0)
        .width(width)
        .sense(Sense::hover())
        .interactable(true)
        .show(|ui| {
            ui.style_mut().interaction.selectable_labels = false;
            card(ui, row, cache, avatars, context);
        });
    if let Some(shown) = shown {
        // The gap between the author cell and the card is not part of either
        // rect. A few points of slack let the pointer cross it.
        ctx.data_mut(|data| data.insert_temp(rect_id, shown.response.rect.expand(8.0)));
    }
}

fn card(
    ui: &mut Ui,
    row: &GraphRow,
    cache: &mut HoverCache,
    avatars: &mut AvatarService,
    context: &CardContext<'_>,
) {
    cache.request(row.oid.as_str());
    let palette = context.palette;
    ui.set_max_width(CARD_WIDTH);
    ui.spacing_mut().item_spacing.y = 6.0;
    let (detail, failed) = match cache.cached(row.oid.as_str()) {
        Some(CachedCommit::Ready(hover)) => (Some(hover), None),
        Some(CachedCommit::Failed(message)) => (None, Some(message)),
        Some(CachedCommit::Pending) | None => (None, None),
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
            match (detail, failed) {
                (Some(hover), _) => {
                    let when = relative_time(now_unix(), hover.authored_unix);
                    ui.label(
                        RichText::new(format!("{when} · {}", hover.authored_at))
                            .color(palette.text_muted),
                    );
                }
                (None, Some(message)) => {
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
        branch_chips(ui, &row.branches, palette);
    }

    if let Some(tags) = detail.map(|hover| hover.tags.as_slice())
        && !tags.is_empty()
    {
        ui.separator();
        tag_chips(ui, tags, palette);
    }

    if let Some(stats) = detail.and_then(|hover| hover.stats) {
        ui.separator();
        commit_stats_line(ui, stats, palette);
    }

    ui.separator();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let hash = ui
            .add(
                Label::new(
                    // `small()` replaces the text style, so the family is set
                    // after it: small size, monospace face.
                    RichText::new(&row.oid)
                        .small()
                        .family(egui::FontFamily::Monospace)
                        .color(palette.hash),
                )
                .sense(Sense::click())
                .selectable(false),
            )
            .on_hover_text("Copy the full commit hash")
            .on_hover_cursor(CursorIcon::PointingHand);
        if hash.clicked() {
            widgets::copy_commit_hash(ui.ctx(), &row.oid);
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
///
/// A long address breaks inside the token so it cannot widen the sidebar.
pub(super) fn email_link(ui: &mut Ui, email: &str, palette: &Palette) {
    if email.is_empty() {
        return;
    }
    let text = format!("<{email}>");
    match mailto_url(email) {
        Some(url) => {
            ui.hyperlink_to(
                text::wrapped(ui, RichText::new(text).color(palette.accent)),
                url,
            );
        }
        None => fitting_label(ui, RichText::new(text).color(palette.text_muted)),
    }
}

/// One line, broken anywhere inside `ui`'s width.
fn fitting_label(ui: &mut Ui, text: RichText) {
    ui.add(Label::new(text::wrapped(ui, text)));
}

/// Branch names that point at one commit, in the graph's colors.
pub(super) fn branch_chips(ui: &mut Ui, branches: &[GraphBranch], palette: &Palette) {
    ui.horizontal_wrapped(|ui| {
        for branch in branches {
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

/// Tag names that point at one commit, in the warning color.
pub(super) fn tag_chips(ui: &mut Ui, tags: &[String], palette: &Palette) {
    ui.horizontal_wrapped(|ui| {
        for tag in tags {
            egui::Frame::new()
                .fill(palette.tag_chip())
                .corner_radius(4)
                .inner_margin(egui::Margin::symmetric(6, 2))
                .show(ui, |ui| {
                    ui.label(RichText::new(tag).color(palette.warning));
                });
        }
    });
}

/// `N files changed, N insertions(+), N deletions(-)`.
pub(super) fn commit_stats_line(ui: &mut Ui, stats: CommitStats, palette: &Palette) {
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

pub(super) fn message_with_links(
    ui: &mut Ui,
    message: &str,
    github: Option<&GitHubRepo>,
    palette: &Palette,
) {
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
        fitting_label(ui, text(line));
        return;
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for segment in issue_segments(line) {
            match segment {
                Segment::Text(content) => {
                    fitting_label(ui, text(content));
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

/// [`clock::now_unix`] as git stores commit times, for [`relative_time`].
pub(super) fn now_unix() -> i64 {
    i64::try_from(clock::now_unix()).unwrap_or(i64::MAX)
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
    use std::time::{Duration, Instant};

    use cthulhu_git::settings::HistoryColumns;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use proptest::prelude::*;

    use super::*;
    use crate::ui::async_cache::MAX_ENTRIES;
    use crate::ui::repo_view::{self, ViewState};
    use crate::ui::test_support::{
        absent_git, commit, opened, painted_texts, proptest_config, tree_text,
    };
    use crate::ui::theme;

    #[test]
    fn relative_times_read_naturally() {
        const NOW: i64 = 1_800_000_000;
        const HOUR: i64 = 60 * 60;
        const DAY: i64 = 24 * HOUR;
        // (seconds ago, text). Each unit starts with a second either side.
        const CASES: [(i64, &str); 22] = [
            // A time in the future is clock skew.
            (-500, "just now"),
            (0, "just now"),
            (59, "just now"),
            (60, "1 minute ago"),
            (5 * 60, "5 minutes ago"),
            (HOUR - 1, "59 minutes ago"),
            (HOUR, "1 hour ago"),
            (HOUR + 1, "1 hour ago"),
            (3 * HOUR, "3 hours ago"),
            (DAY, "1 day ago"),
            (7 * DAY - 1, "6 days ago"),
            (7 * DAY, "1 week ago"),
            (7 * DAY + 1, "1 week ago"),
            (15 * DAY, "2 weeks ago"),
            (30 * DAY - 1, "4 weeks ago"),
            (30 * DAY, "1 month ago"),
            (30 * DAY + 1, "1 month ago"),
            (70 * DAY, "2 months ago"),
            (365 * DAY - 1, "12 months ago"),
            (365 * DAY, "1 year ago"),
            (365 * DAY + 1, "1 year ago"),
            (800 * DAY, "2 years ago"),
        ];
        for (ago, text) in CASES {
            assert_eq!(relative_time(NOW, NOW - ago), text, "{ago}s ago");
        }
        // The difference saturates instead of overflowing.
        for (now, then, text) in [
            (i64::MAX, 0, "292471208677 years ago"),
            (i64::MAX, i64::MIN, "292471208677 years ago"),
            (i64::MIN, i64::MAX, "just now"),
        ] {
            assert_eq!(relative_time(now, then), text, "{now} after {then}");
        }
    }

    /// "just now", then minutes, hours and so on to years; within a unit, the count.
    fn rank(text: &str) -> (usize, u64) {
        const UNITS: [&str; 6] = ["minute", "hour", "day", "week", "month", "year"];
        let Some((count, unit)) = text
            .strip_suffix(" ago")
            .and_then(|text| text.split_once(' '))
        else {
            assert_eq!(text, "just now");
            return (0, 0);
        };
        let unit = UNITS
            .iter()
            .position(|name| unit.trim_end_matches('s') == *name)
            .expect(text);
        (unit + 1, count.parse().expect(text))
    }

    /// An issue reference: its label, number, and owner and repository.
    type Issue<'a> = (&'a str, u64, Option<(&'a str, &'a str)>);

    /// The issues in `text`, once the segments are checked to rebuild `text`
    /// and each label to be `owner/repo`, the marker and the number.
    fn issues(text: &str) -> Vec<Issue<'_>> {
        let is_name = |name: &str| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
        };
        let mut rebuilt = String::new();
        let mut found = Vec::new();
        for segment in issue_segments(text) {
            match segment {
                Segment::Text(part) => rebuilt.push_str(part),
                Segment::Issue {
                    label,
                    owner,
                    repo,
                    number,
                } => {
                    rebuilt.push_str(label);
                    assert_eq!(owner.is_some(), repo.is_some(), "{text:?}");
                    let pair = owner.zip(repo);
                    let prefix = pair.map_or(String::new(), |(owner, repo)| {
                        assert!(is_name(owner) && is_name(repo), "{text:?}: {label:?}");
                        format!("{owner}/{repo}")
                    });
                    assert!(
                        number >= 1
                            && (label == format!("{prefix}#{number}")
                                || label == format!("{prefix}GH-{number}")),
                        "{text:?}: {label:?} is not issue {number}"
                    );
                    found.push((label, number, pair));
                }
            }
        }
        assert_eq!(rebuilt, text);
        found
    }

    proptest! {
        #![proptest_config(proptest_config())]

        #[test]
        fn a_longer_gap_never_reads_shorter(
            now in any::<i64>(),
            left in 0..=i64::MAX,
            right in 0..=i64::MAX,
        ) {
            let (short, long) = (left.min(right), left.max(right));
            let earlier = relative_time(now, now.saturating_sub(short));
            let later = relative_time(now, now.saturating_sub(long));
            prop_assert!(
                rank(&earlier) <= rank(&later),
                "now {now}: {short}s is {earlier}, {long}s is {later}"
            );
        }

        #[test]
        fn issue_segments_rebuild_any_text(
            text in prop_oneof!["\\PC{0,36}", "(GH-|#|[A-Za-z0-9_.]|ünïcode|✓| |/){0,20}"],
        ) {
            issues(&text);
        }
    }

    #[test]
    fn issue_references_follow_the_vs_code_rule() {
        // (text, each issue: label, number, owner and repo)
        #[rustfmt::skip]
        const CASES: &[(&str, &[Issue<'static>])] = &[
            ("", &[]),
            ("#", &[]),
            ("GH-", &[]),
            ("#0", &[]),
            ("#012", &[]),
            ("GH-0", &[]),
            ("C#", &[]),
            ("##", &[]),
            ("issue #", &[]),
            ("#12abc", &[]),
            ("foo#12bar", &[]),
            ("#12_3", &[]),
            // Too large for the number.
            ("#99999999999999999999999", &[]),
            ("#1", &[("#1", 1, None)]),
            ("GH-2", &[("GH-2", 2, None)]),
            ("GH-GH-1", &[("GH-1", 1, None)]),
            ("end #8", &[("#8", 8, None)]),
            ("/#4", &[("#4", 4, None)]),
            // An empty owner is no owner.
            ("/repo#3", &[("#3", 3, None)]),
            // A multibyte letter right before the marker.
            ("ü#9", &[("#9", 9, None)]),
            ("ünïcode #5 ✓", &[("#5", 5, None)]),
            ("Fix #12 and GH-7, see octo/repo#3.", &[
                ("#12", 12, None),
                ("GH-7", 7, None),
                ("octo/repo#3", 3, Some(("octo", "repo"))),
            ]),
            ("a/b/c#1", &[("b/c#1", 1, Some(("b", "c")))]),
            ("owner_name/repo.name#10", &[("owner_name/repo.name#10", 10, Some(("owner_name", "repo.name")))]),
            ("octo/repoGH-3", &[("octo/repoGH-3", 3, Some(("octo", "repo")))]),
        ];
        for (text, expected) in CASES {
            assert_eq!(issues(text), *expected, "{text:?}");
        }
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

    /// Every read goes through a worker; git is installed nowhere, so each
    /// one fails at once. A commit asked for again, as the one on screen is
    /// every frame, stays; the one asked for longest ago leaves.
    #[test]
    fn the_cache_forgets_the_commit_asked_for_longest_ago() {
        let ctx = egui::Context::default();
        let git = absent_git();
        let mut cache = HoverCache::default();
        let oid = |index: usize| format!("{index:040x}");
        for index in 0..=MAX_ENTRIES {
            let wanted = oid(index);
            cache.request(wanted.as_str());
            let deadline = Instant::now() + Duration::from_secs(5);
            while matches!(cache.cached(&wanted), None | Some(CachedCommit::Pending)) {
                assert!(Instant::now() < deadline, "{wanted} never read");
                cache.poll(&ctx, &git, Path::new("/repos"));
                std::thread::sleep(Duration::from_millis(1));
            }
            cache.request(oid(0).as_str());
        }
        let kept: Vec<usize> = (0..=MAX_ENTRIES)
            .filter(|index| cache.cached(&oid(*index)).is_some())
            .collect();
        assert_eq!(kept.len(), MAX_ENTRIES, "{kept:?}");
        assert!(!kept.contains(&1), "the oldest read is still kept");
        assert!(kept.contains(&0), "the commit on screen was dropped");
    }

    /// The message cell opens no card. The author cell opens one with the
    /// row's co-author, addresses and message, and the error of the read:
    /// the fixture's git is installed nowhere.
    #[test]
    fn history_hover_card_names_the_author_and_coauthor() {
        const SUMMARY: &str = "Lanterns for the night expedition";
        let hash = "ab".repeat(20);
        let mut row = commit(SUMMARY);
        row.oid.clone_from(&hash);
        row.author = Person::new("Ada Voss", "ada@miskatonic.test");
        row.co_authors = vec![Person::new("Herbert West", "west@arkham.test")];
        let mut repo = opened("missing-repo", row);
        let mut avatars = AvatarService::disabled();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1500.0, 640.0))
            .with_step_dt(1.0 / 60.0)
            .build_ui(move |ui| {
                theme::apply(ui.ctx(), theme::DEFAULT_THEME);
                let view = ViewState {
                    error: None,
                    branches_open: false,
                    detail_open: false,
                    terminal_open: false,
                    syncing: None,
                    columns: HistoryColumns::default(),
                    history_loading: false,
                    busy: false,
                };
                let _ = repo_view::show(ui, &mut repo, &view, &mut avatars);
            });

        for (cell, opens) in [(SUMMARY, false), ("Ada Voss, Herbert West", true)] {
            harness
                .query_all_by_label(cell)
                .min_by(|left, right| left.rect().left().total_cmp(&right.rect().left()))
                .unwrap_or_else(|| panic!("no {cell:?} cell\n{}", tree_text(&harness)))
                .hover();
            harness.run_steps(60);
            // The read fails at once, and the card says so where the date goes.
            let deadline = Instant::now() + Duration::from_secs(5);
            while opens && !tree_text(&harness).contains("Could not run git") {
                assert!(Instant::now() < deadline, "{}", tree_text(&harness));
                std::thread::sleep(Duration::from_millis(10));
                harness.step();
            }
            let text = tree_text(&harness);
            let card = if opens {
                text.contains("Co-author")
                    && text.contains("Herbert West")
                    && text.contains("<west@arkham.test>")
                    && text.matches(SUMMARY).count() == 2
            } else {
                text.contains("Co-author") || text.contains("Loading…")
            };
            assert_eq!(card, opens, "{cell}\n{text}");
        }
        let painted = painted_texts(&harness);
        // Avatars are off in tests, so each person is drawn as initials.
        for initials in ["AV", "HW"] {
            let drawn = painted.iter().any(|(_, galley)| galley.text() == initials);
            assert!(drawn, "no {initials} avatar");
        }
        let fonts: Vec<_> = painted
            .into_iter()
            .filter(|(_, galley)| galley.text() == hash)
            .flat_map(|(_, galley)| {
                let sections = galley.job.sections.clone();
                sections
                    .into_iter()
                    .map(|section| section.format.font_id.family)
            })
            .collect();
        assert!(
            !fonts.is_empty()
                && fonts
                    .iter()
                    .all(|family| *family == egui::FontFamily::Monospace),
            "the card's hash is not monospace: {fonts:?}"
        );
    }
}
