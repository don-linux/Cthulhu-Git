//! Read-only git commands must not run a repository diff driver.
//!
//! `many_branches_do_not_spawn_one_rev_list_each` records the process count
//! `branches()` uses today. Collapsing that fan-out is not a small change, so
//! the test locks the current count instead of a tighter bound.
//!
//! Fixture git is isolated from the runner's config. `Git::command` drops
//! inherited `GIT_*` (except the SSH helpers), so those variables cannot
//! carry isolation into the library. The hover test therefore re-execs with
//! an empty `HOME` and `XDG_CONFIG_HOME`, which the library does keep.

#![cfg(unix)]

use std::env;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use cthulhu_git::git::{DiscoverInputs, Head, branches, commit_hover, discover_with, file_diff};
use tempfile::TempDir;

const ISOLATED: &str = "CTHULHU_ADV_D_ISOLATED";

const CLEARED_ENV: &[&str] = &[
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_KEY_0",
    "GIT_CONFIG_VALUE_0",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_EXEC_PATH",
    "GIT_EXTERNAL_DIFF",
];

/// One shared fake git. Sidecars next to the invoked symlink select the refs
/// and the invocation log. A fresh executable per test races with `ETXTBSY`.
const FAKE_GIT_SCRIPT: &str = r#"#!/bin/sh
inv="$0.invocations"
cmd=
for arg in "$@"; do
  case "$arg" in
    --version) cmd=version; break ;;
    for-each-ref) cmd=for-each-ref; break ;;
    rev-list) cmd=rev-list; break ;;
  esac
done
if [ -z "$cmd" ]; then
  echo "unexpected fake git arguments: $*" >&2
  exit 1
fi
printf '%s\n' "$cmd" >> "$inv"
case "$cmd" in
  version)
    echo "git version 2.43.0"
    ;;
  for-each-ref)
    cat "$0.refs"
    ;;
  rev-list)
    echo 4
    ;;
esac
exit 0
"#;

fn shared_script() -> &'static Path {
    static SCRIPT_PATH: OnceLock<PathBuf> = OnceLock::new();
    SCRIPT_PATH.get_or_init(|| {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cthulhu-adv-d-fake-git.sh");
        fs::write(&path, FAKE_GIT_SCRIPT).expect("write shared fake git");
        let mut permissions = fs::metadata(&path)
            .expect("fake git metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("chmod fake git");
        path
    })
}

fn sidecar(link: &Path, suffix: &str) -> PathBuf {
    let mut name = link.file_name().expect("fake git name").to_os_string();
    name.push(suffix);
    link.with_file_name(name)
}

fn link_fake(dir: &Path, name: &str) -> PathBuf {
    fs::create_dir_all(dir).expect("fake git dir");
    let path = dir.join(name);
    symlink(shared_script(), &path).expect("symlink fake git");
    path
}

fn probe_fake(path: &Path) -> cthulhu_git::git::Git {
    let inputs = DiscoverInputs {
        override_path: Some(path.to_path_buf().into()),
        path_var: None,
        exe_name: "git",
        fallbacks: Vec::new(),
        skip_paths: Vec::new(),
    };
    discover_with(&inputs).unwrap_or_else(|error| panic!("probe fake git: {error}"))
}

fn write_executable(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("script directory");
    }
    fs::write(path, body).expect("write script");
    let mut permissions = fs::metadata(path).expect("script metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod script");
}

fn shell_single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Runs fixture git with a private config. The library's own git does not see
/// these `GIT_CONFIG_*` values: `Git::command` removes them.
fn fixture_git(git: &Path, scratch: &Path, root: &Path, args: &[&str]) -> String {
    let hooks = scratch.join("empty-hooks");
    let global = scratch.join("empty.gitconfig");
    let system = scratch.join("empty.system.gitconfig");
    fs::create_dir_all(&hooks).expect("empty hooks");
    fs::write(&global, "").expect("empty global gitconfig");
    fs::write(&system, "").expect("empty system gitconfig");

    let mut command = Command::new(git);
    command
        .current_dir(root)
        .stdin(Stdio::null())
        .env("GIT_AUTHOR_NAME", "cthulhu")
        .env("GIT_AUTHOR_EMAIL", "cthulhu@example.invalid")
        .env("GIT_COMMITTER_NAME", "cthulhu")
        .env("GIT_COMMITTER_EMAIL", "cthulhu@example.invalid")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &global)
        .env("GIT_CONFIG_SYSTEM", &system)
        .args(["-c", "commit.gpgSign=false", "-c", "core.autocrlf=false"])
        .arg("-c")
        .arg(format!("core.hooksPath={}", hooks.display()));
    for var in CLEARED_ENV {
        if *var != "GIT_CONFIG_GLOBAL" && *var != "GIT_CONFIG_SYSTEM" {
            command.env_remove(var);
        }
    }
    command.args(args);
    let output = command.output().expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("utf8")
        .trim()
        .to_owned()
}

