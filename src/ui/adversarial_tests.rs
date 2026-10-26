//! Adversarial UI cases.
//!
//! Anything that constructs [`CthulhuApp`] runs in a child process whose
//! `XDG_CONFIG_HOME` and `HOME` are fresh temporary directories. The parent
//! process never reads or writes the real settings file. Pure widgets are
//! driven in-process with [`Harness::new_ui`](egui_kittest::Harness::new_ui).
//!
//! Opening a repository shows a spinner, which repaints forever.
//! [`Harness::run`](egui_kittest::Harness::run) is not used; settle with
//! [`Harness::step`](egui_kittest::Harness::step) instead.
//!
//! `settle_open` waits until the repository chrome is installed. The history
//! graph is a second worker and is not part of that wait; a load still in
//! flight keeps repainting, so the graph cases wait on that separately.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::BTreeSet;
#[cfg(unix)]
use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(unix)]
use std::process::Output;
use std::rc::Rc;
#[cfg(unix)]
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use cthulhu_git::git::{
    Branch, Commit, CommitDetail, GraphCommit, GraphTip, Head, Person, RepoInfo, Upstream,
    layout_graph,
};
use cthulhu_git::github::ApiStatus;
use cthulhu_git::settings::{HistoryColumns, Settings};
use eframe::egui::{self, accesskit::Role};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use tempfile::TempDir;

use super::avatars::AvatarService;
use super::fonts::FontService;
use super::github_auth::GitHubAuth;
use super::home::{self, HomeState};
use super::hover::HoverCache;
use super::repo_view;
use super::settings::{self, AboutInfo, SettingsView};
use super::terminal::Terminal;
use super::theme;
use super::{Action, CthulhuApp, ListedBranch, OpenedRepo, Screen, SyncKind};

const CHILD_ENV: &str = "CTHULHU_ADVERSARIAL_UI_CHILD";
const WINDOW_SIZE: egui::Vec2 = egui::Vec2::new(520.0, 400.0);
const PIXELS_PER_POINT: f32 = 2.0;
const OPEN_ATTEMPTS: usize = 80;

fn spawned() -> bool {
    std::env::var_os(CHILD_ENV).is_some()
}

/// `true` when creating a temp directory failed because the quota is exhausted.
fn is_quota(error: &std::io::Error) -> bool {
    error.kind() == ErrorKind::QuotaExceeded
        || error.raw_os_error() == Some(122)
        || error.to_string().to_ascii_lowercase().contains("quota")
}

fn quota_temp_root() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"),
        PathBuf::from,
    );
    target.join("cthulhu-adv-ui-tmp")
}

/// A temp directory. A `QuotaExceeded` from the default temp location retries
/// under `target/cthulhu-adv-ui-tmp`, unique per call. This does not change
/// the process environment.
fn temp_dir(prefix: &str) -> TempDir {
    match TempDir::with_prefix(prefix) {
        Ok(dir) => dir,
        Err(error) if is_quota(&error) => {
            let root = quota_temp_root();
            fs::create_dir_all(&root).unwrap_or_else(|create| {
                panic!(
                    "quota fallback {}: {create} (original: {error})",
                    root.display()
                )
            });
            TempDir::with_prefix_in(prefix, &root).unwrap_or_else(|again| {
                panic!(
                    "temp dir under {}: {again} (original quota: {error})",
                    root.display()
                )
            })
        }
        Err(error) => panic!("temp dir: {error}"),
    }
}

#[cfg(unix)]
fn reexec(test_name: &str) -> Output {
    reexec_env(test_name, &[], false)
}

/// `clear_git_override` drops `CTHULHU_GIT` so a fake on `PATH` is the one
/// that gets probed. Existing cases leave the override alone.
#[cfg(unix)]
fn reexec_env(test_name: &str, extra: &[(&str, &OsStr)], clear_git_override: bool) -> Output {
    let config = temp_dir("cthulhu-adv-ui-config-");
    let home = temp_dir("cthulhu-adv-ui-home-");
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args([
            "--exact",
            test_name,
            "--include-ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, "1")
        .env("XDG_CONFIG_HOME", config.path())
        .env("HOME", home.path())
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    if clear_git_override {
        command.env_remove("CTHULHU_GIT");
    }
    for (key, value) in extra {
        command.env(key, value);
    }
    command.output().expect("spawn child test")
}

#[cfg(unix)]
fn assert_child_ok(output: &Output, marker: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stdout.lines().filter(|line| line.starts_with("FINDING ")) {
        println!("{line}");
    }
    assert!(
        output.status.success(),
        "{marker} child failed:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("1 passed"),
        "{marker} child did not run:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains(marker),
        "{marker} missing from child stdout:\n{stdout}\n{stderr}"
    );
}

#[cfg(unix)]
fn child_name(name: &str) -> String {
    format!("ui::adversarial_tests::{name}")
}

/// The settings file the child is allowed to touch. Refuses the real user config.
///
/// Linux honors `XDG_CONFIG_HOME`. macOS ignores it and uses
/// `$HOME/Library/Application Support`, so a path under the temporary home is
/// the sandbox there.
fn settings_path() -> PathBuf {
    let config = PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").expect("XDG_CONFIG_HOME"));
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let path = Settings::default_path().expect("settings path");
    let under_config = path.starts_with(&config);
    let under_home = path.starts_with(&home);
    assert!(
        path.is_absolute() && (under_config || under_home),
        "settings path {} is outside the temp config {} and home {}",
        path.display(),
        config.display(),
        home.display()
    );
    // `$HOME/.config` is the Linux fallback used when XDG_CONFIG_HOME is unset.
    // A path there is only acceptable when that directory is the temp config.
    if path.starts_with(home.join(".config")) {
        assert!(under_config, "refusing to touch {}", path.display());
    }
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some(cthulhu_git::settings::FILE_NAME)
    );
    path
}

fn settings_bytes() -> Vec<u8> {
    fs::read(settings_path()).expect("read settings")
}

fn settings_json() -> serde_json::Value {
    serde_json::from_slice(&settings_bytes()).expect("settings json")
}

fn assert_only_settings_file(expected: &[u8]) {
    let path = settings_path();
    let dir = path.parent().expect("config directory");
    let mut names = fs::read_dir(dir)
        .expect("list config dir")
        .map(|entry| {
            entry
                .expect("config entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        vec!["settings.json".to_owned()],
        "unexpected files in {}",
        dir.display()
    );
    assert_eq!(fs::read(&path).expect("read settings"), expected);
}

fn run_git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "cthulhu")
        .env("GIT_AUTHOR_EMAIL", "cthulhu@example.invalid")
        .env("GIT_COMMITTER_NAME", "cthulhu")
        .env("GIT_COMMITTER_EMAIL", "cthulhu@example.invalid")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args(["-c", "commit.gpgSign=false"])
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct GitRepo {
    _tmp: TempDir,
    root: PathBuf,
}

impl GitRepo {
    fn init(name: &str) -> Self {
        let tmp = temp_dir("cthulhu-adv-ui-repo-");
        let root = tmp.path().join(name);
        fs::create_dir(&root).expect("repo dir");
        run_git(&root, &["init", "-b", "main"]);
        Self { _tmp: tmp, root }
    }

    fn empty_commit(&self, message: &str) {
        run_git(&self.root, &["commit", "--allow-empty", "-m", message]);
    }
}

fn plain_dir(name: &str) -> (TempDir, PathBuf) {
    let tmp = temp_dir("cthulhu-adv-ui-plain-");
    let path = tmp.path().join(name);
    fs::create_dir(&path).expect("plain dir");
    (tmp, path)
}

fn app_harness(command_line: Option<PathBuf>) -> Harness<'static, CthulhuApp> {
    Harness::builder()
        .with_size(WINDOW_SIZE)
        .with_pixels_per_point(PIXELS_PER_POINT)
        .build_eframe(|cc| CthulhuApp::new(&cc.egui_ctx, command_line))
}

fn ui_harness<'a>(show: impl FnMut(&mut egui::Ui) + 'a) -> Harness<'a, ()> {
    Harness::builder()
        .with_size(WINDOW_SIZE)
        .with_pixels_per_point(PIXELS_PER_POINT)
        .build_ui(show)
}

/// Poll until the worker thread has delivered an open result.
///
/// Uses [`Harness::step`] only. [`Harness::run`] spins until
/// `ExceededMaxSteps` while the opening spinner is visible.
fn settle_open(harness: &mut Harness<'_, CthulhuApp>) {
    harness.step();
    for _ in 0..OPEN_ATTEMPTS {
        if harness.state().opening.is_none() {
            return;
        }
        thread::sleep(Duration::from_millis(100));
        harness.step();
    }
    panic!(
        "open did not settle after {OPEN_ATTEMPTS} steps; error={:?}\n{}",
        harness.state().error,
        visible_text(harness)
    );
}

fn count_text<State>(harness: &Harness<'_, State>, needle: &str) -> usize {
    harness.query_all_by_label_contains(needle).count()
}

fn assert_text<State>(harness: &Harness<'_, State>, needle: &str) {
    assert!(
        count_text(harness, needle) >= 1,
        "layout is missing {needle:?}\n{}",
        visible_text(harness)
    );
}

