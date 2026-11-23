//! Provides libmpv for linking.
//!
//! Windows: downloads a pinned mpv-dev archive (shinchiro builds, mirrored
//! permanently on SourceForge), verifies its SHA-256, extracts it into
//! `target/libmpv/<build>/`, adds it to the link search path and copies
//! `libmpv-2.dll` next to the binaries. Other platforms link the system
//! libmpv (`libmpv-dev` / `mpv` package).
//!
//! Overrides:
//!  - `MPV_LIB_DIR`: directory with `mpv.lib` (MSVC) or `libmpv.dll.a`
//!    (GNU) and `libmpv-2.dll`; skips the download.
//!  - `MPV_DEV_URL` + `MPV_DEV_SHA256`: another mpv-dev archive.

use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const BUILD: &str = "20261018-git-01e2533f31";
const BASE_URL: &str = "https://sourceforge.net/projects/mpv-player-windows/files/libmpv";

/// (target arch, archive arch, sha256)
const ARCHIVES: &[(&str, &str, &str)] = &[
    (
        "x86_64",
        "x86_64",
        "d7f5621f42b6e161fe032df914a10bdece28a89df3e57c6c3e16a5a250929b5e",
    ),
    (
        "aarch64",
        "aarch64",
        "062b07022d1fadefd341485602658a51d973c216bb4282859e10daa09ad55478",
    ),
];

const DLL: &str = "libmpv-2.dll";

fn main() {
    for var in ["MPV_LIB_DIR", "MPV_DEV_URL", "MPV_DEV_SHA256"] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    println!("cargo:rerun-if-changed=build.rs");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let lib_dir = match env::var_os("MPV_LIB_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => fetch().unwrap_or_else(|e| panic!("libmpv download failed: {e}")),
    };
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    copy_dll(&lib_dir);
}

/// `target/` (or `$CARGO_TARGET_DIR`) and `target/<triple?>/<profile>/`,
/// derived strictly from absolute `OUT_DIR` so relative `CARGO_TARGET_DIR`
/// values never resolve against `crates/player/`.
/// Handles both classic (`<profile>/build/<pkg>-<hash>/out`) and fine-grained
/// (`<profile>/build/<pkg>/<hash>/out`) Cargo layouts.
fn target_dirs() -> (PathBuf, PathBuf) {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let profile_dir = out
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == "build"))
        .and_then(Path::parent)
        .or_else(|| out.ancestors().nth(3))
        .expect("OUT_DIR layout")
        .to_path_buf();
    let target_triple = env::var_os("TARGET");
    let target_dir = match profile_dir.parent() {
        Some(parent) => {
            if target_triple
                .as_deref()
                .is_some_and(|t| parent.file_name() == Some(t))
            {
                parent.parent().unwrap_or(parent).to_path_buf()
            } else {
                parent.to_path_buf()
            }
        }
        None => profile_dir.clone(),
    };
    (target_dir, profile_dir)
}

fn fetch() -> Result<PathBuf, String> {
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let (url, sha256, name) = match (env::var("MPV_DEV_URL"), env::var("MPV_DEV_SHA256")) {
        (Ok(url), Ok(sha)) => (url, sha.to_lowercase(), "custom".to_owned()),
        (Ok(_), Err(_)) => return Err("MPV_DEV_URL needs MPV_DEV_SHA256".into()),
        _ => {
            let (_, archive_arch, sha) = ARCHIVES
                .iter()
                .find(|(a, _, _)| *a == arch)
                .ok_or_else(|| format!("no pinned libmpv for arch {arch}; set MPV_LIB_DIR"))?;
            let file = format!("mpv-dev-{archive_arch}-{BUILD}.7z");
            (
                format!("{BASE_URL}/{file}/download"),
                (*sha).to_owned(),
                format!("{archive_arch}-{BUILD}"),
            )
        }
    };

    let (target_dir, _) = target_dirs();
    let dir = target_dir.join("libmpv").join(&name);
    let marker = dir.join(".sha256");
    if fs::read_to_string(&marker).is_ok_and(|s| s.trim() == sha256) && dir.join(DLL).exists() {
        return Ok(dir);
    }

    println!("cargo:warning=downloading libmpv: {url}");
    let bytes = download(&url)?;
    let actual = hex(&Sha256::digest(&bytes));
    if actual != sha256 {
        return Err(format!(
            "checksum mismatch for {url}: expected {sha256}, got {actual}"
        ));
    }

    let parent = dir.parent().ok_or("bad libmpv target directory")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let staging = parent.join(format!("{name}.tmp-{}-{nonce}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let archive = staging.join("mpv-dev.7z");
    fs::write(&archive, &bytes).map_err(|e| e.to_string())?;
    sevenz_rust2::decompress_file(&archive, &staging).map_err(|e| e.to_string())?;
    fs::remove_file(&archive).map_err(|e| e.to_string())?;

    let dll = find(&staging, DLL).ok_or("libmpv-2.dll not in archive")?;
    let import = find(&staging, "libmpv.dll.a").ok_or("libmpv.dll.a not in archive")?;
    let root = dll.parent().ok_or("bad archive layout")?.to_path_buf();
    // GNU ld finds libmpv.dll.a; MSVC link.exe reads the same COFF import
    // archive under the name it looks for.
    fs::copy(&import, root.join("mpv.lib")).map_err(|e| e.to_string())?;
    fs::write(root.join(".sha256"), &sha256).map_err(|e| e.to_string())?;
    if let Err(rename_err) = fs::rename(&root, &dir) {
        // Another concurrent build (e.g. rust-analyzer) may have published `dir` first.
        if !(fs::read_to_string(&marker).is_ok_and(|s| s.trim() == sha256)
            && dir.join(DLL).exists())
        {
            let _ = fs::remove_dir_all(&dir);
            fs::rename(&root, &dir).map_err(|e| format!("{rename_err}; retry: {e}"))?;
        }
    }
    let _ = fs::remove_dir_all(&staging);
    Ok(dir)
}

fn download(url: &str) -> Result<Vec<u8>, String> {
    let mut response = ureq::get(url).call().map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .with_config()
        .limit(256 * 1024 * 1024)
        .reader()
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find(&path, name) {
                return Some(found);
            }
        } else if path
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case(name))
        {
            return Some(path);
        }
    }
    None
}

/// Puts the DLL where `cargo run` / `cargo test` binaries look for it.
fn copy_dll(lib_dir: &Path) {
    let src = lib_dir.join(DLL);
    if !src.exists() {
        println!("cargo:warning={} not found in {}", DLL, lib_dir.display());
        return;
    }
    let (_, profile_dir) = target_dirs();
    for dir in [profile_dir.clone(), profile_dir.join("deps")] {
        let dst = dir.join(DLL);
        if dst.metadata().map(|m| m.len()).ok() == src.metadata().map(|m| m.len()).ok() {
            continue;
        }
        if fs::create_dir_all(&dir).is_ok()
            && let Err(e) = fs::copy(&src, &dst)
        {
            // A running binary keeps the DLL locked; the old copy still works.
            println!(
                "cargo:warning=could not copy {DLL} to {}: {e}",
                dir.display()
            );
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
