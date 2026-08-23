//! Build script — embeds the Windows app icon into the .exe resource.
//!
//! Looks for `assets/icon.ico`. If present, the icon is compiled into the
//! executable so it appears in the taskbar, Start menu, alt-tab list, and
//! window title bar. If absent, the build still succeeds (icon-less) with
//! a `cargo:warning` so the gap is visible in build logs.
//!
//! Non-Windows targets are a no-op.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(target_os = "windows")]
    {
        let icon_path = std::path::Path::new("assets/icon.ico");
        if !icon_path.exists() {
            println!(
                "cargo:warning=assets/icon.ico not found — the .exe will ship without an embedded icon. \
                 Drop a multi-resolution .ico file there to brand the Windows build."
            );
            return;
        }

        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("FileDescription", "AIS Analytics");
        res.set("ProductName",     "AIS Analytics");
        res.set("CompanyName",     "Bennekrouf");
        res.set("LegalCopyright",  "© Bennekrouf");
        if let Err(e) = res.compile() {
            println!("cargo:warning=Failed to embed Windows icon resource: {} \
                 (the build will continue without an icon)", e);
        }
    }
}