fn visible_text<State>(harness: &Harness<'_, State>) -> String {
    harness
        .query_all_by_role(Role::Label)
        .filter_map(|node| node.value())
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_home(harness: &Harness<'_, CthulhuApp>) {
    assert!(
        harness.state().opening.is_none(),
        "still opening: {:?}",
        harness.state().error
    );
    assert!(
        matches!(harness.state().screen, Screen::Home),
        "expected the home screen, error={:?}",
        harness.state().error
    );
    assert_text(harness, "Cthulhu Git");
}

fn assert_repo(harness: &Harness<'_, CthulhuApp>, name: &str) {
    assert!(harness.state().opening.is_none(), "still opening");
    assert!(
        matches!(harness.state().screen, Screen::Repo(_)),
        "expected the repository screen, error={:?}",
        harness.state().error
    );
    assert_text(harness, name);
    assert_eq!(
        count_text(harness, "Cthulhu Git"),
        0,
        "home title is showing on the repository screen"
    );
}

fn click_extreme_button<State>(harness: &Harness<'_, State>, top: bool) {
    let any_button = harness.query_all_by_role(Role::Button).next().is_some();
    assert!(
        any_button,
        "no buttons in the layout:\n{}",
        visible_text(harness)
    );
    let buttons: Vec<_> = harness.query_all_by_role(Role::Button).collect();
    let mut chosen = 0;
    for index in 1..buttons.len() {
        let y = buttons[index].rect().center().y;
        let best_y = buttons[chosen].rect().center().y;
        let pick = match y.total_cmp(&best_y) {
            Ordering::Less => top,
            Ordering::Greater => !top,
            Ordering::Equal => false,
        };
        if pick {
            chosen = index;
        }
    }
    buttons[chosen].click();
}

fn bar_buttons<State>(harness: &Harness<'_, State>, top: bool) -> Vec<egui::Rect> {
    let buttons: Vec<_> = harness
        .query_all_by_role(Role::Button)
        .map(|button| button.rect())
        .collect();
    let edge = buttons.iter().map(|rect| rect.center().y).fold(
        if top { f32::MAX } else { f32::MIN },
        if top { f32::min } else { f32::max },
    );
    let mut row: Vec<_> = buttons
        .into_iter()
        .filter(|rect| (rect.center().y - edge).abs() < 8.0)
        .collect();
    row.sort_by(|left, right| left.left().total_cmp(&right.left()));
    row
}

fn bottom_bar_buttons<State>(harness: &Harness<'_, State>) -> Vec<egui::Rect> {
    bar_buttons(harness, false)
}

fn top_bar_buttons<State>(harness: &Harness<'_, State>) -> Vec<egui::Rect> {
    bar_buttons(harness, true)
}

fn click_leftmost_top_button<State>(harness: &Harness<'_, State>) {
    let buttons: Vec<_> = harness.query_all_by_role(Role::Button).collect();
    assert!(
        !buttons.is_empty(),
        "no buttons in the layout:\n{}",
        visible_text(harness)
    );
    let top_y = buttons
        .iter()
        .map(|button| button.rect().center().y)
        .fold(f32::MAX, f32::min);
    let mut chosen = 0;
    let mut best_x = f32::MAX;
    for (index, button) in buttons.iter().enumerate() {
        let center = button.rect().center();
        if (center.y - top_y).abs() > 1.0 {
            continue;
        }
        if center.x < best_x {
            best_x = center.x;
            chosen = index;
        }
    }
    buttons[chosen].click();
}

fn opened(name: &str, commits: Vec<Commit>, truncated: bool) -> OpenedRepo {
    let commit_count = commits.len() as u64;
    let latest = commits.first().map(|commit| CommitDetail {
        oid: commit.oid.clone(),
        author: Person::new("cthulhu", "cthulhu@example.invalid"),
        co_authors: Vec::new(),
        authored_at: "2026-01-01 00:00".to_owned(),
        message: commit.summary.clone(),
    });
    let mut graph = linear_graph(&commits);
    graph.truncated = truncated;
    let tips = graph_tips(&commits);
    OpenedRepo {
        info: RepoInfo {
            root: PathBuf::from("/repos").join(name),
            name: name.to_owned(),
            head: Head::Branch("main".to_owned()),
        },
        graph,
        graph_selection: tips.clone(),
        graph_requested: tips,
        detached_oid: None,
        branches: vec![ListedBranch {
            branch: Branch {
                name: "main".to_owned(),
                oid: commits.first().map(|commit| commit.oid.clone()),
                committer_time: Some(1_700_000_000),
                commit_count,
                upstream: Upstream::None,
                current: true,
            },
            checked: true,
        }],
        latest,
        view_id: egui::Id::new(("adversarial-repo", name.len(), truncated)),
        terminal: None,
        github: None,
        hover: HoverCache::default(),
    }
}

/// The repository screen with avatars off, so nothing reaches the network.
fn show_repo(
    ui: &mut egui::Ui,
    repo: &mut OpenedRepo,
    branches_open: bool,
    detail_open: bool,
    terminal_open: bool,
) -> Vec<super::Action> {
    repo_view::show(
        ui,
        repo,
        &repo_view::ViewState {
            error: None,
            branches_open,
            detail_open,
            terminal_open,
            syncing: None,
            columns: HistoryColumns::default(),
            history_loading: false,
            busy: false,
        },
        &mut AvatarService::disabled(),
    )
}

fn listed_branch(name: &str, current: bool) -> ListedBranch {
    ListedBranch {
        branch: Branch {
            name: name.to_owned(),
            oid: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            committer_time: Some(1_700_000_000),
            commit_count: 1,
            upstream: Upstream::None,
            current,
        },
        checked: current,
    }
}

fn repo_at(name: &str, head: Head, branches: Vec<ListedBranch>) -> OpenedRepo {
    let mut repo = opened(name, vec![commit("one")], false);
    repo.info.head = head;
    repo.branches = branches;
    repo
}

struct RepoDrive {
    repo: OpenedRepo,
    branches_open: bool,
    detail_open: bool,
    terminal_open: bool,
    syncing: Option<SyncKind>,
    busy: bool,
}

fn closed_repo(repo: OpenedRepo) -> RepoDrive {
    RepoDrive {
        repo,
        branches_open: false,
        detail_open: false,
        terminal_open: false,
        syncing: None,
        busy: false,
    }
}

type ActionLog = Rc<RefCell<Vec<Action>>>;

/// `show_repo` drops its actions. This keeps the latest frame's list.
fn drive_repo(drive: RepoDrive) -> (Harness<'static, ()>, Rc<RefCell<RepoDrive>>, ActionLog) {
    let state = Rc::new(RefCell::new(drive));
    let state_ui = Rc::clone(&state);
    let actions = Rc::new(RefCell::new(Vec::new()));
    let actions_ui = Rc::clone(&actions);
    let mut avatars = AvatarService::disabled();
    let harness = Harness::builder()
        .with_size(WINDOW_SIZE)
        .with_pixels_per_point(PIXELS_PER_POINT)
        .build_ui(move |ui| {
            let mut drive = state_ui.borrow_mut();
            let branches_open = drive.branches_open;
            let detail_open = drive.detail_open;
            let terminal_open = drive.terminal_open;
            let syncing = drive.syncing;
            let busy = drive.busy;
            let produced = repo_view::show(
                ui,
                &mut drive.repo,
                &repo_view::ViewState {
                    error: None,
                    branches_open,
                    detail_open,
                    terminal_open,
                    syncing,
                    columns: HistoryColumns::default(),
                    history_loading: false,
                    busy,
                },
                &mut avatars,
            );
            *actions_ui.borrow_mut() = produced;
        });
    (harness, state, actions)
}

fn about_info() -> AboutInfo {
    AboutInfo {
        version: "0.0.0-test".to_owned(),
        version_date: "2026-01-01".to_owned(),
        commit: "test".to_owned(),
        license: "MIT".to_owned(),
        description: "Adversarial settings harness".to_owned(),
        repository: "https://example.invalid/cthulhu-git".to_owned(),
        git_version: "git version 2.43.0".to_owned(),
    }
}

fn drive_settings(busy: Rc<RefCell<bool>>) -> (Harness<'static, ()>, ActionLog) {
    let actions = Rc::new(RefCell::new(Vec::new()));
    let actions_ui = Rc::clone(&actions);
    let busy_ui = Rc::clone(&busy);
    let fonts = FontService::default();
    let mut preview: Option<Terminal> = None;
    let mut github = GitHubAuth::default();
    let harness = Harness::builder()
        .with_size(WINDOW_SIZE)
        .with_pixels_per_point(PIXELS_PER_POINT)
        .build_ui(move |ui| {
            theme::apply(ui.ctx(), theme::DEFAULT_THEME);
            let view = SettingsView {
                author_avatars: true,
                api_status: ApiStatus::default(),
                busy: *busy_ui.borrow(),
                about: about_info(),
            };
            let produced = settings::show(ui, None, &fonts, &mut preview, None, &view, &mut github);
            *actions_ui.borrow_mut() = produced;
        });
    (harness, actions)
}

fn button_label(node: &egui_kittest::Node<'_>) -> String {
    node.accesskit_node().label().unwrap_or_default()
}

fn bottom_row_nodes<'a, State>(harness: &'a Harness<'_, State>) -> Vec<egui_kittest::Node<'a>> {
    let nodes: Vec<_> = harness.query_all_by_role(Role::Button).collect();
    let bottom_y = nodes
        .iter()
        .map(|node| node.rect().center().y)
        .fold(f32::MIN, f32::max);
    let mut row: Vec<_> = nodes
        .into_iter()
        .filter(|node| (node.rect().center().y - bottom_y).abs() < 8.0)
        .collect();
    row.sort_by(|left, right| left.rect().left().total_cmp(&right.rect().left()));
    row
}

fn buttons_labeled<'a, State>(
    harness: &'a Harness<'_, State>,
    name: &str,
) -> Vec<egui_kittest::Node<'a>> {
    harness
        .query_all_by_role(Role::Button)
        .filter(|node| button_label(node) == name)
        .collect()
}

fn button_covering<'a, State>(
    harness: &'a Harness<'_, State>,
    rect: egui::Rect,
) -> egui_kittest::Node<'a> {
    harness
        .query_all_by_role(Role::Button)
        .find(|node| node.rect().contains(rect.center()))
        .unwrap_or_else(|| panic!("no button contains {rect:?}\n{}", button_dump(harness)))
}

