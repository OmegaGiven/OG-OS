// Embeds the app icon and version info into og-files.exe on Windows, so
// Explorer, the taskbar and the installer show the OG Files icon.
fn main() {
    println!("cargo:rerun-if-changed=packaging/icons/og-files.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        #[cfg(windows)]
        {
            let mut res = winresource::WindowsResource::new();
            res.set_icon("packaging/icons/og-files.ico");
            res.set("ProductName", "OG Files");
            res.set("FileDescription", "OG Files");
            res.set("CompanyName", "OmegaGiven");
            res.compile().expect("embedding the Windows icon");
        }
    }
}
