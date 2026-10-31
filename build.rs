fn main() {
    // Windows builds only: embed the icon and version info into the .exe.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Screen Mirror");
        res.set("FileDescription", "Screen Mirror");
        res.compile().expect("failed to embed Windows resources");
    }
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
}
