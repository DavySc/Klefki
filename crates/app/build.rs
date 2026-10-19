//! Embeds the app icon and version details in the Windows executable, so Explorer, the Start menu and the
//! installer show Klefki's icon instead of the default one. The window icon is set separately at run time.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon/klefki.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon/klefki.ico")
        .set("ProductName", "Klefki")
        .set("FileDescription", "Klefki");
    if let Err(err) = res.compile() {
        // Without the resource the exe still works, but would ship with the default icon: fail the build.
        panic!("embedding the Windows icon failed: {err}");
    }
}
