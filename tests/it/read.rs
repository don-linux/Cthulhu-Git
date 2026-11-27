//! What the window reads from an open repository: the graph, branches, the
//! latest commit, hover cards, file diffs, remotes, and the home screen's
//! glance.

use cthulhu_git::git::{
    CommitFile, CommitStats, DiffKind, Divergence, Head, Person, Remote, Upstream, branches,
    commit_hover, file_diff, glance, head_commit_oid, latest_commit, remotes, switch_branch,
};

use crate::support::{Repo, summaries};

/// `cthulhu-demo` with `README.md` committed as "Awaken".
fn awaken() -> Repo {
    let repo = Repo::init("cthulhu-demo");
    repo.write("README.md", "Ph'nglui mglw'nafh\n");
    repo.git(&["add", "README.md"]);
    repo.git(&["commit", "-m", "Awaken"]);
    repo
}

/// Subjects a parser could trip on come back whole, newest first. `git
/// commit` refuses a NUL, so each commit is written as a raw object; git's
/// `%s` ends at the NUL.
#[test]
fn odd_subjects_come_back_whole() {
    let mega = "A".repeat(1024 * 1024);
    let cases: [(&str, Vec<u8>, &str); 5] = [
        (
            "CR inside",
            b"line with CR\r inside\n".to_vec(),
            "line with CR\r inside",
        ),
        (
            "bidi override",
            "before \u{202E} after\n".into(),
            "before \u{202E} after",
        ),
        (
            "ASCII controls",
            "\u{1}\u{7}\u{1b}\u{7f} controls\n".into(),
            "\u{1}\u{7}\u{1b}\u{7f} controls",
        ),
        (
            "NUL",
            b"subject with NUL\x00 and more\n".to_vec(),
            "subject with NUL",
        ),
        ("1 MiB", format!("{mega}\n").into(), &mega),
    ];
    let repo = Repo::init("subjects");
    repo.commit("normal parent");
    let tree = repo.git(&["rev-parse", "HEAD^{tree}"]);
    let mut parent = repo.git(&["rev-parse", "HEAD"]);
    for (_, message, _) in &cases {
        let mut object = format!(
            "tree {tree}\nparent {parent}\n\
             author cthulhu <cthulhu@example.invalid> 1 +0000\n\
             committer cthulhu <cthulhu@example.invalid> 1 +0000\n\n"
        )
        .into_bytes();
        object.extend_from_slice(message);
        parent = repo.hash_commit(&object);
    }
    repo.git(&["update-ref", "refs/heads/main", &parent]);

    let graph = repo.graph(10).expect("graph");
    let rows = summaries(&graph);
    assert_eq!(rows.len(), cases.len() + 1, "one row per commit");
    for ((case, _, summary), row) in cases.iter().rev().zip(&rows) {
        assert!(row == summary, "{case:?}: {:?}", &row[..row.len().min(80)]);
    }
    assert_eq!(rows.last(), Some(&"normal parent"));
}

/// Newest first with the merge listed once, and `limit` rows at most:
/// truncated only when more commits exist, `usize::MAX` without wrapping.
#[test]
fn graph_lists_newest_first_and_stops_at_the_limit() {
    let repo = Repo::init("merge");
    repo.commit_at("base", 1_000_000_000);
    repo.git(&["switch", "-c", "side"]);
    repo.commit_at("side", 1_000_000_100);
    repo.git(&["switch", "main"]);
    repo.commit_at("mainline\n\nThe body is not the summary.", 1_000_000_200);
    repo.git_dated(
        1_000_000_300,
        &["merge", "--no-ff", "side", "-m", "Merge branch side"],
    );
    let head = repo.git(&["rev-parse", "HEAD"]);
    let all = ["Merge branch side", "mainline", "side", "base"];

    // (limit, rows, truncated)
    for (limit, rows, truncated) in [
        (10, 4, false),
        (4, 4, false),
        (usize::MAX, 4, false),
        (3, 3, true),
        (1, 1, true),
        (0, 0, false),
    ] {
        let graph = repo
            .graph(limit)
            .unwrap_or_else(|error| panic!("{limit:?}: {error}"));
        assert_eq!(summaries(&graph), all[..rows], "{limit:?}");
        assert_eq!(graph.truncated, truncated, "{limit:?}");
        if rows > 0 {
            assert_eq!(graph.rows[0].oid, head, "{limit:?}");
        }
    }
}

