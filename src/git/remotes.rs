//! The repository's remotes, so the history can tell which one is on GitHub.

use std::path::Path;

use super::exec::{Git, GitError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub url: String,
}

/// Every `remote.<name>.url` of this repository, in config order. Only the
/// repository's own config is read: a remote in the global file belongs to
/// no repository in particular. A repository without remotes gives an empty
/// list.
pub fn remotes(git: &Git, root: &Path) -> Result<Vec<Remote>, GitError> {
    let args = [
        "config",
        "--local",
        "-z",
        "--get-regexp",
        r"^remote\..*\.url$",
    ];
    let output = git.run(root, &args)?;
    // `--get-regexp` exits 1 when nothing matches.
    if output.status.code() == Some(1) {
        return Ok(Vec::new());
    }
    if !output.status.success() {
        return Err(GitError::Command {
            command: format!("git {}", args.join(" ")),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(parse_remote_config(&output.stdout))
}

/// Parses `git config -z --get-regexp`: `key\nvalue\0` per entry. A remote
/// name may contain dots, so only the `remote.` prefix and `.url` suffix are
/// removed from the key.
pub(crate) fn parse_remote_config(stdout: &[u8]) -> Vec<Remote> {
    stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let entry = String::from_utf8_lossy(entry);
            let (key, value) = entry.split_once('\n')?;
            let name = key.strip_prefix("remote.")?.strip_suffix(".url")?;
            let url = value.trim();
            if name.is_empty() || url.is_empty() {
                return None;
            }
            Some(Remote {
                name: name.to_owned(),
                url: url.to_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `git config -z --get-regexp` output, and the `(name, url)` remotes
    /// read from it in config order.
    type Remotes = &'static [(&'static str, &'static str)];

    const CONFIGS: &[(&[u8], Remotes)] = &[
        // A remote name may contain dots.
        (
            b"remote.origin.url\ngit@github.com:don-linux/Cthulhu-Git.git\0\
              remote.my.fork.url\nhttps://github.com/someone/fork\0\
              remote.a.b.c.url\nhttps://example.invalid/a b.git\0",
            &[
                ("origin", "git@github.com:don-linux/Cthulhu-Git.git"),
                ("my.fork", "https://github.com/someone/fork"),
                ("a.b.c", "https://example.invalid/a b.git"),
            ],
        ),
        // A value is trimmed, a CR included; a CR or newline inside stays.
        (
            b"remote.fork.url\nhttps://github.com/someone/fork\r\0\
              remote.ssh.url\ngit@github.com:org/repo.git\r\nmore\0\
              remote.origin.url\n  https://example.invalid/spaced  \0\
              remote.cr.mid.url\nhttps://example.invalid/a\rb\0",
            &[
                ("fork", "https://github.com/someone/fork"),
                ("ssh", "git@github.com:org/repo.git\r\nmore"),
                ("origin", "https://example.invalid/spaced"),
                ("cr.mid", "https://example.invalid/a\rb"),
            ],
        ),
        // Entries that are not a remote URL are skipped, not fatal.
        (
            b"\xff\xfe\0core.bare\nfalse\0remote.origin.url\nhttps://example.invalid/x\0",
            &[("origin", "https://example.invalid/x")],
        ),
        (b"", &[]),
        (b"remote.origin.url\0", &[]),
        (b"remote..url\nx\0", &[]),
        (b"remote.origin.url\n\0", &[]),
        (b"remote.origin.url\n  \0", &[]),
        (b"core.editor\nvim\0", &[]),
    ];

    #[test]
    fn parse_remote_config_table() {
        for (stdout, expected) in CONFIGS {
            let expected: Vec<Remote> = expected
                .iter()
                .map(|(name, url)| Remote {
                    name: (*name).to_owned(),
                    url: (*url).to_owned(),
                })
                .collect();
            assert_eq!(parse_remote_config(stdout), expected, "{stdout:?}");
        }
    }
}
