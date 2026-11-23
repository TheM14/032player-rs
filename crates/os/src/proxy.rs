//! System proxy lookup for mpv.
//!
//! reqwest (the API client) already follows the system proxy, but mpv / ffmpeg
//! only see `http_proxy` and never read the Windows proxy settings. This uses
//! the same matcher as reqwest (hyper-util: `HTTP(S)_PROXY` / `ALL_PROXY` /
//! `NO_PROXY`, then the Windows `Internet Settings` registry key or macOS
//! system configuration), so streams and API requests take the same route.

use hyper_util::client::proxy::matcher::Matcher;

/// The HTTP proxy mpv should use for `url` (`http://host:port`), or `None`
/// for a direct connection. Reads the current settings on every call, so
/// toggling the system proxy takes effect on the next track.
pub fn http_proxy_for(url: &str) -> Option<String> {
    proxy_for(&Matcher::from_system(), url)
}

fn proxy_for(matcher: &Matcher, url: &str) -> Option<String> {
    let uri: http::Uri = url.parse().ok()?;
    let intercept = matcher.intercept(&uri)?;
    let proxy = intercept.uri();
    // ffmpeg only speaks plain HTTP proxies (CONNECT for https streams).
    if !proxy
        .scheme_str()
        .is_some_and(|s| s.eq_ignore_ascii_case("http"))
    {
        tracing::warn!(
            proxy = %proxy,
            "system proxy scheme not supported by mpv, connecting directly"
        );
        return None;
    }
    if intercept.basic_auth().is_some() {
        tracing::warn!(
            proxy = %proxy,
            "proxy credentials are not passed to mpv"
        );
    }
    let authority = proxy.authority()?;
    Some(format!("http://{authority}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(all: &str, no: &str) -> Matcher {
        Matcher::builder().all(all).no(no).build()
    }

    #[test]
    fn bare_host_port_becomes_http_proxy() {
        let m = matcher("127.0.0.1:7890", "");
        assert_eq!(
            proxy_for(&m, "https://music.example.ts.net/rest/stream?id=1").as_deref(),
            Some("http://127.0.0.1:7890")
        );
        assert_eq!(
            proxy_for(&m, "http://100.101.102.103:8080/stream").as_deref(),
            Some("http://127.0.0.1:7890")
        );
    }

    #[test]
    fn no_proxy_list_bypasses() {
        let m = matcher("http://127.0.0.1:7890", "localhost,192.168.0.0/16,.lan");
        assert_eq!(proxy_for(&m, "http://192.168.1.5:4533/stream"), None);
        assert_eq!(proxy_for(&m, "http://nas.lan/stream"), None);
        assert_eq!(proxy_for(&m, "http://localhost:4533/stream"), None);
        assert!(proxy_for(&m, "http://100.64.0.1/stream").is_some());
    }

    #[test]
    fn credentials_are_dropped() {
        let m = matcher("http://user:pass@127.0.0.1:7890", "");
        assert_eq!(
            proxy_for(&m, "https://example.com/").as_deref(),
            Some("http://127.0.0.1:7890")
        );
    }

    #[test]
    fn unsupported_schemes_connect_directly() {
        let m = matcher("socks5://127.0.0.1:7891", "");
        assert_eq!(proxy_for(&m, "https://example.com/"), None);
        let m = matcher("https://proxy.example.com:443", "");
        assert_eq!(proxy_for(&m, "https://example.com/"), None);
    }

    #[test]
    fn no_proxy_configured() {
        let m = Matcher::builder().build();
        assert_eq!(proxy_for(&m, "https://example.com/"), None);
        assert_eq!(proxy_for(&m, "not a url"), None);
    }

    #[test]
    fn uppercase_http_scheme_is_accepted() {
        let m = matcher("HTTP://127.0.0.1:7890", "");
        assert_eq!(
            proxy_for(&m, "https://example.com/stream").as_deref(),
            Some("http://127.0.0.1:7890")
        );
    }
}
