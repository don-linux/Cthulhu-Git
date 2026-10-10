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

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::Duration;

use cthulhu_git::git::{
    Branch, Commit, CommitDetail, GraphCommit, GraphTip, Head, RepoInfo, Upstream, layout_graph,
};
use cthulhu_git::settings::Settings;
use eframe::egui::{self, accesskit::Role};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tempfile::TempDir;

use super::home::{self, HomeState};
use super::repo_view;
use super::{CthulhuApp, ListedBranch, OpenedRepo, Screen};

const CHILD_ENV: &str = "CTHULHU_ADVERSARIAL_UI_CHILD";
const WINDOW_SIZE: egui::Vec2 = egui::Vec2::new(520.0, 400.0);
const PIXELS_PER_POINT: f32 = 2.0;
const OPEN_ATTEMPTS: usize = 80;

fn spawned() -> bool {
    std::env::var_os(CHILD_ENV).is_some()
}

fn reexec(test_name: &str) -> Output {
    let config = TempDir::with_prefix("cthulhu-adv-ui-config-").expect("temp config dir");
    let home = TempDir::with_prefix("cthulhu-adv-ui-home-").expect("temp home dir");
    Command::new(std::env::current_exe().expect("test executable"))
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
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("spawn child test")
}

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
        let tmp = TempDir::with_prefix("cthulhu-adv-ui-repo-").expect("temp dir");
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
    let tmp = TempDir::with_prefix("cthulhu-adv-ui-plain-").expect("temp dir");
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

fn bottom_bar_buttons<State>(harness: &Harness<'_, State>) -> Vec<egui::Rect> {
    let buttons: Vec<_> = harness
        .query_all_by_role(Role::Button)
        .map(|button| button.rect())
        .collect();
    let bottom_y = buttons
        .iter()
        .map(|rect| rect.center().y)
        .fold(f32::MIN, f32::max);
    let mut row: Vec<_> = buttons
        .into_iter()
        .filter(|rect| (rect.center().y - bottom_y).abs() < 8.0)
        .collect();
    row.sort_by(|left, right| left.left().total_cmp(&right.left()));
    row
}

fn bottom_bar_label<State>(harness: &Harness<'_, State>, buttons: &[egui::Rect]) -> egui::Rect {
    let y = buttons[0].center().y;
    let mut labels: Vec<_> = harness
        .query_all_by_role(Role::Label)
        .filter(|label| {
            let value = label.value();
            value.as_deref() != Some("Fetch")
                && value.as_deref() != Some("Pull")
                && (label.rect().center().y - y).abs() < 8.0
        })
        .map(|label| label.rect())
        .collect();
    labels.sort_by(|left, right| left.left().total_cmp(&right.left()));
    assert_eq!(
        labels.len(),
        1,
        "expected the branch name as the only bottom-bar label besides Fetch and Pull"
    );
    labels[0]
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
        author_name: "cthulhu".to_owned(),
        author_email: "cthulhu@example.invalid".to_owned(),
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
                commit_count,
                upstream: Upstream::None,
                current: true,
            },
            checked: true,
        }],
        latest,
        view_id: egui::Id::new(("adversarial-repo", name.len(), truncated)),
        terminal: None,
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
    let tmp = TempDir::with_prefix("cthulhu-adv-ui-missing-").expect("temp dir");
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
    let tmp = TempDir::with_prefix("cthulhu-adv-ui-file-").expect("temp dir");
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
    let tmp = TempDir::with_prefix("cthulhu-adv-ui-gone-").expect("temp dir");
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

#[test]
fn adversarial_min_window_long_name_and_bidi_summary() {
    let name = "N".repeat(400);
    let summary = "port \u{202E}starboard\u{202C} side";
    let mut repo = opened(&name, vec![commit(summary)], false);
    let harness = ui_harness(|ui| {
        let _ = repo_view::show(ui, &mut repo, None, true, true, false, None);
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
    let harness = ui_harness(|ui| {
        let _ = repo_view::show(ui, &mut repo, None, true, true, false, None);
    });
    assert_text(&harness, "Fetch");
    assert_text(&harness, "Pull");
    let row = bottom_bar_buttons(&harness);
    assert_eq!(
        row.len(),
        5,
        "expected home, settings, fetch, pull and terminal on the bottom bar"
    );
    let branch = bottom_bar_label(&harness, &row);
    let name_to_fetch = row[2].left() - branch.right();
    let pull_to_terminal = row[4].left() - row[3].right();
    assert!(
        row[2].left() + 1.0 >= branch.right(),
        "fetch overlaps the branch name"
    );
    assert!(row[3].left() + 1.0 >= row[2].right());
    assert!(
        pull_to_terminal + 1.0 >= name_to_fetch,
        "spare width should sit before the terminal, not between the name and fetch \
         (name gap {name_to_fetch}, pull-to-terminal {pull_to_terminal})"
    );

    let long = "b".repeat(240);
    let mut repo = opened("beside-long", vec![commit("one")], false);
    repo.info.head = Head::Branch(long.clone());
    repo.branches[0].branch.name = long;
    repo.branches[0].branch.upstream = Upstream::Tracking {
        name: "origin/main".to_owned(),
        ahead: 0,
        behind: 0,
    };
    let harness = ui_harness(|ui| {
        let _ = repo_view::show(ui, &mut repo, None, true, true, false, None);
    });
    let row = bottom_bar_buttons(&harness);
    assert_eq!(row.len(), 5);
    assert!(
        row[3].right() <= row[4].left() + 1.0,
        "pull is pushed past the terminal"
    );
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
        let _ = repo_view::show(ui, &mut repo, None, true, false, false, None);
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