/// SHA-256 object names are 64 hex digits in the graph, the latest commit,
/// and after a switch.
#[test]
fn sha256_repository_reads_and_switches_with_full_oids() {
    let repo = Repo::new("sha256-demo").initialized(&["--object-format=sha256"]);
    repo.commit("hashed");
    repo.git(&["branch", "other"]);
    let tip = repo.git(&["rev-parse", "HEAD"]);
    assert_eq!(tip.len(), 64, "{tip}");

    let graph = repo.graph(10).expect("graph");
    assert_eq!(graph.rows[0].oid, tip);
    assert_eq!(summaries(&graph), ["hashed"]);

    switch_branch(&repo.git, &repo.root, "other").expect("switch");
    let info = repo.info();
    assert_eq!(info.head, Head::Branch("other".to_owned()));
    let detail = latest_commit(&repo.git, &info.root, &info.head)
        .expect("latest commit")
        .expect("a commit");
    assert_eq!(detail.oid, tip);
}

#[test]
fn unborn_repository_lists_its_branch_and_has_no_commit_detail() {
    let repo = Repo::init("cthulhu-demo");
    let info = repo.info();
    let listed = branches(&repo.git, &info.root, &info.head).expect("branches");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "main");
    assert!(listed[0].current);
    assert_eq!(listed[0].commit_count, 0);
    assert_eq!(listed[0].oid, None);
    assert_eq!(listed[0].committer_time, None);
    assert_eq!(listed[0].upstream, Upstream::None);
    assert_eq!(
        latest_commit(&repo.git, &info.root, &info.head).expect("detail"),
        None
    );
}

#[test]
fn branches_report_commit_counts_and_ahead_behind() {
    let repo = awaken();
    repo.git(&["branch", "feature"]);
    repo.git(&["switch", "feature"]);
    repo.commit("Second\n\nThe body is not part of the summary.");
    repo.git(&["branch", "--set-upstream-to=main"]);

    let info = repo.info();
    let listed = branches(&repo.git, &info.root, &info.head).expect("branches");
    assert_eq!(listed.len(), 2);

    let feature = &listed[0];
    assert_eq!(feature.name, "feature");
    assert!(feature.current);
    assert_eq!(feature.commit_count, 2);
    assert_eq!(
        feature.upstream,
        Upstream::Tracking {
            name: "main".to_owned(),
            remote: false,
            ahead: 1,
            behind: 0,
        }
    );

    let main = &listed[1];
    assert_eq!(main.name, "main");
    assert!(!main.current);
    assert_eq!(main.commit_count, 1);
    assert_eq!(main.upstream, Upstream::None);
    assert!(feature.oid.is_some());
    assert_ne!(feature.oid, main.oid);
    assert!(feature.committer_time.is_some());
    assert!(main.committer_time.is_some());

    let detail = latest_commit(&repo.git, &info.root, &info.head)
        .expect("detail")
        .expect("commit");
    assert_eq!(detail.oid, repo.git(&["rev-parse", "HEAD"]));
    assert_eq!(detail.author.name, "cthulhu");
    assert_eq!(detail.author.email, "cthulhu@example.invalid");
    assert!(detail.co_authors.is_empty());
    assert!(
        detail
            .message
            .starts_with("Second\n\nThe body is not part of the summary."),
        "{:?}",
        detail.message
    );
    let expected_date = repo.git(&[
        "log",
        "-1",
        "--date=format-local:%Y-%m-%d %H:%M",
        "--format=%ad",
    ]);
    assert_eq!(detail.authored_at, expected_date);
}