fn button_dump<State>(harness: &Harness<'_, State>) -> String {
    harness
        .query_all_by_role(Role::Button)
        .map(|node| {
            format!(
                "{:?} disabled={} label={:?}",
                node.rect(),
                node.accesskit_node().is_disabled(),
                button_label(&node)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn label_values<State>(harness: &Harness<'_, State>) -> BTreeSet<String> {
    harness
        .query_all_by_role(Role::Label)
        .filter_map(|node| node.value())
        .collect()
}

/// Hover `rect` and wait out egui's 0.5s tooltip delay. Default `step_dt` is 0.25s.
fn hover_tooltip<State>(harness: &mut Harness<'_, State>, rect: egui::Rect) -> String {
    let before = label_values(harness);
    harness.hover_at(rect.center());
    let mut appeared = Vec::new();
    for _ in 0..8 {
        harness.step();
        appeared = label_values(harness)
            .into_iter()
            .filter(|label| !before.contains(label))
            .collect();
        if !appeared.is_empty() {
            break;
        }
    }
    assert_eq!(
        appeared.len(),
        1,
        "tooltip at {rect:?} appeared as {appeared:?}\n{}",
        visible_text(harness)
    );
    appeared.remove(0)
}

fn click_bottom_index<State>(harness: &Harness<'_, State>, index: usize) {
    let nodes = bottom_row_nodes(harness);
    assert!(
        nodes.len() > index,
        "bottom row has {} buttons\n{}",
        nodes.len(),
        button_dump(harness)
    );
    nodes[index].click();
}

fn assert_in_window(rect: egui::Rect, what: &str) {
    assert!(rect.width() > 1.0, "{what} width {:.2}", rect.width());
    assert!(rect.height() > 1.0, "{what} height {:.2}", rect.height());
    assert!(rect.left() >= -1.0, "{what} left {:.2}", rect.left());
    assert!(
        rect.right() <= WINDOW_SIZE.x + 1.0,
        "{what} right {:.2} past {}",
        rect.right(),
        WINDOW_SIZE.x
    );
}

fn describe_action(action: &Action) -> String {
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

fn describe_actions(actions: &[Action]) -> String {
    if actions.is_empty() {
        return "[]".to_owned();
    }
    actions
        .iter()
        .map(describe_action)
        .collect::<Vec<_>>()
        .join(", ")
}

fn assert_exact(actions: &[Action], expected: &[&str]) {
    let got: Vec<String> = actions.iter().map(describe_action).collect();
    let expected: Vec<String> = expected.iter().copied().map(str::to_owned).collect();
    assert_eq!(got, expected, "actions");
}

fn assert_no_checkout(actions: &[Action]) {
    assert!(
        actions
            .iter()
            .all(|action| !matches!(action, Action::Checkout(_))),
        "unexpected checkout: {}",
        describe_actions(actions)
    );
}

fn assert_only_checkout(actions: &[Action], name: &str) {
    assert_eq!(
        actions.len(),
        1,
        "expected one Checkout, got {}",
        describe_actions(actions)
    );
    let Action::Checkout(got) = &actions[0] else {
        panic!("expected Checkout, got {}", describe_actions(actions));
    };
    assert_eq!(got.as_bytes(), name.as_bytes(), "checkout payload bytes");
}

fn selected_or_disabled(node: &egui_kittest::Node<'_>) -> bool {
    let access = node.accesskit_node();
    access.toggled() == Some(egui::accesskit::Toggled::True) || access.is_disabled()
}

fn on_screen(rect: egui::Rect) -> bool {
    rect.width() > 1.0
        && rect.height() > 1.0
        && rect.center().x >= -1.0
        && rect.center().x <= WINDOW_SIZE.x + 1.0
        && rect.center().y >= -1.0
        && rect.center().y <= WINDOW_SIZE.y + 1.0
}

/// The opening frame is a sizing pass: rows sit at the origin and are disabled.
/// A row is ready once it is on screen, off the bottom bar, and can take a click.
fn menu_row_ready<State>(harness: &Harness<'_, State>, name: &str) -> bool {
    let bottom_y = bottom_bar_buttons(harness)
        .first()
        .map(|rect| rect.center().y)
        .unwrap_or(f32::MAX);
    buttons_labeled(harness, name).iter().any(|node| {
        let rect = node.rect();
        on_screen(rect)
            && (rect.center().y - bottom_y).abs() > 8.0
            && !node.accesskit_node().is_disabled()
    })
}

/// The menu popup measures on the opening frame. One extra step places it.
fn open_branch_menu(harness: &mut Harness<'static, ()>, marker: &str) {
    if !menu_row_ready(harness, marker) {
        click_bottom_index(harness, 3);
        harness.step();
    }
    if !menu_row_ready(harness, marker) {
        harness.step();
    }
    assert!(
        menu_row_ready(harness, marker),
        "menu did not list {marker}\n{}",
        button_dump(harness)
    );
}

fn click_off_bar(harness: &mut Harness<'static, ()>, name: &str) {
    off_bar(harness, name).click();
    harness.step();
}

/// The selected row's center ignores a pointer click that jumps in from the bar.
/// A press near the top of the row, then one a few points lower, is taken.
fn click_selected_row(harness: &mut Harness<'static, ()>, name: &str) {
    let rect = off_bar(harness, name).rect();
    let x = rect.center().x;
    for y in [rect.top() + 2.0, rect.top() + 6.0] {
        let pos = egui::pos2(x, y);
        harness.hover_at(pos);
        harness.step();
        for pressed in [true, false] {
            harness.event(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            });
            harness.step();
        }
    }
}

fn off_bar<'a>(harness: &'a Harness<'_, ()>, name: &str) -> egui_kittest::Node<'a> {
    let bottom_y = bottom_bar_buttons(harness)[0].center().y;
    let matches: Vec<_> = buttons_labeled(harness, name)
        .into_iter()
        .filter(|node| (node.rect().center().y - bottom_y).abs() > 8.0)
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "off-bar buttons named {name:?}\n{}",
        button_dump(harness)
    );
    matches.into_iter().next().expect("menu row")
}

struct PaintedText {
    rect: egui::Rect,
    elided: bool,
    text: String,
}

fn painted_texts<State>(harness: &Harness<'_, State>) -> Vec<PaintedText> {
    let mut found = Vec::new();
    for clipped in &harness.output().shapes {
        collect_text(clipped.clip_rect, &clipped.shape, &mut found);
    }
    found
}

fn collect_text(clip: egui::Rect, shape: &egui::Shape, found: &mut Vec<PaintedText>) {
    match shape {
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                collect_text(clip, shape, found);
            }
        }
        egui::Shape::Text(text) => {
            let painted = text.visual_bounding_rect().intersect(clip);
            if painted.width() <= 0.0 || painted.height() <= 0.0 {
                return;
            }
            found.push(PaintedText {
                rect: painted,
                elided: text.galley.elided,
                text: text.galley.job.text.clone(),
            });
        }
        _ => {}
    }
}

fn sync_name(kind: SyncKind) -> &'static str {
    match kind {
        SyncKind::Fetch => "fetch",
        SyncKind::Pull => "pull",
        SyncKind::Checkout => "checkout",
    }
}

/// One lane, each commit the parent of the one above it, `main` on the newest.
fn linear_graph(commits: &[Commit]) -> cthulhu_git::git::HistoryGraph {
    let inputs: Vec<GraphCommit> = commits
        .iter()
        .enumerate()
        .map(|(index, commit)| GraphCommit {
            oid: commit.oid.clone(),
            summary: commit.summary.clone(),
            parents: commits
                .get(index + 1)
                .map(|parent| vec![parent.oid.clone()])
                .unwrap_or_default(),
            author: Person::new("cthulhu", "cthulhu@example.invalid"),
            co_authors: Vec::new(),
        })
        .collect();
    layout_graph(&inputs, &graph_tips(commits))
}

fn graph_tips(commits: &[Commit]) -> Vec<GraphTip> {
    commits
        .first()
        .map(|commit| GraphTip {
            name: "main".to_owned(),
            oid: commit.oid.clone(),
            color: 0,
            principal: true,
            current: true,
        })
        .into_iter()
        .collect()
}

fn commit(summary: &str) -> Commit {
    Commit {
        oid: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        summary: summary.to_owned(),
    }
}

// Windows resolves the config directory with SHGetKnownFolderPath, which
// ignores HOME and XDG_CONFIG_HOME, so these tests cannot be sandboxed there.
#[cfg(unix)]
#[test]
fn adversarial_not_a_repo_shows_home_error() {
    let output = reexec(&child_name("child_not_a_repo"));
    assert_child_ok(&output, "CHILD_OK not_a_repo");
}

#[test]
#[ignore = "child of adversarial_not_a_repo_shows_home_error; isolated config"]
fn child_not_a_repo() {
    if !spawned() {
        return;
    }
    let (_tmp, path) = plain_dir("not-a-repo");
    let mut harness = app_harness(Some(path));
    settle_open(&mut harness);
    assert_home(&harness);
    let message = harness.state().error.clone().unwrap_or_default();
    assert!(
        message.contains("not inside a Git repository"),
        "error={message}"
    );
    assert_text(&harness, "not inside a Git repository");
    println!("CHILD_OK not_a_repo");
}

#[cfg(unix)]
#[test]
fn adversarial_missing_cli_path_shows_directory_error() {
    let output = reexec(&child_name("child_missing_cli_path"));
    assert_child_ok(&output, "CHILD_OK missing_cli_path");
}

#[test]
#[ignore = "child of adversarial_missing_cli_path_shows_directory_error; isolated config"]
fn child_missing_cli_path() {
    if !spawned() {
        return;
    }
    let tmp = temp_dir("cthulhu-adv-ui-missing-");
    let missing = tmp.path().join("does-not-exist");
    let mut harness = app_harness(Some(missing));
    settle_open(&mut harness);
    assert_home(&harness);
    assert_text(&harness, "is not a directory");
    println!("CHILD_OK missing_cli_path");
}

#[cfg(unix)]
#[test]
fn adversarial_file_cli_path_shows_directory_error() {
    let output = reexec(&child_name("child_file_cli_path"));
    assert_child_ok(&output, "CHILD_OK file_cli_path");
}

#[test]
#[ignore = "child of adversarial_file_cli_path_shows_directory_error; isolated config"]
fn child_file_cli_path() {
    if !spawned() {
        return;
    }
    let tmp = temp_dir("cthulhu-adv-ui-file-");
    let file = tmp.path().join("notes.txt");
    fs::write(&file, "not a repository\n").expect("write file");
    let mut harness = app_harness(Some(file));
    settle_open(&mut harness);
    assert_home(&harness);
    assert_text(&harness, "is not a directory");
    println!("CHILD_OK file_cli_path");
}

