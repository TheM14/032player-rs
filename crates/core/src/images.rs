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

/// How an artwork image is fitted into its display box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ArtworkFit {
    /// Scale to cover the entire box and center-crop any excess.
    #[default]
    Cover,
    /// Scale to fit inside the box preserving aspect ratio (with near-square
    /// snapping and extreme-aspect clamping).
    Contain,
}

/// Minimum aspect ratio allowed before clamping (`1:2`).
const MIN_ASPECT: f64 = 0.5;
/// Maximum aspect ratio allowed before clamping (`2:1`).
const MAX_ASPECT: f64 = 2.0;
/// Near-square lower bound snapped to `1:1`.
const NEAR_SQUARE_MIN: f64 = 0.94;
/// Near-square upper bound snapped to `1:1`.
const NEAR_SQUARE_MAX: f64 = 1.06;

/// Computes the target `(width, height)` inside `(max_w, max_h)` for
/// [`ArtworkFit::Contain`].
pub fn fitted_size(src_w: u32, src_h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if src_w == 0 || src_h == 0 || max_w == 0 || max_h == 0 {
        return (src_w, src_h);
    }
    let raw_ratio = f64::from(src_w) / f64::from(src_h);
    let ratio = if (NEAR_SQUARE_MIN..=NEAR_SQUARE_MAX).contains(&raw_ratio) {
        1.0
    } else {
        raw_ratio.clamp(MIN_ASPECT, MAX_ASPECT)
    };
    let box_ratio = f64::from(max_w) / f64::from(max_h);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    if box_ratio > ratio {
        let w = (f64::from(max_h) * ratio)
            .round()
            .clamp(1.0, f64::from(max_w)) as u32;
        (w, max_h)
    } else {
        let h = (f64::from(max_w) / ratio)
            .round()
            .clamp(1.0, f64::from(max_h)) as u32;
        (max_w, h)
    }
}

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

    /// Fetches (or reuses) the compressed artwork bytes at `url`.
    pub(crate) async fn fetch_bytes(&self, url: Url) -> Result<bytes::Bytes, String> {
        if let Some(bytes) = self.lock().get(&url).cloned() {
            return Ok(bytes);
        }
        let _permit = self.permits.acquire().await.map_err(|e| e.to_string())?;
        if let Some(bytes) = self.lock().get(&url).cloned() {
            return Ok(bytes);
        }
        let res = self
            .client
            .fetch_media(url.clone())
            .await
            .map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(format!("HTTP {}", res.status().as_u16()));
        }
        let bytes = res.bytes().await.map_err(|e| e.without_url().to_string())?;
        self.lock().put(url, bytes.clone());
        Ok(bytes)
    }

    /// Fetches (or reuses) the artwork at `url` and decodes it for a
    /// `width` x `height` box using `fit`.
    pub(crate) async fn load(
        &self,
        url: Url,
        width: u32,
        height: u32,
        fit: ArtworkFit,
    ) -> Result<Rgba, String> {
        let bytes = self.fetch_bytes(url).await?;
        tokio::task::spawn_blocking(move || decode_fit(&bytes, width, height, fit))
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
/// the only resampling step: use Lanczos3 for downscaling and Catmull-Rom
/// for upscaling instead of relying on the renderer's bilinear filter.
/// A zero side keeps the decoded size.
pub fn decode(bytes: &[u8], width: u32, height: u32) -> Result<Rgba, String> {
    decode_fit(bytes, width, height, ArtworkFit::Cover)
}

/// Decodes and resamples into `width` x `height` using the given [`ArtworkFit`].
/// A zero side keeps the decoded size.
pub fn decode_fit(bytes: &[u8], width: u32, height: u32, fit: ArtworkFit) -> Result<Rgba, String> {
    let mut image = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    if width > 0 && height > 0 {
        let (target_w, target_h) = match fit {
            ArtworkFit::Cover => (width, height),
            ArtworkFit::Contain => fitted_size(image.width(), image.height(), width, height),
        };
        if target_w > 0 && target_h > 0 && (image.width(), image.height()) != (target_w, target_h) {
            let filter = if image.width() >= target_w && image.height() >= target_h {
                image::imageops::FilterType::Lanczos3
            } else {
                image::imageops::FilterType::CatmullRom
            };
            image = image.resize_to_fill(target_w, target_h, filter);
        }
    }
    let rgba = image.into_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(Rgba {
        width,
        height,
        pixels: rgba.into_raw().into(),
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

    #[test]
    fn contain_fits_and_clamps_aspect_ratios() {
        // Tall poster 2:3 fits inside 300x300 as 200x300.
        assert_eq!(fitted_size(400, 600, 300, 300), (200, 300));
        // Wide landscape 3:2 fits inside 300x300 as 300x200.
        assert_eq!(fitted_size(600, 400, 300, 300), (300, 200));
        // Small portrait upscales proportionally.
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(100, 150, image::Rgba([9, 8, 7, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let out = decode_fit(&png, 300, 300, ArtworkFit::Contain).unwrap();
        assert_eq!((out.width, out.height), (200, 300));
        // Near-square (0.94..=1.06) snaps to 1:1.
        assert_eq!(fitted_size(1000, 960, 300, 300), (300, 300));
        assert_eq!(fitted_size(960, 1000, 300, 300), (300, 300));
        // Outside near-square preserves aspect.
        assert_eq!(fitted_size(900, 1000, 300, 300), (270, 300));
        // Extreme ratios clamp to 1:2 and 2:1.
        assert_eq!(fitted_size(100, 500, 300, 300), (150, 300));
        assert_eq!(fitted_size(500, 100, 300, 300), (300, 150));
    }
}
