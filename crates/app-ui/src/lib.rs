//! Compiled Slint UI module (`crates/app/ui/app.slint`).
//!
//! Kept in its own leaf crate so Rust edits in `app`, `client-core`, `player`,
//! `os`, and release version bumps do not recompile the generated Slint code,
//! and so Cargo profiles can tune codegen and debuginfo independently of hot
//! runtime crates.

#[allow(clippy::all, clippy::todo)]
mod generated {
    slint::include_modules!();
}

pub use generated::*;
