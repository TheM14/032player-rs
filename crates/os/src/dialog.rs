//! Native file dialogs.

use std::path::PathBuf;

/// Asks for an image file (playlist artwork). Blocks until the dialog
/// closes; `None` when cancelled.
pub fn pick_image() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("选择封面图片")
        .add_filter("图片", &["jpg", "jpeg", "png", "webp"])
        .pick_file()
}