#[test]
fn graph_and_latest_commit_read_co_author_trailers() {
    let repo = awaken();
    repo.commit(
        "Seal the vault\n\nThe body mentions Co-authored-by: inline, which is not a trailer.\n\n\
         Co-authored-by: Ada Voss <ada@miskatonic.test>\n\
         Co-authored-by: cthulhu <old-address@example.invalid>\n\
         Co-authored-by: Ada Voss <ADA@miskatonic.test>",
    );

    let graph = repo.graph(10).expect("graph");
    let sealed = graph
        .rows
        .iter()
        .find(|row| row.summary == "Seal the vault")
        .expect("sealed row");
    assert_eq!(sealed.author.name, "cthulhu");
    assert_eq!(sealed.author.email, "cthulhu@example.invalid");
    assert_eq!(sealed.author.aliases, ["old-address@example.invalid"]);
    assert_eq!(
        sealed.co_authors,
        [Person::new("Ada Voss", "ada@miskatonic.test")]
    );
    let awakened = graph
        .rows
        .iter()
        .find(|row| row.summary == "Awaken")
        .expect("awaken row");
    assert!(awakened.co_authors.is_empty());
    assert!(awakened.author.aliases.is_empty());

    let info = repo.info();
    let detail = latest_commit(&repo.git, &info.root, &info.head)
        .expect("detail")
        .expect("commit");
    assert_eq!(detail.co_authors, sealed.co_authors);
    assert_eq!(detail.author, sealed.author);
}

#[test]
fn commit_hover_reads_the_message_dates_and_short_stat() {
    let repo = awaken();
    repo.write(
        "README.md",
        "Ph'nglui mglw'nafh\nCthulhu R'lyeh\nwgah'nagl fhtagn\n",
    );
    repo.write("notes/one.md", "one\n");
    repo.git(&["add", "-A"]);
    repo.git(&[
        "commit",
        "-m",
        "Chant\n\nLonger body.\n\nCo-authored-by: Ada Voss <ada@miskatonic.test>",
    ]);
    let info = repo.info();
    let oid = repo.git(&["rev-parse", "HEAD"]);
    let hover = commit_hover(&repo.git, &info.root, &oid).expect("hover");
    assert_eq!(hover.oid, oid);
    assert_eq!(hover.author.name, "cthulhu");
    assert_eq!(
        hover.co_authors,
        [Person::new("Ada Voss", "ada@miskatonic.test")]
    );
    assert!(hover.message.starts_with("Chant\n\nLonger body."));
    assert!(!hover.message.ends_with('\n'));
    assert_eq!(
        hover.stats,
        Some(CommitStats {
            files: 2,
            insertions: 3,
            deletions: 0,
        })
    );
    let unix: i64 = repo
        .git(&["log", "-1", "--format=%at"])
        .parse()
        .expect("unix");
    assert_eq!(hover.authored_unix, unix);

    repo.commit("Nothing changes");
    let empty_oid = repo.git(&["rev-parse", "HEAD"]);
    let empty = commit_hover(&repo.git, &info.root, &empty_oid).expect("empty commit");
    assert_eq!(empty.stats, None);

    assert!(commit_hover(&repo.git, &info.root, "--output=/tmp/x").is_err());
}

/// The merge brings in a file named in NFD. macOS git stores it as NFC; either
/// way the name the hover lists is the one its diff accepts.
#[test]
fn commit_hover_reads_a_root_commit_a_merge_and_an_annotated_tag() {
    let repo = awaken();
    let info = repo.info();
    let root = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["tag", "-a", "v1,0", "-m", "annotated", &root]);
    // A tag and a branch that share a name are each listed by that name.
    repo.git(&["tag", "v1", &root]);
    repo.git(&["branch", "v1"]);
    let graph = repo.graph(10).expect("graph");
    let labels: Vec<&str> = graph.rows[0]
        .branches
        .iter()
        .map(|tip| tip.name.as_str())
        .collect();
    assert_eq!(labels, ["main", "v1"]);

    let hover = commit_hover(&repo.git, &info.root, &root).expect("root");
    assert_eq!(hover.tags, ["v1", "v1,0"]);
    assert_eq!(
        hover.files,
        [CommitFile {
            path: "README.md".to_owned(),
            previous: None,
            insertions: Some(1),
            deletions: Some(0),
        }]
    );
    let diff = file_diff(&repo.git, &info.root, &root, "README.md").expect("patch");
    assert!(
        diff.lines
            .iter()
            .any(|line| line.kind == DiffKind::Added && line.text.contains("Ph'nglui")),
        "{diff:?}"
    );

    let nfd = "topic-e\u{301}.txt";
    repo.git(&["checkout", "-b", "topic"]);
    repo.write(nfd, "from the topic\n");
    repo.git(&["add", "--", nfd]);
    repo.git(&["commit", "-m", "topic work"]);
    repo.git(&["checkout", "main"]);
    repo.git(&["merge", "--no-ff", "-m", "Merge topic", "topic"]);
    let merge = repo.git(&["rev-parse", "HEAD"]);
    let merged = commit_hover(&repo.git, &info.root, &merge).expect("merge");
    let listed = if cfg!(target_os = "macos") {
        "topic-\u{e9}.txt"
    } else {
        nfd
    };
    assert!(
        merged.files.iter().any(|file| file.path == listed),
        "{:?}",
        merged.files
    );
    assert!(merged.stats.is_some_and(|stats| stats.files >= 1));
    assert!(merged.tags.is_empty());
    let diff = file_diff(&repo.git, &info.root, &merge, listed).expect("NFD patch");
    assert!(
        diff.lines
            .iter()
            .any(|line| line.kind == DiffKind::Added && line.text == "from the topic"),
        "{diff:?}"
    );
}

