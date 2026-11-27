//! The window under hostile input: folders that are not repositories, files
//! that cannot be read, long and bidirectional names, and clicks while busy.
//!
//! Anything that constructs [`CthulhuApp`] does it through a [`Sandbox`]: its
//! own settings file and avatars that never reach the network. Screens and
//! bars are also drawn alone with fixed state, without the app.
//!
//! Opening a repository shows a spinner, which repaints forever, so the app
//! is settled with [`Harness::step`] rather than [`Harness::run`].

#[path = "../../tests/it/support.rs"]
mod fixtures;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::thread;
use std::time::Duration;

use cthulhu_git::git::{CommitGlance, Divergence, Git, Head, RepoGlance};
use cthulhu_git::github::ApiStatus;
use cthulhu_git::settings::{HistoryColumns, Settings};
use eframe::egui::{self, accesskit::Role, accesskit::Toggled};
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::{Harness, HarnessBuilder};
use tempfile::TempDir;

use super::avatars::AvatarService;
use super::github_auth::GitHubAuth;
use super::home::{self, CARD_SIZE, Glance, HomeState};
use super::repo_view::{self, ViewState};
use super::settings::{self, AboutInfo, SettingsView};
use super::test_support::{commit, listed, opened, painted_texts, tree_text};
use super::theme;
use super::{Action, CthulhuApp, OpenedRepo, Screen, Startup, SyncKind};

/// The app's minimum inner size.
const WINDOW_SIZE: egui::Vec2 = egui::Vec2::new(520.0, 400.0);
/// 200 × 100 ms. Opening a repository on a slow runner can take longer than 8 s.
const OPEN_ATTEMPTS: usize = 200;

type ActionLog = Rc<RefCell<Vec<Action>>>;

fn builder<State>(size: egui::Vec2) -> HarnessBuilder<State> {
    HarnessBuilder::default()
        .with_size(size)
        .with_pixels_per_point(2.0)
        .with_step_dt(1.0 / 60.0)
}

/// A settings file of its own for one app, so no test reads or writes the
/// user's.
struct Sandbox {
    _dir: TempDir,
    settings: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let dir = TempDir::with_prefix("cthulhu-ui-config-").expect("config dir");
        let settings = dir.path().join(cthulhu_git::settings::FILE_NAME);
        Self {
            _dir: dir,
            settings,
        }
    }

    /// The app as it starts, on this settings file and without the network.
    fn app(&self, command_line: Option<PathBuf>) -> Harness<'static, CthulhuApp> {
        let startup = Startup {
            command_line,
            settings_path: Some(self.settings.clone()),
            avatars: |_| AvatarService::disabled(),
            find_git: Git::discover,
        };
        builder(WINDOW_SIZE).build_eframe(|cc| CthulhuApp::new(&cc.egui_ctx, startup))
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&fs::read(&self.settings).expect("read settings"))
            .expect("settings json")
    }
}

/// Steps until the workers have delivered the open and its history graph.
fn settle_open(harness: &mut Harness<'_, CthulhuApp>) {
    harness.step();
    for _ in 0..OPEN_ATTEMPTS {
        let app = harness.state();
        if app.opening.is_none() && !app.history_loading() {
            return;
        }
        thread::sleep(Duration::from_millis(100));
        harness.step();
    }
    panic!(
        "open did not settle; error={:?}\n{}",
        harness.state().error,
        tree_text(harness)
    );
}

fn shows<State>(harness: &Harness<'_, State>, text: &str) -> bool {
    harness.query_all_by_label_contains(text).next().is_some()
}

fn assert_shows<State>(harness: &Harness<'_, State>, text: &str, case: &str) {
    assert!(
        shows(harness, text),
        "{case}: no {text:?} in\n{}",
        tree_text(harness)
    );
}

/// The repository screen, or Home with its title.
fn assert_screen(harness: &Harness<'_, CthulhuApp>, repo: bool, case: &str) {
    let app = harness.state();
    assert!(app.opening.is_none(), "{case}: still opening");
    assert_eq!(
        matches!(app.screen, Screen::Repo { .. }),
        repo,
        "{case}: error={:?}",
        app.error
    );
    assert_eq!(
        shows(harness, "Cthulhu Git"),
        !repo,
        "{case}: the home title\n{}",
        tree_text(harness)
    );
}