#[test]
fn hover_and_file_diff_do_not_run_textconv() {
    let home = TempDir::new().expect("private home");
    let xdg = TempDir::new().expect("private xdg");
    let mut child = Command::new(env::current_exe().expect("test binary"));
    child
        .args([
            "--exact",
            "child_hover_and_file_diff_do_not_run_textconv",
            "--include-ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ISOLATED, "1")
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", xdg.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_CONFIG_GLOBAL")
        .env_remove("GIT_CONFIG_SYSTEM")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    let output = child.output().expect("spawn isolated child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "textconv child failed:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("1 passed"),
        "textconv child did not run:\n{stdout}\n{stderr}"
    );
}

#[test]
#[ignore = "child of hover_and_file_diff_do_not_run_textconv; isolated config"]
fn child_hover_and_file_diff_do_not_run_textconv() {
    if env::var_os(ISOLATED).is_none() {
        return;
    }

    let scratch = TempDir::new().expect("scratch");
    let root = scratch.path().join("repo");
    fs::create_dir(&root).expect("repo dir");
    let git = cthulhu_git::git::Git::discover().expect("system git");
    let git_bin = git.path.clone();

    fixture_git(&git_bin, scratch.path(), &root, &["init", "-b", "main"]);

    let marker = scratch.path().join("textconv-marker");
    let script = scratch.path().join("pwn.sh");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\nprintf 'pwned\\n' >> {}\ncat\n",
            shell_single_quote(&marker.display().to_string())
        ),
    );
    let script_arg = script.display().to_string();
    fixture_git(
        &git_bin,
        scratch.path(),
        &root,
        &["config", "diff.pwn.textconv", &script_arg],
    );
    fs::write(root.join(".gitattributes"), "* diff=pwn\n").expect("gitattributes");
    fs::write(root.join("note.txt"), "hello\n").expect("tracked file");
    fixture_git(
        &git_bin,
        scratch.path(),
        &root,
        &["add", "note.txt", ".gitattributes"],
    );
    fixture_git(&git_bin, scratch.path(), &root, &["commit", "-m", "one"]);
    let oid = fixture_git(&git_bin, scratch.path(), &root, &["rev-parse", "HEAD"]);
    assert!(!marker.exists(), "marker must start absent");

    let hover = commit_hover(&git, &root, &oid).expect("commit hover parses");
    let diff = file_diff(&git, &root, &oid, "note.txt").expect("file diff parses");

    assert!(!marker.exists(), "textconv ran while reading the commit");
    assert_eq!(hover.oid, oid);
    assert!(
        diff.lines.iter().any(|line| line.text == "hello"),
        "file diff did not contain the committed line: {diff:?}"
    );
}

const MANY_BRANCHES: usize = 30;

/// Characterization. `branches()` runs one `for-each-ref` and then one
/// `rev-list --count` per local branch, so thirty branches spawn 31
/// processes. `rev-list --count` on many tips is one union, and walking
/// `rev-list --all` in process would replace the counter and can exceed the
/// 8 MiB output cap. The assertion is the current cost, not a tighter bound.
#[test]
fn many_branches_do_not_spawn_one_rev_list_each() {
    let tmp = TempDir::new().expect("temp");
    let link = link_fake(tmp.path(), "fake-git");
    let mut refs = Vec::new();
    for index in 0..MANY_BRANCHES {
        let name = format!("b{index:02}");
        let oid = format!("{:040x}", index + 1);
        refs.extend(name.as_bytes());
        refs.push(0x1f);
        refs.extend(oid.as_bytes());
        refs.extend_from_slice(b"\x1f\x1f\x1f1700000000\n");
    }
    fs::write(sidecar(&link, ".refs"), refs).expect("refs sidecar");
    let invocations = sidecar(&link, ".invocations");
    fs::write(&invocations, "").expect("truncate invocations");

    let git = probe_fake(&link);
    fs::write(&invocations, "").expect("drop the version probe");
    let root = tmp.path().join("repo");
    fs::create_dir(&root).expect("repo dir");

    let listed = branches(&git, &root, &Head::Branch("b00".to_owned())).expect("branches");
    assert_eq!(listed.len(), MANY_BRANCHES);
    assert!(listed.iter().all(|branch| branch.commit_count == 4));

    let log = fs::read_to_string(&invocations).expect("read invocations");
    let commands: Vec<&str> = log.lines().filter(|line| !line.is_empty()).collect();
    let rev_lists = commands.iter().filter(|line| **line == "rev-list").count();
    let for_each = commands
        .iter()
        .filter(|line| **line == "for-each-ref")
        .count();
    assert_eq!(for_each, 1, "invocations: {commands:?}");
    // Characterization, not the desired bound. See MANY_BRANCHES.
    assert_eq!(
        rev_lists, MANY_BRANCHES,
        "one rev-list --count per branch is still the implementation; invocations: {commands:?}"
    );
    assert_eq!(
        commands.len(),
        1 + MANY_BRANCHES,
        "invocations: {commands:?}"
    );
}