#[cfg(unix)]
#[test]
fn adversarial_home_keeps_last_repository() {
    let output = reexec(&child_name("child_home_keeps_last_repository"));
    assert_child_ok(&output, "CHILD_OK home_keeps_last_repository");
}

#[test]
#[ignore = "child of adversarial_home_keeps_last_repository; isolated config"]
fn child_home_keeps_last_repository() {
    if !spawned() {
        return;
    }
    let repo = GitRepo::init("rlyeh-valid");
    repo.empty_commit("Awaken");
    let canonical = fs::canonicalize(&repo.root).expect("canonical repo");

    let mut harness = app_harness(Some(repo.root.clone()));
    settle_open(&mut harness);
    assert_repo(&harness, "rlyeh-valid");

    let saved = settings_json();
    assert_eq!(
        saved["last_repository"].as_str().map(PathBuf::from),
        Some(canonical.clone())
    );
    let before_home = settings_bytes();

    // Home is the first button on the bottom row; the terminal toggle is the
    // last. The click is applied after the repository view draws, so the
    // release frame still shows the repo. One more step paints Home.
    click_extreme_button(&harness, false);
    harness.step();
    harness.step();

    assert_home(&harness);
    assert_text(&harness, "rlyeh-valid");
    assert_eq!(settings_bytes(), before_home, "Going Home rewrote settings");
    assert_eq!(
        settings_json()["last_repository"]
            .as_str()
            .map(PathBuf::from),
        Some(canonical)
    );
    println!("CHILD_OK home_keeps_last_repository");
}

#[cfg(unix)]
#[test]
fn adversarial_missing_recent_shows_error_when_opened() {
    let output = reexec(&child_name("child_missing_recent"));
    assert_child_ok(&output, "CHILD_OK missing_recent");
}

#[test]
#[ignore = "child of adversarial_missing_recent_shows_error_when_opened; isolated config"]
fn child_missing_recent() {
    if !spawned() {
        return;
    }
    let tmp = temp_dir("cthulhu-adv-ui-gone-");
    let gone = tmp.path().join("gone-recent-rlyeh");
    fs::create_dir(&gone).expect("recent dir");
    let mut settings = Settings::default();
    settings.recent_repositories.push(gone.clone());
    settings.save_to(&settings_path()).expect("seed settings");
    fs::remove_dir(&gone).expect("delete recent dir");

    let mut harness = app_harness(None);
    settle_open(&mut harness);
    assert_home(&harness);

    if count_text(&harness, "gone-recent-rlyeh") > 0 {
        let quiet_before_click = harness.state().error.is_none();
        {
            let row = harness
                .query_all_by_label_contains("gone-recent-rlyeh")
                .next()
                .expect("recent row");
            row.click();
        }
        settle_open(&mut harness);
        assert_home(&harness);
        assert_text(&harness, "is not a directory");
        if quiet_before_click {
            let still_listed = count_text(&harness, "gone-recent-rlyeh") > 0;
            println!(
                "\nFINDING stale recent is listed with no error until it is clicked; still listed after the error: {still_listed}"
            );
        }
    } else {
        println!("\nFINDING stale recent was removed from the home screen before a click");
    }
    println!("CHILD_OK missing_recent");
}

#[cfg(unix)]
#[test]
fn adversarial_corrupt_settings_are_not_overwritten() {
    let output = reexec(&child_name("child_corrupt_settings"));
    assert_child_ok(&output, "CHILD_OK corrupt_settings");
}

#[test]
#[ignore = "child of adversarial_corrupt_settings_are_not_overwritten; isolated config"]
fn child_corrupt_settings() {
    if !spawned() {
        return;
    }
    let path = settings_path();
    fs::create_dir_all(path.parent().expect("config dir")).expect("create config dir");
    fs::write(&path, b"{").expect("write corrupt settings");

    let mut harness = app_harness(None);
    harness.run_steps(4);

    assert_only_settings_file(b"{");
    assert_home(&harness);
    assert_text(&harness, "is invalid");
    assert_text(&harness, "using the defaults");
    let message = harness.state().error.clone().unwrap_or_default();
    assert!(
        message.contains("is invalid") && message.contains("using the defaults"),
        "error={message}"
    );
    println!("CHILD_OK corrupt_settings");
}

#[cfg(unix)]
#[test]
fn adversarial_history_sidebar_toggle_is_saved() {
    let output = reexec(&child_name("child_history_sidebar_toggle"));
    assert_child_ok(&output, "CHILD_OK history_sidebar_toggle");
}

#[test]
#[ignore = "child of adversarial_history_sidebar_toggle_is_saved; isolated config"]
fn child_history_sidebar_toggle() {
    if !spawned() {
        return;
    }
    let repo = GitRepo::init("sidebar-repo");
    repo.empty_commit("Awaken");
    let mut harness = app_harness(Some(repo.root.clone()));
    settle_open(&mut harness);
    assert_repo(&harness, "sidebar-repo");
    assert_text(&harness, "Branches");
    assert_text(&harness, "Commit history");
    assert_eq!(
        settings_json()["history_sidebar_hidden"].as_bool(),
        Some(false)
    );
    let last_before = settings_json()["last_repository"].clone();

    click_leftmost_top_button(&harness);
    harness.step();

    assert_eq!(
        settings_json()["history_sidebar_hidden"].as_bool(),
        Some(true),
        "branches sidebar toggle was not saved"
    );
    assert_eq!(
        settings_json()["last_repository"],
        last_before,
        "toggling the sidebar changed last_repository"
    );
    assert!(harness.state().settings.history_sidebar_hidden);
    assert_eq!(count_text(&harness, "Branches"), 0);
    assert_text(&harness, "Commit history");
    println!("CHILD_OK history_sidebar_toggle");
}

#[test]
fn adversarial_long_recent_name_stays_in_layout() {
    let name = "N".repeat(400);
    let recent = vec![PathBuf::from("/repos").join(&name)];
    let harness = ui_harness(|ui| {
        let _ = home::show(
            ui,
            &HomeState {
                recent: &recent,
                busy: false,
                opening: None,
                error: None,
            },
        );
    });
    assert_text(&harness, &name);
    assert_text(&harness, "Recent repositories");
}

const HISTORY_TITLES: [&str; 5] = ["Branch", "Graph", "Commit Hash", "Commit Message", "Author"];

fn label_rect<State>(harness: &Harness<'_, State>, exact: &str) -> egui::Rect {
    harness
        .query_all_by_role(Role::Label)
        .find(|node| node.value().as_deref() == Some(exact))
        .unwrap_or_else(|| panic!("missing label {exact:?}\n{}", visible_text(harness)))
        .rect()
}

#[test]
fn history_titles_start_where_their_columns_start() {
    let summary = "lantern inventory";
    let mut repo = opened("labels", vec![commit(summary)], false);
    let harness = Harness::builder()
        .with_size(egui::Vec2::new(1600.0, 500.0))
        .with_pixels_per_point(PIXELS_PER_POINT)
        .build_ui(|ui| {
            theme::apply(ui.ctx(), theme::DEFAULT_THEME);
            let _ = show_repo(ui, &mut repo, false, false, false);
        });

    let titles: Vec<egui::Rect> = HISTORY_TITLES
        .iter()
        .map(|title| label_rect(&harness, title))
        .collect();
    for pair in titles.windows(2) {
        assert!(
            pair[0].left() < pair[1].left(),
            "titles should read {HISTORY_TITLES:?}: {titles:?}"
        );
    }

    let summary_rect = label_rect(&harness, summary);
    let chip = harness
        .query_all_by_label_contains("main")
        .map(|node| node.rect())
        .find(|rect| (rect.center().y - summary_rect.center().y).abs() < 8.0)
        .expect("branch chip on the commit row");
    let hash = label_rect(&harness, "0123456789ab");
    let author = label_rect(&harness, "cthulhu");
    for (title, column) in [
        ("Branch", chip),
        ("Commit Hash", hash),
        ("Commit Message", summary_rect),
        ("Author", author),
    ] {
        let title_rect = label_rect(&harness, title);
        assert!(
            (title_rect.left() - column.left()).abs() <= 1.0,
            "{title} title starts at {} but its column's content at {}",
            title_rect.left(),
            column.left()
        );
        assert!(
            title_rect.bottom() <= column.top() + 1.0,
            "{title} title should sit above its rows"
        );
    }
    assert!(
        chip.right() + 24.0 < hash.left(),
        "the graph sits between the branch label and the hash (chip right {}, hash left {})",
        chip.right(),
        hash.left()
    );
}

#[test]
fn a_narrow_history_keeps_its_titles_in_order() {
    let mut repo = opened("narrow", vec![commit("ritual")], false);
    let harness = ui_harness(|ui| {
        theme::apply(ui.ctx(), theme::DEFAULT_THEME);
        let _ = show_repo(ui, &mut repo, true, true, false);
    });
    assert_text(&harness, "Branch");
    assert_text(&harness, "ritual");
}

#[test]
fn adversarial_min_window_long_name_and_bidi_summary() {
    let name = "N".repeat(400);
    let summary = "port \u{202E}starboard\u{202C} side";
    let mut repo = opened(&name, vec![commit(summary)], false);
    let harness = ui_harness(|ui| {
        let _ = show_repo(ui, &mut repo, true, true, false);
    });
    assert_text(&harness, &name);
    assert_text(&harness, "\u{202E}");
    assert_text(&harness, "starboard");
    assert_text(&harness, "Latest commit");
    assert_text(&harness, "main");
}

