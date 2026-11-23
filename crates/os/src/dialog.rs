//! Native file dialogs.

use std::future::Future;
use std::path::PathBuf;

use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

/// Asks for an image file (playlist artwork) asynchronously without blocking
/// the UI event loop. When `parent` is provided, the dialog attaches to the
/// application window so it stays above it.
pub fn pick_image_async<W>(parent: Option<&W>) -> impl Future<Output = Option<PathBuf>> + 'static
where
    W: HasWindowHandle + HasDisplayHandle,
{
    let mut dialog = rfd::AsyncFileDialog::new()
        .set_title("选择封面图片")
        .add_filter("图片", &["jpg", "jpeg", "png", "webp"]);
    if let Some(parent) = parent {
        dialog = dialog.set_parent(parent);
    }
    async move { dialog.pick_file().await.map(|f| f.path().to_path_buf()) }
}
