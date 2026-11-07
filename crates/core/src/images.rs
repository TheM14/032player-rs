//! Artwork loading: download a server size comfortably above the display
//! size, decode and resample to exactly the display box on a blocking
//! thread, so the renderer draws every cover 1:1. The cache
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

/// Server size to fetch for a box whose long side is `px`: an exact match
/// (drawn without resampling), else the smallest size at least 1.25 x
/// `px`. A source only slightly larger than the box goes soft when
/// resampled (512 -> 480 smears every pixel over its neighbours), so
/// there is headroom. `None` (original) above the largest.
pub fn server_size(px: u32) -> Option<u32> {
    if SERVER_SIZES.contains(&px) {
        return Some(px);
    }
    let want = px.saturating_add(px.div_ceil(4));
    SERVER_SIZES.iter().copied().find(|s| *s >= want)
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

    /// URL of the server size for a `width` x `height` box.
    pub(crate) fn url(&self, path: &str, width: u32, height: u32) -> Option<Url> {
        self.client.image_url(path, server_size(width.max(height)))
    }

    /// The artwork as uploaded (no `size`; `size=0` is rejected).
    pub(crate) fn original_url(&self, path: &str) -> Option<Url> {
        self.client.image_url(path, None)
    }

    /// Fetches (or reuses) the artwork at `url` and decodes it to fill a
    /// `width` x `height` box.
    pub(crate) async fn load(&self, url: Url, width: u32, height: u32) -> Result<Rgba, String> {
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
        tokio::task::spawn_blocking(move || decode(&bytes, width, height))
            .await
            .map_err(|e| e.to_string())?
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LruCache<Url, bytes::Bytes>> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Decodes and resamples to exactly `width` x `height`, scaled to cover
/// the box and center-cropped (what `ImageFit.cover` shows). The UI draws
/// the result 1:1 (its boxes are snapped to physical pixels), so this is
/// the only resampling step: use a sharp filter (Catmull-Rom) rather than
/// the renderer's bilinear one, also when a small source has to grow.
/// A zero side keeps the decoded size.
pub fn decode(bytes: &[u8], width: u32, height: u32) -> Result<Rgba, String> {
    let mut image = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    if width > 0 && height > 0 && (image.width(), image.height()) != (width, height) {
        image = image.resize_to_fill(width, height, image::imageops::FilterType::CatmullRom);
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
    fn picks_a_server_size_with_headroom() {
        assert_eq!(server_size(1), Some(256));
        assert_eq!(server_size(256), Some(256));
        assert_eq!(server_size(204), Some(256));
        assert_eq!(server_size(205), Some(512));
        assert_eq!(server_size(480), Some(768));
        assert_eq!(server_size(570), Some(768));
        assert_eq!(server_size(1024), Some(1024));
        assert_eq!(server_size(1300), None);
        assert_eq!(server_size(2000), None);
    }

    #[test]
    fn decodes_and_downscales() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(400, 200, image::Rgba([1, 2, 3, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        // Covers the box, then the sides are cropped.
        let out = decode(&png, 100, 100).unwrap();
        assert_eq!((out.width, out.height), (100, 100));
        assert_eq!(&out.pixels[..4], &[1, 2, 3, 255]);
        // Small sources grow to the box.
        let out = decode(&png, 600, 900).unwrap();
        assert_eq!((out.width, out.height), (600, 900));
        // Zero: as decoded.
        let out = decode(&png, 0, 0).unwrap();
        assert_eq!((out.width, out.height), (400, 200));
        assert!(decode(b"nope", 100, 100).is_err());
    }
}