#[test]
fn fetch_and_pull_sit_beside_the_branch_name() {
    let mut repo = opened("beside", vec![commit("one")], false);
    repo.branches[0].branch.upstream = Upstream::Tracking {
        name: "origin/main".to_owned(),
        ahead: 1,
        behind: 0,
    };
    let mut harness = ui_harness(|ui| {
        let _ = show_repo(ui, &mut repo, true, true, false);
    });
    assert_text(&harness, "Fetch");
    assert_text(&harness, "Pull");
    let row = bottom_bar_buttons(&harness);
    assert_eq!(
        row.len(),
        7,
        "expected home, folder, settings, branch, fetch, pull and terminal\n{}",
        button_dump(&harness)
    );
    assert_eq!(
        hover_tooltip(&mut harness, row[0]),
        "Open another repository"
    );
    assert_eq!(hover_tooltip(&mut harness, row[1]), "Open a folder");
    assert_eq!(hover_tooltip(&mut harness, row[2]), "Settings");
    let row = bottom_bar_buttons(&harness);
    assert!(
        row[0].right() <= row[1].left() + 1.0,
        "folder overlaps home ({:?} then {:?})",
        row[0],
        row[1]
    );
    assert!(
        row[1].right() <= row[2].left() + 1.0,
        "settings overlaps folder ({:?} then {:?})",
        row[1],
        row[2]
    );
    assert!(
        row[3].right() <= row[4].left() + 1.0,
        "fetch overlaps the branch button ({:?} then {:?})",
        row[3],
        row[4]
    );
    assert!(
        row[4].right() <= row[5].left() + 1.0,
        "pull overlaps fetch ({:?} then {:?})",
        row[4],
        row[5]
    );
    let branch_to_fetch = row[4].left() - row[3].right();
    let pull_to_terminal = row[6].left() - row[5].right();
    assert!(
        pull_to_terminal + 1.0 >= branch_to_fetch,
        "spare width should sit before the terminal, not between the branch and fetch \
         (branch gap {branch_to_fetch}, pull-to-terminal {pull_to_terminal})"
    );

    let long = "b".repeat(240);
    let mut repo = opened("beside-long", vec![commit("one")], false);
    repo.info.head = Head::Branch(long.clone());
    repo.branches[0].branch.name = long.clone();
    repo.branches[0].branch.upstream = Upstream::Tracking {
        name: "origin/main".to_owned(),
        ahead: 0,
        behind: 0,
    };
    let harness = ui_harness(|ui| {
        let _ = show_repo(ui, &mut repo, true, true, false);
    });
    let row = bottom_bar_buttons(&harness);
    assert_eq!(
        row.len(),
        7,
        "long branch bottom row\n{}",
        button_dump(&harness)
    );
    assert!(
        row[5].right() <= row[6].left() + 1.0,
        "pull is pushed past the terminal ({:?} then {:?})",
        row[5],
        row[6]
    );
    assert!(
        row[6].right() <= WINDOW_SIZE.x + 1.0,
        "terminal right {:.2} leaves the window",
        row[6].right()
    );
    let branch = row[3];
    let fetch_left = row[4].left();
    let on_bar: Vec<_> = painted_texts(&harness)
        .into_iter()
        .filter(|painted| {
            painted.text == long
                && painted.rect.center().y >= branch.top() - 2.0
                && painted.rect.center().y <= branch.bottom() + 2.0
                && painted.rect.left() >= branch.left() - 4.0
                && painted.rect.left() <= branch.right() + 4.0
        })
        .collect();
    assert!(
        !on_bar.is_empty(),
        "no galley of the 240-character branch name on the branch button\n{}",
        painted_texts(&harness)
            .iter()
            .filter(|painted| painted.text.chars().filter(|ch| *ch == 'b').count() > 40)
            .map(|painted| {
                format!(
                    "elided={} rect={:?} len={}",
                    painted.elided,
                    painted.rect,
                    painted.text.chars().count()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    );
    for painted in &on_bar {
        assert!(
            painted.rect.right() <= fetch_left + 1.0,
            "branch galley right {:.2} crosses Fetch at {fetch_left:.2}",
            painted.rect.right()
        );
    }
    if on_bar.iter().any(|painted| painted.elided) {
        assert!(
            on_bar.iter().all(|painted| painted.elided),
            "the long branch galley was not elided"
        );
    } else {
        println!(
            "\nFINDING egui did not set elided on the 240-character branch galley; painted text stays left of Fetch"
        );
    }
}

#[test]
fn a_bidi_branch_name_stays_inside_the_bottom_bar() {
    let name = "topic\u{202E}evil";
    let mut repo = opened("bidi-branch", vec![commit("one")], false);
    repo.info.head = Head::Branch(name.to_owned());
    repo.branches[0].branch.name = name.to_owned();
    let harness = ui_harness(|ui| {
        let _ = show_repo(ui, &mut repo, false, false, false);
    });
    let nodes = bottom_row_nodes(&harness);
    assert_eq!(nodes.len(), 7, "bottom row\n{}", button_dump(&harness));
    let index = nodes
        .iter()
        .position(|node| button_label(node).contains('\u{202E}'))
        .unwrap_or_else(|| {
            panic!(
                "no bottom-row button contains U+202E\n{}",
                button_dump(&harness)
            )
        });
    assert_eq!(index, 3, "U+202E button is not the branch control");
    assert_in_window(nodes[3].rect(), "bidi branch");
    assert!(
        nodes[4].rect().left() < nodes[5].rect().left(),
        "Fetch is not left of Pull"
    );
    assert_eq!(button_label(&nodes[4]), "Fetch");
    assert_eq!(button_label(&nodes[5]), "Pull");
}

#[test]
fn the_minimum_window_keeps_folder_branch_fetch_and_pull_reachable() {
    let mut repo = opened("minimum", vec![commit("one")], false);
    let harness = ui_harness(|ui| {
        let _ = show_repo(ui, &mut repo, true, true, false);
    });
    let row = bottom_bar_buttons(&harness);
    assert_eq!(row.len(), 7, "bottom row\n{}", button_dump(&harness));
    assert_eq!(button_label(&bottom_row_nodes(&harness)[3]), "main");
    assert_eq!(button_label(&bottom_row_nodes(&harness)[4]), "Fetch");
    assert_eq!(button_label(&bottom_row_nodes(&harness)[5]), "Pull");
    assert_in_window(row[1], "folder");
    assert_in_window(row[3], "branch");
    assert_in_window(row[4], "fetch");
    assert_in_window(row[5], "pull");
}

#[test]
fn the_folder_button_browses_once_and_ignores_a_second_click_while_busy() {
    let (mut harness, drive, actions) =
        drive_repo(closed_repo(opened("browse", vec![commit("one")], false)));
    let row = bottom_bar_buttons(&harness);
    assert!(row.len() >= 2, "bottom row\n{}", button_dump(&harness));
    let home_index = (0..row.len())
        .find(|&index| hover_tooltip(&mut harness, row[index]) == "Open another repository")
        .expect("home tooltip");
    let row = bottom_bar_buttons(&harness);
    assert!(
        home_index + 1 < row.len(),
        "nothing sits right of home\n{}",
        button_dump(&harness)
    );
    assert_eq!(
        hover_tooltip(&mut harness, row[home_index + 1]),
        "Open a folder"
    );
    {
        let row = bottom_bar_buttons(&harness);
        button_covering(&harness, row[home_index + 1]).click();
    }
    harness.step();
    assert_exact(&actions.borrow(), &["Browse"]);

    drive.borrow_mut().busy = true;
    harness.step();
    {
        let row = bottom_bar_buttons(&harness);
        let folder = button_covering(&harness, row[home_index + 1]);
        assert!(folder.accesskit_node().is_disabled());
        folder.click();
    }
    harness.step();
    assert!(
        actions.borrow().is_empty(),
        "busy folder click emitted {}",
        describe_actions(&actions.borrow())
    );
    let row = bottom_bar_buttons(&harness);
    assert!(
        button_covering(&harness, row[home_index + 1])
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn the_settings_folder_button_sits_right_of_home_and_browses() {
    let busy = Rc::new(RefCell::new(false));
    let (mut harness, actions) = drive_settings(Rc::clone(&busy));
    let row = top_bar_buttons(&harness);
    assert!(
        row.len() >= 3,
        "settings top row has {} buttons\n{}",
        row.len(),
        button_dump(&harness)
    );
    assert_eq!(hover_tooltip(&mut harness, row[0]), "Back");
    assert_eq!(
        hover_tooltip(&mut harness, row[1]),
        "Open another repository"
    );
    assert_eq!(hover_tooltip(&mut harness, row[2]), "Open a folder");
    let row = top_bar_buttons(&harness);
    assert!(
        row[1].right() <= row[2].left() + 1.0,
        "folder is not immediately right of home: {:?} then {:?}",
        row[1],
        row[2]
    );
    assert!(row[2].width() > 1.0, "folder width {:.2}", row[2].width());
    assert!(
        row[2].right() <= WINDOW_SIZE.x + 1.0,
        "folder right {:.2}",
        row[2].right()
    );
    button_covering(&harness, row[2]).click();
    harness.step();
    assert_exact(&actions.borrow(), &["Browse"]);

    *busy.borrow_mut() = true;
    harness.step();
    {
        let row = top_bar_buttons(&harness);
        let folder = button_covering(&harness, row[2]);
        assert!(folder.accesskit_node().is_disabled());
        folder.click();
    }
    harness.step();
    assert!(
        actions.borrow().is_empty(),
        "busy settings folder click emitted {}",
        describe_actions(&actions.borrow())
    );
    let row = top_bar_buttons(&harness);
    assert!(
        button_covering(&harness, row[2])
            .accesskit_node()
            .is_disabled()
    );
}

fn branch_menu_repo(name: &str, head: Head, branches: Vec<ListedBranch>) -> OpenedRepo {
    repo_at(name, head, branches)
}

#[test]
fn the_branch_menu_checks_out_another_branch_and_not_the_current_one() {
    let (mut harness, _drive, actions) = drive_repo(closed_repo(branch_menu_repo(
        "checkout-feature",
        Head::Branch("main".to_owned()),
        vec![listed_branch("main", true), listed_branch("feature", false)],
    )));
    assert_eq!(
        button_label(&bottom_row_nodes(&harness)[3]),
        "main",
        "bottom row\n{}",
        button_dump(&harness)
    );
    open_branch_menu(&mut harness, "feature");
    assert_no_checkout(&actions.borrow());
    {
        let current = off_bar(&harness, "main");
        assert!(
            selected_or_disabled(&current),
            "current row toggled={:?} disabled={}",
            current.accesskit_node().toggled(),
            current.accesskit_node().is_disabled()
        );
        click_off_bar(&mut harness, "feature");
    }
    assert_only_checkout(&actions.borrow(), "feature");

    // The selected row is its own open. A second click in the same harness
    // does not land on a menu row.
    let (mut harness, _drive, actions) = drive_repo(closed_repo(branch_menu_repo(
        "checkout-current",
        Head::Branch("main".to_owned()),
        vec![listed_branch("main", true), listed_branch("feature", false)],
    )));
    open_branch_menu(&mut harness, "feature");
    click_selected_row(&mut harness, "main");
    assert!(
        actions.borrow().is_empty(),
        "clicking the current branch emitted {}",
        describe_actions(&actions.borrow())
    );
    assert!(
        buttons_labeled(&harness, "feature").is_empty(),
        "clicking the current row left the menu open\n{}",
        button_dump(&harness)
    );
}

#[test]
fn a_branch_named_like_an_option_is_passed_through_unchanged() {
    let repo = repo_at(
        "switch",
        Head::Branch("main".to_owned()),
        vec![listed_branch("main", true), listed_branch("-switch", false)],
    );
    let (mut harness, _drive, actions) = drive_repo(closed_repo(repo));
    open_branch_menu(&mut harness, "-switch");
    assert_no_checkout(&actions.borrow());
    click_off_bar(&mut harness, "-switch");
    {
        let borrowed = actions.borrow();
        assert_only_checkout(&borrowed, "-switch");
        let Action::Checkout(payload) = &borrowed[0] else {
            unreachable!("assert_only_checkout");
        };
        assert_ne!(payload.as_bytes(), b"switch");
        assert_ne!(payload.as_bytes(), b"--switch");
    }
}

#[test]
fn detached_and_unborn_heads_still_list_other_branches() {
    let detached = repo_at(
        "detached",
        Head::Detached {
            short_oid: "abc1234def56".to_owned(),
        },
        vec![listed_branch("feature", false)],
    );
    let (mut harness, _drive, actions) = drive_repo(closed_repo(detached));
    let label = button_label(&bottom_row_nodes(&harness)[3]);
    assert!(
        label.contains("Detached HEAD at abc1234def56"),
        "branch button is {label:?}"
    );
    click_bottom_index(&harness, 3);
    harness.step();
    assert_no_checkout(&actions.borrow());
    if !menu_row_ready(&harness, "feature") {
        harness.step();
    }
    assert!(
        menu_row_ready(&harness, "feature"),
        "detached menu did not list feature\n{}",
        button_dump(&harness)
    );
    click_off_bar(&mut harness, "feature");
    assert_only_checkout(&actions.borrow(), "feature");

    let unborn = repo_at(
        "unborn",
        Head::Unborn("new".to_owned()),
        vec![listed_branch("new", true), listed_branch("feature", false)],
    );
    let (mut harness, _drive, actions) = drive_repo(closed_repo(unborn));
    let label = button_label(&bottom_row_nodes(&harness)[3]);
    assert!(
        label.contains("new (no commits yet)"),
        "unborn button is {label:?}"
    );
    open_branch_menu(&mut harness, "feature");
    assert_no_checkout(&actions.borrow());
    {
        let current = off_bar(&harness, "new");
        assert!(
            selected_or_disabled(&current),
            "unborn row toggled={:?} disabled={}",
            current.accesskit_node().toggled(),
            current.accesskit_node().is_disabled()
        );
        click_off_bar(&mut harness, "feature");
    }
    assert_only_checkout(&actions.borrow(), "feature");

    let (mut harness, _drive, actions) = drive_repo(closed_repo(repo_at(
        "unborn-current",
        Head::Unborn("new".to_owned()),
        vec![listed_branch("new", true), listed_branch("feature", false)],
    )));
    open_branch_menu(&mut harness, "feature");
    click_selected_row(&mut harness, "new");
    assert!(
        actions
            .borrow()
            .iter()
            .all(|action| !matches!(action, Action::Checkout(_))),
        "clicking the unborn row emitted {}",
        describe_actions(&actions.borrow())
    );
    assert!(
        buttons_labeled(&harness, "feature").is_empty(),
        "clicking the unborn row left the menu open\n{}",
        button_dump(&harness)
    );
}

#[test]
fn the_branch_menu_stays_closed_while_fetch_pull_or_checkout_runs() {
    for kind in [SyncKind::Fetch, SyncKind::Pull, SyncKind::Checkout] {
        let repo = repo_at(
            "syncing",
            Head::Branch("main".to_owned()),
            vec![listed_branch("main", true), listed_branch("feature", false)],
        );
        let mut drive = closed_repo(repo);
        drive.syncing = Some(kind);
        let (mut harness, _drive, actions) = drive_repo(drive);
        assert!(
            buttons_labeled(&harness, "feature").is_empty(),
            "{} listed feature before a click\n{}",
            sync_name(kind),
            button_dump(&harness)
        );
        {
            let nodes = bottom_row_nodes(&harness);
            assert!(
                nodes[3].accesskit_node().is_disabled(),
                "{} branch button is enabled",
                sync_name(kind)
            );
            nodes[3].click();
        }
        harness.step();
        assert_no_checkout(&actions.borrow());
        assert!(
            buttons_labeled(&harness, "feature").is_empty(),
            "{} opened the menu\n{}",
            sync_name(kind),
            button_dump(&harness)
        );
    }
}

#[test]
fn adversarial_thousand_commits_are_virtualized() {
    let commits = (0..1000)
        .map(|index| Commit {
            oid: format!("{index:040x}"),
            summary: format!("commit-{index:04}"),
        })
        .collect();
    let mut repo = opened("bulk-history", commits, true);
    let harness = ui_harness(|ui| {
        let _ = show_repo(ui, &mut repo, true, false, false);
    });
    assert_text(&harness, "Showing the latest 1000 commits.");
    assert_text(&harness, "Commit history (1000+)");
    assert_text(&harness, "commit-0000");
    let rendered = count_text(&harness, "commit-");
    assert!(
        (1..80).contains(&rendered),
        "expected a virtualized history, rendered {rendered} commit rows"
    );
    assert_eq!(count_text(&harness, "commit-0800"), 0);
}

/// Stderr the fake git writes when the history log is told to fail.
/// Not a "not a git repository" line, so the open is not reclassified.
#[cfg(unix)]
const GRAPH_FAIL_TEXT: &str = "fatal: adversarial graph log failed";

/// Seconds the fake sleeps inside the history log. Long enough that the first
/// settled paint still shows the in-flight line, short enough for [`OPEN_ATTEMPTS`].
#[cfg(unix)]
const GRAPH_LOG_SLEEP_SECS: &str = "2";

/// One shared script. Each case symlinks `git` at it and writes its own sidecars,
/// so parallel tests do not rewrite an executable that another child is running.
#[cfg(unix)]
const FAKE_GIT_SCRIPT: &str = r#"#!/bin/sh
# Metadata commands return immediately. Only `log` with `--max-count` sleeps or fails.
# `log -1` is the latest-commit read and must not take either path.
# Sidecars, next to the invoked symlink: .mode .subject .message .sleep .failtext .paths .hits

if [ "${1-}" = "--version" ]; then
  echo "git version 2.43.0"
  exit 0
fi

saw() {
  needle=$1
  shift
  for arg in "$@"; do
    if [ "$arg" = "$needle" ]; then
      return 0
    fi
  done
  return 1
}

emit_graph() {
  subject=$1
  printf '%s\037%s\037%s\037%s\037%s\037%s\037%s\036' \
    "$OID" "" "cthulhu" "cthulhu@example.invalid" "cthulhu" "cthulhu@example.invalid" "$subject"
}

emit_detail() {
  message=$1
  printf '%s\037%s\037%s\037%s\037%s\037%s\037\036%s\n' \
    "$OID" "cthulhu" "cthulhu@example.invalid" "cthulhu" "cthulhu@example.invalid" \
    "2026-01-01 00:00" "$message"
}

record_start() {
  hit_root=$0.hits
  mkdir -p "$hit_root" || exit 1
  n=0
  while ! mkdir "$hit_root/$n" 2>/dev/null; do
    n=$((n + 1))
    if [ "$n" -gt 200 ]; then
      echo "fake git: too many history logs" >&2
      exit 1
    fi
  done
  HIT=$hit_root/$n
  printf '%s\n' "$1" > "$HIT/path"
}

record_done() {
  printf 'done\n' > "$HIT/done"
}

resolve_rule() {
  here=$1
  RULE_ACTION=
  RULE_SUBJECT=
  RULE_MESSAGE=
  if [ -f "$0.paths" ]; then
    while IFS=$(printf '\t') read -r rule_action rule_subject rule_message rule_path; do
      if [ -z "$rule_path" ]; then
        continue
      fi
      if [ "$rule_path" = "$here" ]; then
        RULE_ACTION=$rule_action
        RULE_SUBJECT=$rule_subject
        RULE_MESSAGE=$rule_message
        return 0
      fi
    done < "$0.paths"
    echo "fake git: no history rule for $here" >&2
    exit 1
  fi
  RULE_ACTION=$(cat "$0.mode" 2>/dev/null || echo ok)
  RULE_SUBJECT=$(cat "$0.subject" 2>/dev/null || echo row-subject)
  if [ -f "$0.message" ]; then
    RULE_MESSAGE=$(cat "$0.message")
  else
    RULE_MESSAGE=$RULE_SUBJECT
  fi
}

history_log() {
  here=$(pwd -P) || exit 1
  record_start "$here"
  resolve_rule "$here"
  case "$RULE_ACTION" in
    slow)
      sleep "$SLEEP_SECS"
      emit_graph "$RULE_SUBJECT"
      record_done
      ;;
    fail)
      printf '%s\n' "$FAIL_TEXT" >&2
      record_done
      exit 128
      ;;
    ok)
      emit_graph "$RULE_SUBJECT"
      record_done
      ;;
    *)
      echo "fake git: unknown history action $RULE_ACTION" >&2
      exit 1
      ;;
  esac
}

detail_log() {
  here=$(pwd -P) || exit 1
  resolve_rule "$here"
  emit_detail "$RULE_MESSAGE"
}

OID=$(cat "$0.oid" 2>/dev/null || echo aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa)
SLEEP_SECS=$(cat "$0.sleep" 2>/dev/null || echo 2)
FAIL_TEXT=$(cat "$0.failtext" 2>/dev/null || echo "fatal: adversarial graph log failed")

has_max_count=0
subcommand=
prev=
for arg in "$@"; do
  if [ "$prev" = "-c" ]; then
    prev=
    continue
  fi
  case "$arg" in
    -c)
      prev=-c
      ;;
    --max-count|--max-count=*)
      has_max_count=1
      ;;
    -*)
      ;;
    *)
      if [ -z "$subcommand" ]; then
        subcommand=$arg
      fi
      ;;
  esac
