//! Which remote is a GitHub repository, picked the way VS Code's GitHub
//! extension does: `upstream`, then `origin`, then the first one on GitHub.

use crate::git::Remote;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GitHubRepo {
    pub owner: String,
    pub name: String,
}

impl GitHubRepo {
    pub fn commit_url(&self, oid: &str) -> String {
        format!(
            "https://github.com/{}/{}/commit/{oid}",
            self.owner, self.name
        )
    }

    pub fn issue_url(&self, number: u64) -> String {
        format!(
            "https://github.com/{}/{}/issues/{number}",
            self.owner, self.name
        )
    }

    /// `GET /repos/{owner}/{repo}/commits`. Owner and name were validated, so
    /// they need no escaping.
    pub(crate) fn commits_api(&self) -> String {
        format!(
            "https://api.github.com/repos/{}/{}/commits",
            self.owner, self.name
        )
    }
}

pub fn github_repo(remotes: &[Remote]) -> Option<GitHubRepo> {
    let on_github: Vec<(&str, GitHubRepo)> = remotes
        .iter()
        .filter_map(|remote| parse_github_url(&remote.url).map(|repo| (remote.name.as_str(), repo)))
        .collect();
    for wanted in ["upstream", "origin"] {
        if let Some((_, repo)) = on_github.iter().find(|(name, _)| *name == wanted) {
            return Some(repo.clone());
        }
    }
    on_github.into_iter().next().map(|(_, repo)| repo)
}

/// `https://github.com/o/r(.git)`, `git@github.com:o/r(.git)` and
/// `ssh://git@github.com/o/r(.git)`. A user name, the default port, `www.`
/// and a trailing slash are accepted.
pub fn parse_github_url(url: &str) -> Option<GitHubRepo> {
    let url = url.trim();
    let path = if let Some(rest) = strip_prefix_ignore_case(url, "https://") {
        let (authority, path) = rest.split_once('/')?;
        github_authority(authority, ":443").then_some(path)?
    } else if let Some(rest) = strip_prefix_ignore_case(url, "ssh://") {
        let (authority, path) = rest.split_once('/')?;
        github_authority(authority, ":22").then_some(path)?
    } else {
        let (authority, path) = url.split_once(':')?;
        let host = authority.strip_prefix("git@")?;
        is_github_host(host).then_some(path)?
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    (valid_owner(owner) && valid_name(name)).then(|| GitHubRepo {
        owner: owner.to_owned(),
        name: name.to_owned(),
    })
}

/// `[user@]host[:port]` naming GitHub. Characters a URL treats as the end of
/// the authority (`#`, `?`, `\`, spaces) refuse it, so
/// `https://evil.example#@github.com/o/r` is not taken for GitHub.
fn github_authority(authority: &str, default_port: &str) -> bool {
    if authority
        .chars()
        .any(|c| matches!(c, '#' | '?' | '\\') || c.is_whitespace() || c.is_control())
    {
        return false;
    }
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = host.strip_suffix(default_port).unwrap_or(host);
    is_github_host(host)
}

fn is_github_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("github.com") || host.eq_ignore_ascii_case("www.github.com")
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

/// GitHub logins: letters, digits and single hyphens, up to 39 characters.
fn valid_owner(owner: &str) -> bool {
    (1..=39).contains(&owner.len())
        && owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn valid_name(name: &str) -> bool {
    (1..=100).contains(&name.len())
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(owner: &str, name: &str) -> GitHubRepo {
        GitHubRepo {
            owner: owner.to_owned(),
            name: name.to_owned(),
        }
    }

    fn remote(name: &str, url: &str) -> Remote {
        Remote {
            name: name.to_owned(),
            url: url.to_owned(),
        }
    }

    #[test]
    fn parses_the_github_url_forms() {
        for url in [
            "https://github.com/don-linux/Cthulhu-Git",
            "https://github.com/don-linux/Cthulhu-Git.git",
            "https://github.com/don-linux/Cthulhu-Git/",
            "HTTPS://GitHub.com/don-linux/Cthulhu-Git.git",
            "https://token@github.com/don-linux/Cthulhu-Git.git",
            "git@github.com:don-linux/Cthulhu-Git.git",
            "git@github.com:don-linux/Cthulhu-Git",
            "ssh://git@github.com/don-linux/Cthulhu-Git.git",
            "ssh://git@github.com:22/don-linux/Cthulhu-Git.git",
            "https://www.github.com/don-linux/Cthulhu-Git",
            "https://github.com:443/don-linux/Cthulhu-Git.git",
        ] {
            assert_eq!(
                parse_github_url(url),
                Some(repo("don-linux", "Cthulhu-Git")),
                "{url}"
            );
        }
    }

    #[test]
    fn rejects_other_hosts_and_unsafe_paths() {
        for url in [
            "https://gitlab.com/don-linux/Cthulhu-Git.git",
            "https://github.com.evil.example/a/b",
            "http://github.com/a/b",
            "git@gitlab.com:a/b.git",
            "https://github.com/a",
            "https://github.com/a/b/c",
            "https://github.com/a/..",
            "https://github.com/a%2f/b",
            "https://github.com/a/b?x=1",
            "https://evil.example#@github.com/o/r",
            "https://evil.example?@github.com/o/r",
            "https://evil.example\\@github.com/o/r",
            "https://github.com:8443/o/r",
            "/srv/git/repo.git",
            "",
        ] {
            assert_eq!(parse_github_url(url), None, "{url}");
        }
    }

    #[test]
    fn picks_upstream_then_origin_then_the_first_github_remote() {
        let fork = remote("fork", "https://github.com/me/fork.git");
        let origin = remote("origin", "git@github.com:me/project.git");
        let upstream = remote("upstream", "https://github.com/team/project.git");
        let elsewhere = remote("upstream", "https://gitlab.com/team/project.git");

        assert_eq!(
            github_repo(&[fork.clone(), origin.clone(), upstream.clone()]),
            Some(repo("team", "project"))
        );
        assert_eq!(
            github_repo(&[fork.clone(), elsewhere.clone(), origin.clone()]),
            Some(repo("me", "project"))
        );
        assert_eq!(
            github_repo(&[elsewhere.clone(), fork]),
            Some(repo("me", "fork"))
        );
        assert_eq!(github_repo(&[elsewhere]), None);
        assert_eq!(github_repo(&[]), None);
    }

    #[test]
    fn builds_links() {
        let repo = repo("don-linux", "Cthulhu-Git");
        assert_eq!(
            repo.commit_url("f605456"),
            "https://github.com/don-linux/Cthulhu-Git/commit/f605456"
        );
        assert_eq!(
            repo.issue_url(12),
            "https://github.com/don-linux/Cthulhu-Git/issues/12"
        );
    }
}
