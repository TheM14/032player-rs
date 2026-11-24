fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    windows_resources();
}

/// Icon and version info of `032player.exe` (Explorer, taskbar, installer).
#[cfg(windows)]
fn windows_resources() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION");
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico")
        .set("ProductName", "032player")
        .set("FileDescription", "032player")
        .set("CompanyName", "032")
        .set("LegalCopyright", "032player");
    res.compile().expect("compile Windows resources");
}