done

case "$subcommand" in
  rev-parse)
    if saw --is-bare-repository "$@"; then
      echo false
      exit 0
    fi
    if saw --show-toplevel "$@"; then
      pwd -P
      exit 0
    fi
    if saw --verify "$@"; then
      printf '%s\n' "$OID"
      exit 0
    fi
    ;;
  symbolic-ref)
    printf '%s\n' refs/heads/main
    exit 0
    ;;
  for-each-ref)
    printf '%s\037%s\037%s\037%s\037%s\n' main "$OID" "" "" 1700000000
    exit 0
    ;;
  rev-list)
    echo 1
    exit 0
    ;;
  config)
    exit 1
    ;;
  log)
    if [ "$has_max_count" -eq 1 ]; then
      history_log
      exit 0
    fi
    detail_log
    exit 0
    ;;
esac
echo "unexpected fake git ($subcommand): $*" >&2
exit 1
"#;

#[cfg(unix)]
fn shared_fake_script() -> &'static Path {
    static SCRIPT: OnceLock<PathBuf> = OnceLock::new();
    SCRIPT.get_or_init(|| {
        let path = quota_temp_root().join("cthulhu-adv-ui-fake-git.sh");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fake git script directory");
        }
        fs::write(&path, FAKE_GIT_SCRIPT).expect("write shared fake git");
        let mut permissions = fs::metadata(&path)
            .expect("fake git metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("chmod fake git");
        path
    })
}