/// What the settings file holds before the app starts.
#[derive(Debug, Clone, Copy)]
enum Seed {
    Nothing,
    Corrupt,
    /// `last_repository` names this path in the scratch directory.
    Last(&'static str),
    /// The recent list names this path, and its card is clicked.
    RecentCard(&'static str),
}

/// What the settings file holds afterwards.
#[derive(Debug, Clone, Copy)]
enum Saved {
    NoFile,
    Seed,
    Last(Option<&'static str>),
}

/// Seed, command line, repository screen, texts shown, settings after.
#[rustfmt::skip]
type OpenCase = (Seed, Option<&'static str>, bool, &'static [&'static str], Saved);

#[test]
fn opening_a_folder_ends_in_the_repository_or_a_home_error() {
    // The name, and the graph of its one commit.
    const OPENED: &[&str] = &["rlyeh", "Commit history (1)"];
    #[rustfmt::skip]
    const CASES: [OpenCase; 9] = [
        (Seed::Nothing, Some("plain"), false, &["not inside a Git repository"], Saved::NoFile),
        (Seed::Nothing, Some("gone"), false, &["is not a directory"], Saved::NoFile),
        (Seed::Nothing, Some("notes.txt"), false, &["is not a directory"], Saved::NoFile),
        (Seed::Nothing, Some("rlyeh"), true, OPENED, Saved::Last(Some("rlyeh"))),
        // A repository that is gone since the last session is forgotten.
        (Seed::Last("gone"), None, false, &["is not a directory"], Saved::Last(None)),
        // A file that could not be read is never replaced, not even after an open.
        (Seed::Corrupt, Some("rlyeh"), true, OPENED, Saved::Seed),
        (Seed::Corrupt, None, false, &["is invalid", "using the defaults"], Saved::Seed),
        (Seed::RecentCard("gone"), None, false, &["is not a directory"], Saved::Seed),
        (Seed::RecentCard("rlyeh"), None, true, OPENED, Saved::Last(Some("rlyeh"))),
    ];
    let repo = fixtures::Repo::init("rlyeh");
    repo.commit("Awaken");
    let scratch = repo.tmp.path();
    fs::create_dir(scratch.join("plain")).expect("plain dir");
    fs::write(scratch.join("notes.txt"), "not a repository\n").expect("file");
    for (seed, command_line, on_repo, texts, saved) in CASES {
        let case = format!("{seed:?} with {command_line:?}");
        let sandbox = Sandbox::new();
        let mut settings = Settings::default();
        match seed {
            Seed::Nothing => {}
            Seed::Corrupt => fs::write(&sandbox.settings, b"{").expect("corrupt settings"),
            Seed::Last(name) => settings.last_repository = Some(scratch.join(name)),
            Seed::RecentCard(name) => settings.recent_repositories.push(scratch.join(name)),
        }
        if matches!(seed, Seed::Last(_) | Seed::RecentCard(_)) {
            settings.save_to(&sandbox.settings).expect("seed settings");
        }
        let seeded = fs::read(&sandbox.settings).ok();

        let mut harness = sandbox.app(command_line.map(|name| scratch.join(name)));
        settle_open(&mut harness);
        if let Seed::RecentCard(name) = seed {
            // The card may sit below the fold of the minimum window.
            let card = scratch.join(name).display().to_string();
            harness
                .get_by_role_and_label(Role::Button, &card)
                .click_accesskit();
            settle_open(&mut harness);
        }

        assert_screen(&harness, on_repo, &case);
        for text in texts {
            assert_shows(&harness, text, &case);
            if !on_repo {
                let error = harness.state().error.clone().unwrap_or_default();
                assert!(error.contains(text), "{case}: error={error}");
            }
        }
        match saved {
            Saved::NoFile => assert!(!sandbox.settings.exists(), "{case}: settings written"),
            Saved::Seed => {
                assert_eq!(fs::read(&sandbox.settings).ok(), seeded, "{case}");
                let names: Vec<_> = fs::read_dir(sandbox.settings.parent().expect("dir"))
                    .expect("list config dir")
                    .map(|entry| {
                        let name = entry.expect("entry").file_name();
                        name.to_string_lossy().into_owned()
                    })
                    .collect();
                assert_eq!(names, ["settings.json"], "{case}: files left behind");
            }
            Saved::Last(name) => {
                let expected = name.map(|name| {
                    dunce::canonicalize(scratch.join(name)).expect("canonical repository")
                });
                let last = sandbox.json()["last_repository"]
                    .as_str()
                    .map(PathBuf::from);
                assert_eq!(last, expected, "{case}");
            }
        }
    }
}

#[test]
fn branch_names_git_shortens_ambiguously_still_open() {
    // `%(refname:short)` prints `heads/v1` for a branch that shares its name
    // with a tag, and `heads/HEAD` for a branch called HEAD.
    for (name, setup) in [
        (
            "tag-named",
            &[&["branch", "v1"][..], &["tag", "v1"][..]][..],
        ),
        (
            "head-named",
            &[
                &["update-ref", "refs/heads/HEAD", "HEAD"][..],
                &["symbolic-ref", "HEAD", "refs/heads/HEAD"][..],
            ][..],
        ),
    ] {
        let sandbox = Sandbox::new();
        let repo = fixtures::Repo::init(name);
        repo.commit("Awaken");
        for args in setup {
            repo.git(args);
        }
        let mut harness = sandbox.app(Some(repo.root.clone()));
        settle_open(&mut harness);
        assert_screen(&harness, true, name);
        assert_shows(&harness, name, name);
    }
}

#[test]
fn history_sidebar_toggle_is_saved() {
    let sandbox = Sandbox::new();
    let repo = fixtures::Repo::init("sidebar-repo");
    repo.commit("Awaken");
    let mut harness = sandbox.app(Some(repo.root.clone()));
    settle_open(&mut harness);
    assert_shows(&harness, "Branches", "open");
    let before = sandbox.json();
    assert_eq!(before["history_sidebar_hidden"], false);

    harness
        .get_by_role_and_label(Role::Button, "Hide branches")
        .click();
    harness.step();

    let after = sandbox.json();
    assert_eq!(
        after["history_sidebar_hidden"], true,
        "the toggle was not saved"
    );
    assert_eq!(
        after["last_repository"], before["last_repository"],
        "toggling the sidebar changed last_repository"
    );
    assert!(!shows(&harness, "Branches"), "the sidebar is still open");
    assert_shows(&harness, "Commit history", "closed");
}

/// `show` drawn every frame with the default theme, keeping the last frame's actions.
fn draw(
    size: egui::Vec2,
    mut show: impl FnMut(&mut egui::Ui) -> Vec<Action> + 'static,
) -> (Harness<'static>, ActionLog) {
    let actions = ActionLog::default();
    let log = Rc::clone(&actions);
    let harness = builder(size).build_ui(move |ui| {
        theme::apply(ui.ctx(), theme::DEFAULT_THEME);
        *log.borrow_mut() = show(ui);
    });
    (harness, actions)
}

fn home_screen(
    size: egui::Vec2,
    recent: Vec<PathBuf>,
    glances: HashMap<PathBuf, Glance>,
    busy: Rc<Cell<bool>>,
) -> (Harness<'static>, ActionLog) {
    draw(size, move |ui| {
        let state = HomeState {
            recent: &recent,
            glances: &glances,
            busy: busy.get(),
            opening: None,
            error: None,
        };
        home::show(ui, &state).into_iter().collect()
    })
}

/// The repository screen with avatars off, so nothing reaches the network.
fn repo_screen(
    mut repo: OpenedRepo,
    sidebars: bool,
    syncing: Option<SyncKind>,
    busy: Rc<Cell<bool>>,
) -> (Harness<'static>, ActionLog) {
    let mut avatars = AvatarService::disabled();
    draw(WINDOW_SIZE, move |ui| {
        let view = ViewState {
            error: None,
            branches_open: sidebars,
            detail_open: sidebars,
            terminal_open: false,
            syncing,
            columns: HistoryColumns::default(),
            history_loading: false,
            busy: busy.get(),
        };
        repo_view::show(ui, &mut repo, &view, &mut avatars)
    })
}

fn settings_screen(busy: Rc<Cell<bool>>) -> (Harness<'static>, ActionLog) {
    let mut preview = None;
    let mut github = GitHubAuth::default();
    draw(WINDOW_SIZE, move |ui| {
        let view = SettingsView {
            author_avatars: true,
            api_status: ApiStatus::default(),
            busy: busy.get(),
            about: AboutInfo::current("2.43.0"),
        };
        settings::show(ui, None, None, &mut preview, None, &view, &mut github)
    })
}

const HOME_SIZE: egui::Vec2 = egui::Vec2::new(1100.0, 720.0);

#[test]
fn the_home_screen_shows_the_version_browses_and_opens_settings() {
    let recent = vec![PathBuf::from("/repos/rlyeh")];
    let (mut harness, actions) = home_screen(HOME_SIZE, recent, HashMap::new(), Rc::default());
    // The brand column centers itself from the previous frame's height.
    harness.step();
    for text in [env!("CARGO_PKG_VERSION"), "Recent repositories", "rlyeh"] {
        assert_shows(&harness, text, "home");
    }
    for (button, action) in [
        ("Open repository…", Action::Browse),
        ("Settings", Action::OpenSettings),
        ("/repos/rlyeh", Action::Open(PathBuf::from("/repos/rlyeh"))),
    ] {
        harness.get_by_role_and_label(Role::Button, button).click();
        harness.step();
        assert_eq!(*actions.borrow(), [action], "{button}");
    }
}

#[test]
fn browse_is_disabled_while_busy() {
    // (screen, the control that browses, one that stays enabled, what it asks for)
    let cases = [
        ("home", "Open repository…", "Settings", Action::OpenSettings),
        (
            "repository",
            "Open a folder",
            "Settings",
            Action::OpenSettings,
        ),
        ("settings", "Open a folder", "Back", Action::CloseSettings),
    ];
    for (screen, browse, other, other_action) in cases {
        let busy = Rc::new(Cell::new(false));
        let (mut harness, actions) = match screen {
            "home" => home_screen(
                HOME_SIZE,
                vec![PathBuf::from("/repos/rlyeh")],
                HashMap::new(),
                Rc::clone(&busy),
            ),
            "repository" => repo_screen(
                opened("browse", commit("one")),
                false,
                None,
                Rc::clone(&busy),
            ),
            _ => settings_screen(Rc::clone(&busy)),
        };
        harness.get_by_role_and_label(Role::Button, browse).click();
        harness.step();
        assert_eq!(*actions.borrow(), [Action::Browse], "{screen}");

        busy.set(true);
        harness.step();
        let button = harness.get_by_role_and_label(Role::Button, browse);
        assert!(button.accesskit_node().is_disabled(), "{screen}");
        button.click();
        harness.step();
        assert!(
            actions.borrow().is_empty(),
            "{screen}: {:?}",
            actions.borrow()
        );
        harness.get_by_role_and_label(Role::Button, other).click();
        harness.step();
        assert_eq!(*actions.borrow(), [other_action], "{screen}");
    }
}

/// The card for each path, as laid out on a wide window.
fn card_rects(recent: &[PathBuf]) -> Vec<egui::Rect> {
    let (harness, _) = home_screen(
        egui::vec2(1200.0, 800.0),
        recent.to_vec(),
        HashMap::new(),
        Rc::default(),
    );
    recent
        .iter()
        .map(|path| {
            let label = path.display().to_string();
            harness.get_by_role_and_label(Role::Button, &label).rect()
        })
        .collect()
}

#[test]
fn recent_cards_keep_the_same_size() {
    let long = PathBuf::from("/repos").join("N".repeat(400));
    let rlyeh = PathBuf::from("/repos/rlyeh");
    let pair = card_rects(&[long, rlyeh.clone()]);
    let solo = card_rects(std::slice::from_ref(&rlyeh));
    let wrapped = card_rects(&[
        rlyeh,
        PathBuf::from("/repos/innsmouth"),
        PathBuf::from("/repos/kadath"),
    ]);
    for rect in pair.iter().chain(&solo).chain(&wrapped) {
        assert!(
            (rect.size() - CARD_SIZE).length() <= 1.0,
            "card is {:?}, expected {CARD_SIZE:?}",
            rect.size()
        );
    }
    assert!(
        pair[1].min.x + 1.0 >= pair[0].max.x && (pair[1].min.y - pair[0].min.y).abs() <= 1.0,
        "the second card is not beside the first: {pair:?}"
    );
    assert!(
        (wrapped[0].min.y - wrapped[1].min.y).abs() <= 1.0
            && wrapped[2].min.y + 1.0 >= wrapped[0].max.y,
        "the third card did not wrap onto the next row: {wrapped:?}"
    );
}

#[test]
fn a_card_shows_the_branch_the_commit_and_the_upstream() {
    let ready = PathBuf::from("/repos/rlyeh");
    let missing = PathBuf::from("/repos/innsmouth");
    let pending = PathBuf::from("/repos/kadath");
    let mut glances = HashMap::new();
    glances.insert(
        ready.clone(),
        Glance::Ready(RepoGlance {
            head: Head::Branch("main".to_owned()),
            latest: Some(CommitGlance {
                subject: "Awaken".to_owned(),
                when: "2026-11-10 01:23".to_owned(),
            }),
            upstream: Some(Divergence {
                ahead: 2,
                behind: 1,
            }),
        }),
    );
    glances.insert(missing, Glance::Unreadable);
    let recent = vec![ready, PathBuf::from("/repos/innsmouth"), pending];
    let (harness, _) = home_screen(egui::vec2(1200.0, 800.0), recent, glances, Rc::default());
    for (card, lines) in [
        (
            "/repos/rlyeh",
            &["main", "Awaken", "2026-11-10 01:23", "2 ahead, 1 behind"][..],
        ),
        ("/repos/innsmouth", &["Can't read this repository"][..]),
    ] {
        let card_rect = harness.get_by_role_and_label(Role::Button, card).rect();
        for line in lines {
            let line_rect = harness
                .query_all_by_role_and_label(Role::Label, line)
                .next()
                .unwrap_or_else(|| panic!("no {line:?}\n{}", tree_text(&harness)))
                .rect();
            assert!(
                card_rect.contains_rect(line_rect),
                "{line} {line_rect:?} is outside {card} {card_rect:?}"
            );
        }
    }
    let pending_lines = harness
        .query_all_by_role_and_label(Role::Label, "…")
        .count();
    assert_eq!(pending_lines, 4, "pending card\n{}", tree_text(&harness));
}

/// The bottom bar's buttons left to right, by name.
fn bottom_row<State>(harness: &Harness<'_, State>) -> Vec<(String, egui::Rect)> {
    let buttons: Vec<_> = harness
        .query_all_by_role(Role::Button)
        .map(|node| {
            (
                node.accesskit_node().label().unwrap_or_default(),
                node.rect(),
            )
        })
        .collect();
    let bottom = buttons
        .iter()
        .map(|(_, rect)| rect.center().y)
        .fold(f32::MIN, f32::max);
    let mut row: Vec<_> = buttons
        .into_iter()
        .filter(|(_, rect)| (rect.center().y - bottom).abs() < 8.0)
        .collect();
    row.sort_by(|left, right| left.1.left().total_cmp(&right.1.left()));
    row
}

/// Hovers the button named `name` until a tip with the same words shows.
fn shows_tip(harness: &mut Harness<'_>, name: &str) -> bool {
    let center = harness
        .get_by_role_and_label(Role::Button, name)
        .rect()
        .center();
    harness.hover_at(center);
    (0..60).any(|_| {
        harness.step();
        harness
            .query_all_by_role_and_label(Role::Label, name)
            .next()
            .is_some()
    })
}

#[test]
fn hostile_names_keep_the_bottom_bar_in_order_and_in_the_window() {
    let long_name = "N".repeat(400);
    let long_branch = "b".repeat(240);
    // (repository, branch, latest commit, the branch name fits beside Fetch and Pull)
    let cases = [
        ("minimum", "main", "one", true),
        (
            long_name.as_str(),
            "main",
            "port \u{202E}starboard\u{202C} side",
            true,
        ),
        ("long-branch", long_branch.as_str(), "one", false),
        ("bidi-branch", "topic\u{202E}evil", "one", true),
    ];
    for (name, branch, summary, fits) in cases {
        for sidebars in [true, false] {
            let case = format!("{name:.16} on {branch:.16}, sidebars {sidebars}");
            let mut repo = opened(name, commit(summary));
            repo.info.head = Head::Branch(branch.to_owned());
            repo.branches[0].branch.name = branch.to_owned();
            let (mut harness, _) = repo_screen(repo, sidebars, None, Rc::default());

            let row = bottom_row(&harness);
            let names: Vec<&str> = row.iter().map(|(label, _)| label.as_str()).collect();
            assert_eq!(
                names,
                [
                    "Open another repository",
                    "Open a folder",
                    "Settings",
                    branch,
                    "Fetch",
                    "Pull",
                    "Show terminal"
                ],
                "{case}"
            );
            for pair in row.windows(2) {
                assert!(
                    pair[0].1.right() <= pair[1].1.left() + 1.0,
                    "{case}: {:?} overlaps {:?}",
                    pair[1],
                    pair[0]
                );
            }
            for (label, rect) in &row {
                let inside = rect.width() > 1.0
                    && rect.height() > 1.0
                    && rect.left() >= -1.0
                    && rect.right() <= WINDOW_SIZE.x + 1.0;
                assert!(inside, "{case}: {label:.16} at {rect:?}");
            }
            let (bar, fetch, pull, terminal) = (row[3].1, row[4].1, row[5].1, row[6].1);
            if fits {
                let branch_to_fetch = fetch.left() - bar.right();
                let pull_to_terminal = terminal.left() - pull.right();
                assert!(
                    pull_to_terminal + 1.0 >= branch_to_fetch,
                    "{case}: spare width should sit before the terminal, not between the \
                     branch and Fetch ({branch_to_fetch} then {pull_to_terminal})"
                );
            }
            // The name is cut before Fetch, not painted under it.
            let on_bar: Vec<_> = painted_texts(&harness)
                .into_iter()
                .filter(|(rect, galley)| {
                    rect.is_positive()
                        && galley.job.text == branch
                        && bar
                            .expand2(egui::vec2(4.0, 2.0))
                            .contains(egui::pos2(rect.left(), rect.center().y))
                })
                .collect();
            assert!(!on_bar.is_empty(), "{case}: the branch name is not painted");
            for (rect, _) in &on_bar {
                assert!(
                    rect.right() <= fetch.left() + 1.0,
                    "{case}: the name ends at {} past Fetch at {}",
                    rect.right(),
                    fetch.left()
                );
            }
            for text in [name, summary] {
                assert_shows(&harness, text, &case);
            }
            if sidebars {
                assert_shows(&harness, "Latest commit", &case);
            }
            for tip in ["Open another repository", "Open a folder", "Settings"] {
                assert!(shows_tip(&mut harness, tip), "{case}: no tip {tip:?}");
            }
        }
    }
}

/// The menu's row for `name` once the popup is placed: above the bar and enabled.
fn menu_row<'h>(
    harness: &'h Harness<'_>,
    name: &str,
    bar: egui::Rect,
) -> Option<egui_kittest::Node<'h>> {
    harness.query_all_by_role(Role::Button).find(|node| {
        let rect = node.rect();
        node.accesskit_node().label().as_deref() == Some(name)
            && rect.width() > 1.0
            && rect.center().y < bar.top()
            && !node.accesskit_node().is_disabled()
    })
}

#[test]
fn the_branch_menu_checks_out_only_another_branch() {
    let main = || Head::Branch("main".to_owned());
    let unborn = || Head::Unborn("new".to_owned());
    let detached = Head::Detached {
        short_oid: "abc1234def56".to_owned(),
    };
    // Each branch and whether it is the current one.
    let pair: &[(&str, bool)] = &[("main", true), ("feature", false)];
    let option: &[(&str, bool)] = &[("main", true), ("-switch", false)];
    let other: &[(&str, bool)] = &[("feature", false)];
    let unborn_pair: &[(&str, bool)] = &[("new", true), ("feature", false)];
    let unborn_label = "new (no commits yet)";
    // (case, head, branches, syncing, branch button, the row clicked, checked out)
    #[rustfmt::skip]
    let cases = [
        ("another branch", main(), pair, None, "main", "feature", Some("feature")),
        ("the current branch", main(), pair, None, "main", "main", None),
        ("an option-shaped name", main(), option, None, "main", "-switch", Some("-switch")),
        ("a detached head", detached, other, None, "Detached HEAD at abc1234def56", "feature", Some("feature")),
        ("an unborn head", unborn(), unborn_pair, None, unborn_label, "feature", Some("feature")),
        ("the unborn branch", unborn(), unborn_pair, None, unborn_label, "new", None),
        ("while fetching", main(), pair, Some(SyncKind::Fetch), "main", "feature", None),
        ("while pulling", main(), pair, Some(SyncKind::Pull), "main", "feature", None),
        ("while switching", main(), pair, Some(SyncKind::Checkout), "main", "feature", None),
    ];
    for (case, head, branches, syncing, button, pick, checkout) in cases {
        let mut repo = opened(case, commit("one"));
        repo.info.head = head;
        repo.branches = branches
            .iter()
            .map(|(name, current)| listed(name, *current))
            .collect();
        let (mut harness, actions) = repo_screen(repo, false, syncing, Rc::default());
        let (label, bar) = bottom_row(&harness).swap_remove(3);
        assert!(
            label.contains(button),
            "{case}: the branch button reads {label:?}"
        );
        let branch_button = harness.get_by_role_and_label(Role::Button, &label);
        let disabled = branch_button.accesskit_node().is_disabled();
        assert_eq!(
            disabled,
            syncing.is_some(),
            "{case}: branch button disabled"
        );
        branch_button.click();
        // The popup measures itself on its first frame and is placed on the next.
        let listed = (0..4).any(|_| {
            harness.step();
            menu_row(&harness, pick, bar).is_some()
        });
        assert_eq!(listed, syncing.is_none(), "{case}: menu listed {pick}");
        if listed {
            assert!(
                actions.borrow().is_empty(),
                "{case}: {:?}",
                actions.borrow()
            );
            for (name, _) in branches.iter().filter(|(_, current)| *current) {
                let row = menu_row(&harness, name, bar).expect("current row");
                let access = row.accesskit_node();
                assert!(
                    access.toggled() == Some(Toggled::True) || access.is_disabled(),
                    "{case}: the current row {name} is not selected"
                );
            }
            menu_row(&harness, pick, bar)
                .expect("menu row")
                .click_accesskit();
            harness.step();
        }
        let expected: Vec<_> = checkout
            .map(|name| Action::Checkout(name.to_owned()))
            .into_iter()
            .collect();
        assert_eq!(*actions.borrow(), expected, "{case}");
    }
}

/// Screen readers and kittest's accessibility check need a name on every
/// control, including the icon-only buttons and the branch checkboxes.
#[test]
fn every_button_and_checkbox_has_an_accessible_name() {
    let mut repo = opened("names", commit("one"));
    repo.branches.push(listed("feature", false));
    let (harness, _) = repo_screen(repo, true, None, Rc::default());
    let mut named = Vec::new();
    for role in [Role::Button, Role::CheckBox] {
        for node in harness.query_all_by_role(role) {
            let name = node.accesskit_node().label().unwrap_or_default();
            assert!(
                !name.is_empty(),
                "{role:?} at {:?} has no name",
                node.rect()
            );
            named.push(name);
        }
    }
    for expected in ["Settings", "Open another repository", "main", "feature"] {
        assert!(
            named.iter().any(|name| name == expected),
            "{expected:?} in {named:?}"
        );
    }
}
