fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("compile ui/app.slint");
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
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico")
        .set("ProductName", "032player")
        .set("FileDescription", "032player")
        .set("CompanyName", "032")
        .set("LegalCopyright", "032player");
    res.compile().expect("compile Windows resources");
}
