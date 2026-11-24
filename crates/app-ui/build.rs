fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("../app/ui/app.slint", config)
        .expect("compile ../app/ui/app.slint");
}