#[test]
fn remotes_are_listed_in_config_order() {
    let repo = awaken();
    let info = repo.info();
    assert!(remotes(&repo.git, &info.root).expect("none").is_empty());

    repo.git(&[
        "remote",
        "add",
        "origin",
        "git@github.com:don-linux/Cthulhu-Git.git",
    ]);
    repo.git(&[
        "remote",
        "add",
        "my.fork",
        "https://github.com/someone/fork.git",
    ]);
    let listed = remotes(&repo.git, &info.root).expect("remotes");
    assert_eq!(
        listed,
        [
            Remote {
                name: "origin".to_owned(),
                url: "git@github.com:don-linux/Cthulhu-Git.git".to_owned(),
            },
            Remote {
                name: "my.fork".to_owned(),
                url: "https://github.com/someone/fork.git".to_owned(),
            },
        ]
    );
}

#[test]
fn graph_authors_follow_the_mailmap() {
    let repo = awaken();
    repo.write(
        ".mailmap",
        "Great Old One <great@rlyeh.test> cthulhu <cthulhu@example.invalid>\n",
    );
    let graph = repo.graph(10).expect("graph");
    assert_eq!(graph.rows[0].author.name, "Great Old One");
    assert_eq!(graph.rows[0].author.email, "great@rlyeh.test");
    // The recorded address stays as an alias, for avatars and for folding.
    assert_eq!(graph.rows[0].author.aliases, ["cthulhu@example.invalid"]);

    repo.commit("Rise\n\nCo-authored-by: cthulhu <cthulhu@example.invalid>");
    let graph = repo.graph(10).expect("graph");
    let risen = graph
        .rows
        .iter()
        .find(|row| row.summary == "Rise")
        .expect("rise row");
    assert!(
        risen.co_authors.is_empty(),
        "the mailmapped author is not their own co-author: {:?}",
        risen.co_authors
    );
}

#[test]
fn graph_shows_where_a_branch_leaves_main() {
    let repo = Repo::init("cthulhu-demo");
    repo.commit_at("Awaken", 1_577_836_800);
    repo.git(&["switch", "-c", "feature"]);
    repo.commit_at("Feature", 1_577_923_200);
    repo.git(&["switch", "main"]);
    repo.commit_at("On main", 1_578_009_600);

    let graph = repo.graph(100).expect("graph");
    let main = graph
        .rows
        .iter()
        .find(|row| row.branches.iter().any(|branch| branch.name == "main"))
        .expect("main tip");
    let feature = graph
        .rows
        .iter()
        .find(|row| row.branches.iter().any(|branch| branch.name == "feature"))
        .expect("feature tip");
    assert_eq!(main.oid, repo.git(&["rev-parse", "main"]));
    assert_eq!(main.commit_lane, 0);
    assert_eq!(main.commit_color, 0);
    assert_eq!(feature.oid, repo.git(&["rev-parse", "feature"]));
    assert_eq!(feature.commit_lane, 1);
    assert_eq!(feature.commit_color, 2);
    assert!(
        feature
            .traces
            .iter()
            .any(|trace| trace.from_lane.is_none() && trace.to_lane == Some(0))
    );

    let feature_at = graph
        .rows
        .iter()
        .position(|row| row.summary == "Feature")
        .expect("feature row");
    assert_eq!(graph.rows[feature_at + 1].summary, "Awaken");
    assert_eq!(graph.rows[feature_at + 1].commit_lane, 0);
    assert!(graph.rows[feature_at + 1].branches.is_empty());
}

