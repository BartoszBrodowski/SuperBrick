// Embeds the icon and version info into sb.exe (Windows targets only; a soft failure
// elsewhere so cross builds without a resource compiler still work).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/SuperBrick.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/SuperBrick.ico")
        .set("ProductName", "SuperBrick")
        .set("FileDescription", "SuperBrick")
        .set("OriginalFilename", "sb.exe")
        .set("InternalName", "sb")
        .set("ProductVersion", &std::env::var("CARGO_PKG_VERSION").unwrap_or_default());
    if let Err(e) = res.compile() {
        println!("cargo:warning=could not embed the Windows icon/version resource: {e}");
    }
}
