//! Opening web pages in the default browser and local folders in the file manager.

use std::path::Path;

/// Validates that `raw` is a well-formed `http` or `https` URL with a host.
pub fn validate_http_url(raw: &str) -> Result<url::Url, String> {
    let trimmed = raw.trim();
    let parsed = url::Url::parse(trimmed).map_err(|e| format!("invalid URL: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("disallowed URL scheme: {}", parsed.scheme()));
    }
    if parsed.host().is_none() {
        return Err("URL has no host".into());
    }
    Ok(parsed)
}

/// Opens `url` in the default browser without blocking (032 web console
/// pages, Bangumi, GitHub release page). Refuses any scheme other than
/// `http` or `https`. Failures are logged.
pub fn open_url(url: &str) {
    let parsed = match validate_http_url(url) {
        Ok(u) => u,
        Err(reason) => {
            tracing::warn!(%reason, "refused to open external URL");
            return;
        }
    };
    if let Err(e) = open::that_detached(parsed.as_str()) {
        tracing::warn!("opening {}: {e}", parsed.origin().ascii_serialization());
    }
}

/// Opens a local file or directory in the default system file manager
/// without blocking (for example, the logs directory).
pub fn open_path(path: &Path) {
    if let Err(e) = open::that_detached(path) {
        tracing::warn!("opening {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_http_and_https_urls() {
        assert_eq!(
            validate_http_url("https://bgm.tv/subject/123")
                .unwrap()
                .as_str(),
            "https://bgm.tv/subject/123"
        );
        assert_eq!(
            validate_http_url("  http://127.0.0.1:4533/albums  ")
                .unwrap()
                .as_str(),
            "http://127.0.0.1:4533/albums"
        );
    }

    #[test]
    fn rejects_non_http_schemes_and_local_paths() {
        for bad in [
            "",
            "file:///etc/passwd",
            "file://host/share/evil.exe",
            r"\\host\share\evil.exe",
            r"C:\Windows\System32\cmd.exe",
            "javascript:alert(1)",
            "ms-msdt:/id PCWDiagnostic",
            "mailto:test@example.com",
            "http://",
            "https://",
        ] {
            assert!(
                validate_http_url(bad).is_err(),
                "expected `{bad}` to be rejected"
            );
        }
    }
}