/// A detached HEAD keeps its commit on the graph, labelled `HEAD`, beside
/// `main`, and no branch claims to be current.
#[test]
fn detached_head_stays_on_the_graph() {
    let repo = awaken();
    repo.commit("Second");
    repo.git(&["checkout", "--detach", "HEAD~1"]);

    let info = repo.info();
    let listed = branches(&repo.git, &info.root, &info.head).expect("branches");
    assert!(listed.iter().all(|branch| !branch.current), "{listed:?}");
    let head = head_commit_oid(&repo.git, &info.root).expect("head oid");
    assert_eq!(head, repo.git(&["rev-parse", "HEAD"]));
    let graph = repo.graph(10).expect("graph");
    for label in ["main", "HEAD"] {
        assert!(
            graph
                .rows
                .iter()
                .any(|row| row.branches.iter().any(|branch| branch.name == label)),
            "{label:?}"
        );
    }
}

/// The home screen's card for a recent repository.
#[test]
fn glance_reads_head_subject_and_divergence() {
    // (case, setup, HEAD, latest subject, divergence from the upstream)
    type Case = (
        &'static str,
        fn(&Repo),
        Head,
        Option<&'static str>,
        Option<Divergence>,
    );
    let cases: [Case; 3] = [
        (
            "diverged from its upstream",
            |repo| {
                repo.commit("base");
                repo.git(&["checkout", "-b", "feature"]);
                repo.commit("feature work");
                repo.git(&["checkout", "main"]);
                repo.commit("main tip");
                repo.git(&["checkout", "feature"]);
                repo.git(&["branch", "--set-upstream-to=main"]);
            },
            Head::Branch("feature".to_owned()),
            Some("feature work"),
            Some(Divergence {
                ahead: 1,
                behind: 1,
            }),
        ),
        (
            "unborn",
            |_| {},
            Head::Unborn("main".to_owned()),
            None,
            None,
        ),
        (
            "no upstream",
            |repo| repo.commit("Awaken"),
            Head::Branch("main".to_owned()),
            Some("Awaken"),
            None,
        ),
    ];
    for (case, setup, head, subject, upstream) in cases {
        let repo = Repo::init("glance");
        setup(&repo);
        let glance = glance(&repo.git, &repo.root).expect(case);
        assert_eq!(glance.head, head, "{case:?}");
        let latest = glance.latest.as_ref();
        assert_eq!(
            latest.map(|latest| latest.subject.as_str()),
            subject,
            "{case:?}"
        );
        // `YYYY-MM-DD HH:MM`
        assert!(
            latest.is_none_or(|latest| {
                let when = latest.when.as_bytes();
                when.len() == 16 && [when[4], when[7], when[10], when[13]] == *b"-- :"
            }),
            "{case:?}: {latest:?}"
        );
        assert_eq!(glance.upstream, upstream, "{case:?}");
    }
}

/// One `for-each-ref` and at most one more git per branch, never more.
#[cfg(unix)]
#[test]
fn many_branches_spawn_at_most_one_git_per_branch() {
    use std::fs;

    use crate::support::{fake_git, git_at, sidecar};

    const BRANCHES: usize = 30;
    let tmp = tempfile::TempDir::with_prefix("cthulhu-branches-").expect("temp dir");
    let fake = fake_git(&tmp.path().join("git"), "2.43.0", "branches");
    let mut refs = Vec::new();
    for index in 0..BRANCHES {
        refs.extend(
            format!(
                "b{index:02}\x1f{:040x}\x1f\x1f\x1f\x1f1700000000\n",
                index + 1
            )
            .bytes(),
        );
    }
    fs::write(sidecar(&fake, ".refs"), refs).expect("refs sidecar");

    let listed = branches(
        &git_at(fake.clone()),
        tmp.path(),
        &Head::Branch("b00".to_owned()),
    )
    .expect("branches");
    assert_eq!(listed.len(), BRANCHES);
    assert!(listed.iter().all(|branch| branch.commit_count == 4));
    let calls = fs::read_to_string(sidecar(&fake, ".calls")).expect("calls");
    let listings = calls.lines().filter(|call| *call == "for-each-ref").count();
    assert_eq!(listings, 1, "{calls}");
    assert!(
        calls.lines().count() <= 1 + BRANCHES,
        "{} git processes for {BRANCHES} branches",
        calls.lines().count()
    );
}
