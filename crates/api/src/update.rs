//! The newest release of this app on GitHub (update notice; nothing is
//! downloaded or installed).

use serde::Deserialize;

use crate::ApiError;

/// GitHub REST API root.
pub const GITHUB_API: &str = "https://api.github.com";
/// Repository the releases come from.
pub const RELEASE_REPO: &str = "lux032/032player-rs";

/// A published release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// Tag, e.g. `v0.1.7`.
    pub tag: String,
    /// Release page.
    pub url: String,
}

#[derive(Deserialize)]
struct LatestRelease {
    tag_name: String,
    html_url: String,
}

/// `GET /repos/{repo}/releases/latest` under `api_root`: the newest
/// release that is neither a draft nor a prerelease (the rolling `preview`
/// package is a prerelease).
pub async fn latest_release(api_root: &str, repo: &str) -> Result<Release, ApiError> {
    let http = reqwest::Client::builder()
        .user_agent(concat!("032player-rs/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let url = format!(
        "{}/repos/{repo}/releases/latest",
        api_root.trim_end_matches('/')
    );
    let res = http
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;
    let status = res.status();
    if !status.is_success() {
        return Err(ApiError::Http {
            status: status.as_u16(),
            code: None,
            message: format!("GitHub HTTP {}", status.as_u16()),
        });
    }
    let latest: LatestRelease = res
        .json()
        .await
        .map_err(|e| ApiError::Decode(e.without_url().to_string()))?;
    Ok(Release {
        tag: latest.tag_name,
        url: latest.html_url,
    })
}

/// `tag` (`v1.2.3`, `1.2`) is a newer version than `current` (`1.2.2`, `1.2.3-rc1`).
/// Follows SemVer 2.0.0 pre-release precedence; unparsable tags are never newer.
pub fn is_newer(tag: &str, current: &str) -> bool {
    fn parse(v: &str) -> Option<(Vec<u64>, Option<Vec<&str>>)> {
        let v = v.trim().trim_start_matches(['v', 'V']);
        let without_build = v.split('+').next()?;
        let (core, pre) = match without_build.split_once('-') {
            Some((c, p)) if !p.is_empty() => (c, Some(p.split('.').collect())),
            Some((c, _)) => (c, None),
            None => (without_build, None),
        };
        let nums: Option<Vec<u64>> = core.split('.').map(|p| p.parse().ok()).collect();
        Some((nums?, pre))
    }

    fn cmp_pre(a: &[&str], b: &[&str]) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        for (sa, sb) in a.iter().zip(b.iter()) {
            let ord = match (sa.parse::<u64>(), sb.parse::<u64>()) {
                (Ok(na), Ok(nb)) => na.cmp(&nb),
                (Ok(_), Err(_)) => Ordering::Less,
                (Err(_), Ok(_)) => Ordering::Greater,
                (Err(_), Err(_)) => sa.cmp(sb),
            };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        a.len().cmp(&b.len())
    }

    match (parse(tag), parse(current)) {
        (Some((mut a, pre_a)), Some((mut b, pre_b))) => {
            let n = a.len().max(b.len());
            a.resize(n, 0);
            b.resize(n, 0);
            match a.cmp(&b) {
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Less => false,
                std::cmp::Ordering::Equal => match (pre_a, pre_b) {
                    (None, Some(_)) => true,
                    (Some(pa), Some(pb)) => cmp_pre(&pa, &pb) == std::cmp::Ordering::Greater,
                    _ => false,
                },
            }
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("v0.1.7", "0.1.6"));
        assert!(is_newer("v0.2", "0.1.9"));
        assert!(is_newer("v1.0.0", "0.9.12"));
        assert!(!is_newer("v0.1.6", "0.1.6"));
        assert!(!is_newer("v0.1.5", "0.1.6"));
        assert!(!is_newer("preview", "0.1.6"));
        assert!(!is_newer("v0.1.6-rc1", "0.1.6"));
        assert!(is_newer("v0.2.26", "0.2.26-rc1"));
        assert!(is_newer("v0.2.26-rc.2", "0.2.26-rc.1"));
        assert!(is_newer("v0.2.26-beta", "0.2.26-alpha"));
        assert!(!is_newer("v0.2.26-rc.1", "0.2.26-rc.2"));
    }
}
