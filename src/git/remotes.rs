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

    fn remote(name: &str, url: &str) -> Remote {
        Remote {
            name: name.to_owned(),
            url: url.to_owned(),
        }
    }

    #[test]
    fn parses_names_with_dots_and_keeps_config_order() {
        let stdout = b"remote.origin.url\ngit@github.com:don-linux/Cthulhu-Git.git\0\
                       remote.my.fork.url\nhttps://github.com/someone/fork\0\
                       remote.upstream.url\nhttps://example.invalid/a b.git\0";
        assert_eq!(
            parse_remote_config(stdout),
            [
                remote("origin", "git@github.com:don-linux/Cthulhu-Git.git"),
                remote("my.fork", "https://github.com/someone/fork"),
                remote("upstream", "https://example.invalid/a b.git"),
            ]
        );
    }

    #[test]
    fn skips_malformed_entries() {
        assert!(parse_remote_config(b"").is_empty());
        assert!(parse_remote_config(b"remote.origin.url\0").is_empty());
        assert!(parse_remote_config(b"remote..url\nx\0").is_empty());
        assert!(parse_remote_config(b"remote.origin.url\n\0").is_empty());
        assert!(parse_remote_config(b"core.editor\nvim\0").is_empty());
    }
}