#[cfg(unix)]
fn sidecar(link: &Path, suffix: &str) -> PathBuf {
    let mut name = link.file_name().expect("fake git name").to_os_string();
    name.push(suffix);
    link.with_file_name(name)
}

/// Symlink plus sidecars. The directory stays alive until the child exits.
#[cfg(unix)]
struct FakeGit {
    _dir: TempDir,
    link: PathBuf,
}

#[cfg(unix)]
impl FakeGit {
    fn install() -> Self {
        let dir = temp_dir("cthulhu-adv-ui-git-");
        let link = dir.path().join("git");
        symlink(shared_fake_script(), &link).expect("symlink fake git");
        Self { _dir: dir, link }
    }

    fn dir(&self) -> &Path {
        self.link.parent().expect("fake git directory")
    }
}

#[cfg(unix)]
fn reexec_with_fake(test_name: &str, fake: &FakeGit) -> Output {
    let path = std::env::join_paths(std::iter::once(fake.dir().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("join PATH");
    reexec_env(
        test_name,
        &[
            ("PATH", path.as_os_str()),
            ("CTHULHU_ADV_UI_GIT", fake.link.as_os_str()),
        ],
        true,
    )
}

#[cfg(unix)]
fn fake_from_env() -> PathBuf {
    PathBuf::from(std::env::var_os("CTHULHU_ADV_UI_GIT").expect("CTHULHU_ADV_UI_GIT"))
}

#[cfg(unix)]
fn repo_dir(name: &str) -> (TempDir, PathBuf) {
    let tmp = temp_dir("cthulhu-adv-ui-fake-repo-");
    let root = tmp.path().join(name);
    fs::create_dir(&root).expect("repo dir");
    (tmp, root)
}

#[cfg(unix)]
fn canonical_repo(root: &Path) -> PathBuf {
    fs::canonicalize(root).expect("canonical repo")
}

#[cfg(unix)]
struct GraphCase<'a> {
    action: &'a str,
    subject: &'a str,
    message: &'a str,
    root: &'a Path,
}

#[cfg(unix)]
fn write_sleep_and_failure(link: &Path) {
    fs::write(sidecar(link, ".sleep"), GRAPH_LOG_SLEEP_SECS).expect("sleep sidecar");
    fs::write(sidecar(link, ".failtext"), GRAPH_FAIL_TEXT).expect("failure sidecar");
}

#[cfg(unix)]
fn write_single_case(link: &Path, action: &str, subject: &str, message: &str) {
    fs::write(sidecar(link, ".mode"), action).expect("mode sidecar");
    fs::write(sidecar(link, ".subject"), subject).expect("subject sidecar");
    fs::write(sidecar(link, ".message"), message).expect("message sidecar");
    write_sleep_and_failure(link);
}

#[cfg(unix)]
fn write_graph_cases(link: &Path, cases: &[GraphCase<'_>]) {
    let mut body = String::new();
    for case in cases {
        let path = canonical_repo(case.root);
        let path = path.to_str().expect("utf-8 repo path");
        assert!(
            !case.action.contains('\t')
                && !case.subject.contains('\t')
                && !case.message.contains('\t')
                && !path.contains('\t'),
            "a graph case field contains a tab"
        );
        body.push_str(case.action);
        body.push('\t');
        body.push_str(case.subject);
        body.push('\t');
        body.push_str(case.message);
        body.push('\t');
        body.push_str(path);
        body.push('\n');
    }
    fs::write(sidecar(link, ".paths"), body).expect("path rules");
    write_sleep_and_failure(link);
}

#[cfg(unix)]
#[derive(Debug)]
struct HistoryHit {
    path: String,
    finished: bool,
}

#[cfg(unix)]
fn history_hits(link: &Path) -> Vec<HistoryHit> {
    let root = sidecar(link, ".hits");
    let mut hits = Vec::new();
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return hits,
        Err(error) => panic!("read {}: {error}", root.display()),
    };
    for entry in entries {
        let entry = entry.expect("history hit");
        let path_file = entry.path().join("path");
        if !path_file.is_file() {
            continue;
        }
        let path = fs::read_to_string(&path_file)
            .expect("history hit path")
            .trim_end()
            .to_owned();
        hits.push(HistoryHit {
            path,
            finished: entry.path().join("done").is_file(),
        });
    }
    hits
}

#[cfg(unix)]
fn assert_one_history_log(link: &Path, root: &Path) {
    let hits = history_hits(link);
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one history log (--max-count), saw {} hits: {hits:?}",
        hits.len()
    );
    assert!(
        hits[0].finished,
        "the history log was started but has not finished: {hits:?}"
    );
    let ran = fs::canonicalize(&hits[0].path)
        .unwrap_or_else(|error| panic!("history log directory {}: {error}", hits[0].path));
    assert_eq!(
        ran,
        canonical_repo(root),
        "history log ran in the wrong repository"
    );
}

