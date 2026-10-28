//! Opening web pages in the default browser.

/// Opens `url` in the default browser without blocking (032 web console
/// pages, Bangumi). Failures are logged.
pub fn open_url(url: &str) {
    if let Err(e) = open::that_detached(url) {
        tracing::warn!("opening {url}: {e}");
    }
}
