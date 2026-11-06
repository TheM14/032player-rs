//! Artwork loading: download at the server size covering the display size,
//! decode and downscale to the display size on a blocking thread. The cache
//! keeps the compressed bytes (tens of KB each) rather than decoded pixels,
//! so it can be large; the UI only receives ready RGBA pixels.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use api::{Client, Url};
use lru::LruCache;
use tokio::sync::Semaphore;

/// Decoded RGBA8 image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<[u8]>,
}

/// Artwork sizes the server renders (`capabilities.artwork.sizes`).
const SERVER_SIZES: [u32; 5] = [256, 512, 768, 1024, 1536];

/// Smallest server size covering `px`; `None` (original) above the largest.
pub fn server_size(px: u32) -> Option<u32> {
    SERVER_SIZES.iter().copied().find(|s| *s >= px)
}

pub(crate) struct Images {
    client: Client,
    /// Compressed artwork by URL.
    cache: Mutex<LruCache<Url, bytes::Bytes>>,
    permits: Semaphore,
}

impl Images {
    pub(crate) fn new(client: Client) -> Self {
        Self {
            client,
            cache: Mutex::new(LruCache::new(NonZeroUsize::new(400).expect("non-zero"))),
            permits: Semaphore::new(6),
        }
    }

    pub(crate) fn url(&self, path: &str, px: u32) -> Option<Url> {
        self.client.image_url(path, server_size(px))
    }

    /// The artwork as uploaded (no `size`; `size=0` is rejected).
    pub(crate) fn original_url(&self, path: &str) -> Option<Url> {
        self.client.image_url(path, None)
    }

    /// Fetches (or reuses) the artwork at `url` and decodes it to at most `px`.
    pub(crate) async fn load(&self, url: Url, px: u32) -> Result<Rgba, String> {
        let cached = self.lock().get(&url).cloned();
        let bytes = match cached {
            Some(bytes) => bytes,
            None => {
                let _permit = self.permits.acquire().await.map_err(|e| e.to_string())?;
                let res = self
                    .client
                    .fetch_media(url.clone())
                    .await
                    .map_err(|e| e.to_string())?;
                if !res.status().is_success() {
                    return Err(format!("HTTP {}", res.status().as_u16()));
                }
                let bytes = res.bytes().await.map_err(|e| e.to_string())?;
                self.lock().put(url, bytes.clone());
                bytes
            }
        };
        tokio::task::spawn_blocking(move || decode(&bytes, px))
            .await
            .map_err(|e| e.to_string())?
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LruCache<Url, bytes::Bytes>> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Decodes and shrinks to at most `px` on the long side. The UI draws the
/// result 1:1 (its boxes are snapped to physical pixels), so this is the
/// only resampling step: use a sharp filter (Catmull-Rom) rather than the
/// soft bilinear one.
pub fn decode(bytes: &[u8], px: u32) -> Result<Rgba, String> {
    let mut image = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    if px > 0 && (image.width() > px || image.height() > px) {
        image = image.resize(px, px, image::imageops::FilterType::CatmullRom);
    }
    let rgba = image.into_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(Rgba {
        width,
        height,
        pixels: rgba.into_raw().into(),
    })
}

/// Spotlight backdrop (Electron `.hero-bg img`: the cover at 45% opacity
/// under `blur(64px) saturate(1.3)`). Slint has no blur filter, so the
/// cover is shrunk, blurred and saturated here; the UI stretches the small
/// result, which stays smooth because the image has no detail left.
pub fn backdrop(width: u32, height: u32, rgba: &[u8]) -> Option<Rgba> {
    let image = image::RgbaImage::from_raw(width, height, rgba.to_vec())?;
    // Shrink first (cheap), then blur at a size where the stretched result
    // has no visible steps.
    let small = image::imageops::resize(&image, 24, 24, image::imageops::FilterType::Triangle);
    let mut big = image::imageops::resize(&small, 96, 96, image::imageops::FilterType::Triangle);
    big = image::imageops::blur(&big, 6.0);
    for p in big.pixels_mut() {
        let [r, g, b, _] = p.0;
        let gray = 0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b);
        let sat = |c: u8| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let v = (gray + (f32::from(c) - gray) * 1.3).clamp(0.0, 255.0) as u8;
            v
        };
        p.0 = [sat(r), sat(g), sat(b), 255];
    }
    let (width, height) = big.dimensions();
    Some(Rgba {
        width,
        height,
        pixels: big.into_raw().into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_smallest_covering_server_size() {
        assert_eq!(server_size(1), Some(256));
        assert_eq!(server_size(256), Some(256));
        assert_eq!(server_size(300), Some(512));
        assert_eq!(server_size(2000), None);
    }

    #[test]
    fn decodes_and_downscales() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(400, 200, image::Rgba([1, 2, 3, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let out = decode(&png, 100).unwrap();
        assert_eq!((out.width, out.height), (100, 50));
        assert_eq!(&out.pixels[..4], &[1, 2, 3, 255]);
        assert!(decode(b"nope", 100).is_err());
    }
}