/// Both workers have exited. A hit is recorded when the log starts and marked
/// done when it is about to exit, so a match means every `--max-count` log
/// has finished and no new one began during the recheck.
#[cfg(unix)]
fn wait_for_history_logs(link: &Path) {
    for _ in 0..OPEN_ATTEMPTS {
        let hits = history_hits(link);
        let started = hits.len();
        let finished = hits.iter().filter(|hit| hit.finished).count();
        if started > 0 && started == finished {
            thread::sleep(Duration::from_millis(200));
            let again = history_hits(link);
            if again.len() == started && again.iter().all(|hit| hit.finished) {
                return;
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("history logs did not finish: {:?}", history_hits(link));
}

#[cfg(unix)]
fn has_exact_label<State>(harness: &Harness<'_, State>, text: &str) -> bool {
    harness
        .query_all_by_role(Role::Label)
        .any(|node| node.value().as_deref() == Some(text))
}

#[cfg(unix)]
fn assert_exact_label<State>(harness: &Harness<'_, State>, text: &str) {
    assert!(
        has_exact_label(harness, text),
        "layout is missing exact label {text:?}\n{}",
        visible_text(harness)
    );
}

/// The center line while this view's graph worker is running and the table is empty.
#[cfg(unix)]
fn assert_opening_history(harness: &Harness<'_, CthulhuApp>, name: &str, row_subject: &str) {
    assert_repo(harness, name);
    assert!(
        !matches!(harness.state().screen, Screen::Home),
        "the in-flight history painted Home, error={:?}\n{}",
        harness.state().error,
        visible_text(harness)
    );
    assert_exact_label(harness, "Opening history…");
    assert_exact_label(harness, "Commit history (0)");
    assert_eq!(
        count_text(harness, "No commits yet."),
        0,
        "an in-flight history said there were no commits\n{}",
        visible_text(harness)
    );
    assert_eq!(
        count_text(harness, row_subject),
        0,
        "commit subject is visible while the history log is still running\n{}",
        visible_text(harness)
    );
    let Screen::Repo(repo) = &harness.state().screen else {
        panic!("expected the repository screen");
    };
    assert!(
        repo.graph.rows.is_empty(),
        "rows arrived before the history log finished"
    );
    assert!(
        repo.graph_selection.is_empty(),
        "graph_selection filled before the worker reported"
    );
    assert!(
        repo.graph_requested.is_empty(),
        "graph_requested filled before the worker reported"
    );
}

/// Poll until the graph worker has painted `subject`. Does not use [`Harness::run`].
#[cfg(unix)]
fn settle_graph(harness: &mut Harness<'_, CthulhuApp>, subject: &str) {
    for _ in 0..OPEN_ATTEMPTS {
        if harness.state().opening.is_none()
            && count_text(harness, "Opening history…") == 0
            && count_text(harness, subject) >= 1
            && has_exact_label(harness, "Commit history (1)")
        {
            return;
        }
        thread::sleep(Duration::from_millis(100));
        harness.step();
    }
    panic!(
        "graph did not show {subject:?}; error={:?}\n{}",
        harness.state().error,
        visible_text(harness)
    );
}

#[cfg(unix)]
fn settle_graph_failure(harness: &mut Harness<'_, CthulhuApp>, needle: &str) {
    for _ in 0..OPEN_ATTEMPTS {
        harness.step();
        if harness.state().opening.is_none()
            && count_text(harness, needle) >= 1
            && count_text(harness, "Opening history…") == 0
        {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "graph failure did not settle; error={:?}\n{}",
        harness.state().error,
        visible_text(harness)
    );
}

#[cfg(unix)]
fn assert_last_repository(root: &Path) {
    assert_eq!(
        settings_json()["last_repository"]
            .as_str()
            .map(PathBuf::from),
        Some(canonical_repo(root)),
        "last_repository is not the canonical root"
    );
}

/// The frame that handles Open also runs `sync_graph` while the previous
/// repository is still on screen. Calling both matches that order. The late
/// worker is not joined.
#[cfg(unix)]
fn supersede_open(harness: &mut Harness<'_, CthulhuApp>, path: PathBuf) {
    let ctx = harness.ctx.clone();
    let app = harness.state_mut();
    app.open(&ctx, path);
    app.sync_graph(&ctx);
}

#[cfg(unix)]
fn settle_named_graph(harness: &mut Harness<'_, CthulhuApp>, name: &str, subject: &str) {
    for _ in 0..OPEN_ATTEMPTS {
        harness.step();
        if harness.state().opening.is_none()
            && matches!(harness.state().screen, Screen::Repo(_))
            && count_text(harness, name) >= 1
            && count_text(harness, subject) >= 1
            && count_text(harness, "Opening history…") == 0
            && has_exact_label(harness, "Commit history (1)")
        {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "second repository did not show {subject:?}; error={:?}\n{}",
        harness.state().error,
        visible_text(harness)
    );
}

#[cfg(unix)]
fn assert_history_contains(link: &Path, root: &Path) {
    let expected = canonical_repo(root);
    let hits = history_hits(link);
    assert!(
        hits.iter().any(|hit| {
            fs::canonicalize(&hit.path)
                .ok()
                .is_some_and(|path| path == expected)
        }),
        "no history log for {}\n{hits:?}",
        expected.display()
    );
}

#[cfg(unix)]
#[test]
fn adversarial_inflight_graph_shows_opening_history() {
    let fake = FakeGit::install();
    let output = reexec_with_fake(
        &child_name("child_inflight_graph_shows_opening_history"),
        &fake,
    );
    assert_child_ok(&output, "CHILD_OK inflight_graph");
}

#[cfg(unix)]
#[test]
#[ignore = "child of adversarial_inflight_graph_shows_opening_history; isolated config"]
fn child_inflight_graph_shows_opening_history() {
    if !spawned() {
        return;
    }
    inflight_graph_shows_opening_history();
}

#[cfg(unix)]
fn inflight_graph_shows_opening_history() {
    let link = fake_from_env();
    let (_tmp, root) = repo_dir("lantern-repo");
    write_single_case(&link, "slow", "row-lantern", "detail-lantern");

    let mut harness = app_harness(Some(root.clone()));
    settle_open(&mut harness);
    assert_opening_history(&harness, "lantern-repo", "row-lantern");

    settle_graph(&mut harness, "row-lantern");
    assert_repo(&harness, "lantern-repo");
    assert_eq!(
        count_text(&harness, "Opening history…"),
        0,
        "opening line stayed after the history log finished\n{}",
        visible_text(&harness)
    );
    assert_text(&harness, "row-lantern");
    assert_exact_label(&harness, "Commit history (1)");
    assert_one_history_log(&link, &root);

    // A follow-up log would record its hit before sleeping. A few frames
    // later the count is still one.
    for _ in 0..3 {
        thread::sleep(Duration::from_millis(50));
        harness.step();
    }
    assert_one_history_log(&link, &root);
    println!("CHILD_OK inflight_graph");
}

#[cfg(unix)]
#[test]
fn adversarial_graph_failure_keeps_repository() {
    let fake = FakeGit::install();
    let output = reexec_with_fake(&child_name("child_graph_failure_keeps_repository"), &fake);
    assert_child_ok(&output, "CHILD_OK graph_failure");
}

#[cfg(unix)]
#[test]
#[ignore = "child of adversarial_graph_failure_keeps_repository; isolated config"]
fn child_graph_failure_keeps_repository() {
    if !spawned() {
        return;
    }
    graph_failure_keeps_repository();
}

#[cfg(unix)]
fn graph_failure_keeps_repository() {
    let link = fake_from_env();
    let (_tmp, root) = repo_dir("failure-repo");
    write_single_case(&link, "fail", "row-failure", "detail-failure");

    let mut harness = app_harness(Some(root.clone()));
    settle_graph_failure(&mut harness, GRAPH_FAIL_TEXT);

    assert!(
        !matches!(harness.state().screen, Screen::Home),
        "graph failure returned to Home, error={:?}\n{}",
        harness.state().error,
        visible_text(&harness)
    );
    assert_repo(&harness, "failure-repo");
    assert_eq!(
        count_text(&harness, "Opening history…"),
        0,
        "opening line stayed after the history log failed\n{}",
        visible_text(&harness)
    );
    assert_text(&harness, GRAPH_FAIL_TEXT);
    let message = harness.state().error.clone().unwrap_or_default();
    assert!(
        message.contains(GRAPH_FAIL_TEXT),
        "error banner text is missing from the app error: {message}"
    );
    assert_last_repository(&root);
    assert_one_history_log(&link, &root);
    println!("CHILD_OK graph_failure");
}

#[cfg(unix)]
#[test]
fn adversarial_home_during_graph_load_keeps_settings() {
    let fake = FakeGit::install();
    let output = reexec_with_fake(
        &child_name("child_home_during_graph_load_keeps_settings"),
        &fake,
    );
    assert_child_ok(&output, "CHILD_OK home_during_graph_load");
}

#[cfg(unix)]
#[test]
#[ignore = "child of adversarial_home_during_graph_load_keeps_settings; isolated config"]
fn child_home_during_graph_load_keeps_settings() {
    if !spawned() {
        return;
    }
    home_during_graph_load_keeps_settings();
}

#[cfg(unix)]
fn home_during_graph_load_keeps_settings() {
    let link = fake_from_env();
    let (_tmp, root) = repo_dir("rlyeh-slow");
    write_single_case(&link, "slow", "row-rlyeh", "detail-rlyeh");

    let mut harness = app_harness(Some(root.clone()));
    settle_open(&mut harness);
    assert_opening_history(&harness, "rlyeh-slow", "row-rlyeh");
    let before_home = settings_bytes();

    // Home is the first button on the bottom row. The click is applied after
    // the repository view draws, so the release frame still shows the repo.
    // One more step paints Home.
    click_extreme_button(&harness, false);
    harness.step();
    harness.step();

    assert_home(&harness);
    assert_eq!(settings_bytes(), before_home, "Going Home rewrote settings");
    assert_last_repository(&root);

    wait_for_history_logs(&link);
    for _ in 0..10 {
        thread::sleep(Duration::from_millis(100));
        harness.step();
        assert_home(&harness);
        assert_eq!(
            settings_bytes(),
            before_home,
            "a late graph result rewrote settings"
        );
        assert!(
            !matches!(harness.state().screen, Screen::Repo(_)),
            "a late graph result restored the repository\n{}",
            visible_text(&harness)
        );
    }
    assert_one_history_log(&link, &root);
    println!("CHILD_OK home_during_graph_load");
}

#[cfg(unix)]
#[test]
fn adversarial_second_open_supersedes_inflight_graph() {
    let fake = FakeGit::install();
    let output = reexec_with_fake(
        &child_name("child_second_open_supersedes_inflight_graph"),
        &fake,
    );
    assert_child_ok(&output, "CHILD_OK second_open_supersedes");
}

#[cfg(unix)]
#[test]
#[ignore = "child of adversarial_second_open_supersedes_inflight_graph; isolated config"]
fn child_second_open_supersedes_inflight_graph() {
    if !spawned() {
        return;
    }
    second_open_supersedes_inflight_graph();
}

#[cfg(unix)]
fn second_open_supersedes_inflight_graph() {
    let link = fake_from_env();
    let (_first_tmp, first) = repo_dir("alpha-repo");
    let (_second_tmp, second) = repo_dir("beta-repo");
    // The same tip hash on both repositories, so a late graph is not discarded
    // merely because the branch tips differ. The subjects are what show the leak.
    write_graph_cases(
        &link,
        &[
            GraphCase {
                action: "slow",
                subject: "row-alpha",
                message: "detail-alpha",
                root: &first,
            },
            GraphCase {
                action: "ok",
                subject: "row-beta",
                message: "detail-beta",
                root: &second,
            },
        ],
    );

    let mut harness = app_harness(Some(first.clone()));
    settle_open(&mut harness);
    assert_opening_history(&harness, "alpha-repo", "row-alpha");

    supersede_open(&mut harness, second.clone());
    settle_named_graph(&mut harness, "beta-repo", "row-beta");
    assert_second_repo_without_first(&harness);

    wait_for_history_logs(&link);
    for _ in 0..10 {
        thread::sleep(Duration::from_millis(100));
        harness.step();
        assert_second_repo_without_first(&harness);
    }
    assert_history_contains(&link, &first);
    assert_history_contains(&link, &second);
    println!("CHILD_OK second_open_supersedes");
}

#[cfg(unix)]
fn assert_second_repo_without_first(harness: &Harness<'_, CthulhuApp>) {
    assert!(
        matches!(harness.state().screen, Screen::Repo(_)),
        "the second open left the screen as Home, error={:?}\n{}",
        harness.state().error,
        visible_text(harness)
    );
    assert_repo(harness, "beta-repo");
    assert_text(harness, "row-beta");
    assert_eq!(
        count_text(harness, "row-alpha"),
        0,
        "the first repository's commit appeared on the second\n{}",
        visible_text(harness)
    );
    assert_eq!(
        count_text(harness, "alpha-repo"),
        0,
        "the first repository's name appeared on the second\n{}",
        visible_text(harness)
    );
    assert_eq!(
        count_text(harness, "Opening history…"),
        0,
        "the second repository is still opening history\n{}",
        visible_text(harness)
    );
}
