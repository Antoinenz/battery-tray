//! Generates the application icon and manifest, and embeds them.
//!
//! The icon is rendered from `battery_core::glyph`, the same code the tray and
//! window icons use, so there is one definition of the app's appearance rather
//! than a hand-drawn asset that can drift from it.

use std::path::{Path, PathBuf};

/// Common Controls v6 is what makes standard Win32 controls render themed
/// rather than in the Windows 95 style, and the manifest is the supported way
/// to declare per-monitor DPI awareness.
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0" processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </windowsSettings>
  </application>
</assembly>
"#;

/// ICO entries are uncompressed 32bpp bitmaps, so each size costs `w * h * 4`
/// bytes in the executable. A 256 px entry alone is 262 KB -- nearly the size
/// of the whole app -- and Explorer is content to scale 128 up on the rare
/// views that want more.
const SIZES: [i32; 7] = [16, 20, 24, 32, 48, 64, 128];

fn rc_literal(p: &Path) -> String {
    p.display().to_string().replace('\\', "\\\\")
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../battery-core/src/glyph.rs");

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let images: Vec<(i32, Vec<u8>)> = SIZES
        .iter()
        .map(|&s| (s, battery_core::glyph::app_icon_rgba(s)))
        .collect();
    let ico = battery_core::glyph::encode_ico(&images);

    let ico_path = out.join("logo.ico");
    if let Err(e) = std::fs::write(&ico_path, &ico) {
        println!("cargo:warning=could not write icon: {e}");
        return;
    }

    // Also drop a copy beside the sources so the artwork is available to
    // shortcuts and installers without running a build.
    let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets");
    if std::fs::create_dir_all(&assets).is_ok() {
        let _ = std::fs::write(assets.join("logo.ico"), &ico);
    }

    let manifest_path = out.join("app.manifest");
    if let Err(e) = std::fs::write(&manifest_path, MANIFEST) {
        println!("cargo:warning=could not write manifest: {e}");
        return;
    }

    // Resource ids: 1 = the application icon; id 1 of type 24 (RT_MANIFEST) =
    // the process manifest.
    let rc = format!(
        "1 ICON \"{}\"\n1 24 \"{}\"\n",
        rc_literal(&ico_path),
        rc_literal(&manifest_path)
    );
    let rc_path = out.join("app.rc");
    if let Err(e) = std::fs::write(&rc_path, rc) {
        println!("cargo:warning=could not write resource script: {e}");
        return;
    }

    // Embedding needs the SDK resource compiler. Without it the app still
    // builds and still sets its window icons at runtime; it loses only the
    // Explorer icon and themed controls, so this must not fail the build.
    if let Err(e) = embed_resource::compile(&rc_path, embed_resource::NONE).manifest_optional() {
        println!("cargo:warning=resources not embedded: {e}");
    }
}
