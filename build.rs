//! Embeds the Oynx icon in the Windows executable, so Explorer, the taskbar
//! and the installer's shortcuts show it.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon/oynx.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/icon/oynx.ico");
        resource
            .compile()
            .expect("failed to embed the Windows icon resource");
    }
}
