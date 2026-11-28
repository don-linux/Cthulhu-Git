//! Avatar URLs that need no API call: GitHub no-reply addresses, a few
//! known agents, and Gravatar.

use std::fmt::Write as _;

use sha2::{Digest, Sha256};

/// Pixels asked for. Twice the 40-point hover avatar, so it stays sharp at 2x.
const AVATAR_PX: u32 = 96;

const NOREPLY_DOMAIN: &str = "@users.noreply.github.com";

/// Addresses whose GitHub account is known but not derivable from the address.
/// Each id was checked against `https://github.com/{login}.png`.
const KNOWN_ACCOUNTS: [(&str, &str); 1] = [("cursoragent@cursor.com", "199161495")];

/// Lowercase hex SHA-256 of `text`.
pub fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// `{id}+{login}@users.noreply.github.com`, the form GitHub has used since
/// 2017. Same rule as VS Code's GitHub extension.
pub fn noreply_user_id(email_key: &str) -> Option<&str> {
    let local = email_key.strip_suffix(NOREPLY_DOMAIN)?;
    let (id, login) = local.split_once('+')?;
    (!id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()) && !login.is_empty())
        .then_some(id)
}

pub fn github_user_avatar(id: &str) -> String {
    format!("https://avatars.githubusercontent.com/u/{id}?s={AVATAR_PX}")
}

/// An avatar URL the address alone gives away. `email_key` is trimmed and
/// lowercased ([`crate::git::email_key`]).
pub fn direct_avatar_url(email_key: &str) -> Option<String> {
    if let Some(id) = noreply_user_id(email_key) {
        return Some(github_user_avatar(id));
    }
    if let Some((_, id)) = KNOWN_ACCOUNTS
        .iter()
        .find(|(address, _)| *address == email_key)
    {
        return Some(github_user_avatar(id));
    }
    // The older `{login}@users.noreply.github.com`. github.com redirects
    // `/{login}.png` to the avatar host.
    let login = email_key.strip_suffix(NOREPLY_DOMAIN)?;
    let valid = (1..=39).contains(&login.len())
        && login
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    valid.then(|| format!("https://github.com/{login}.png?size={AVATAR_PX}"))
}

/// Gravatar's documented form: SHA-256 of the trimmed, lowercased address,
/// and `d=404` so a missing avatar is an error instead of a placeholder.
pub fn gravatar_url(email_key: &str) -> String {
    format!(
        "https://gravatar.com/avatar/{}?s={AVATAR_PX}&d=404",
        sha256_hex(email_key)
    )
}

/// An `avatar_url` from the API, asked at [`AVATAR_PX`].
pub fn sized(avatar_url: &str) -> String {
    let separator = if avatar_url.contains('?') { '&' } else { '?' };
    format!("{avatar_url}{separator}s={AVATAR_PX}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_avatar_urls_skip_the_api() {
        for (email, direct) in [
            // A noreply address with an id goes straight to the avatar host.
            (
                "194445529+don-linux@users.noreply.github.com",
                Some("https://avatars.githubusercontent.com/u/194445529?s=96"),
            ),
            (
                "49699333+dependabot[bot]@users.noreply.github.com",
                Some("https://avatars.githubusercontent.com/u/49699333?s=96"),
            ),
            // An older one, with only the login, uses the profile redirect.
            (
                "don-linux@users.noreply.github.com",
                Some("https://github.com/don-linux.png?size=96"),
            ),
            ("a/b@users.noreply.github.com", None),
            ("@users.noreply.github.com", None),
            (
                "cursoragent@cursor.com",
                Some("https://avatars.githubusercontent.com/u/199161495?s=96"),
            ),
            ("someone@cursor.com", None),
            ("ada@miskatonic.test", None),
        ] {
            assert_eq!(direct_avatar_url(email).as_deref(), direct, "{email}");
        }
        for email in [
            "+login@users.noreply.github.com",
            "12a+login@users.noreply.github.com",
            "123+@users.noreply.github.com",
            "123+login@users.noreply.github.com.evil",
        ] {
            assert_eq!(noreply_user_id(email), None, "{email}");
        }
    }

    #[test]
    fn gravatar_hashes_the_address_and_api_avatars_get_a_size() {
        // Gravatar's documented example hash for "MyEmailAddress@example.com".
        assert_eq!(
            sha256_hex("myemailaddress@example.com"),
            "84059b07d4be67b806386c0aad8070a23f18836bbaae342275dc0a83414c32ee"
        );
        assert_eq!(
            gravatar_url("myemailaddress@example.com"),
            "https://gravatar.com/avatar/84059b07d4be67b806386c0aad8070a23f18836bbaae342275dc0a83414c32ee?s=96&d=404"
        );
        for (url, sized_url) in [
            (
                "https://avatars.githubusercontent.com/u/1?v=4",
                "https://avatars.githubusercontent.com/u/1?v=4&s=96",
            ),
            (
                "https://avatars.githubusercontent.com/u/1",
                "https://avatars.githubusercontent.com/u/1?s=96",
            ),
        ] {
            assert_eq!(sized(url), sized_url, "{url}");
        }
    }
}
