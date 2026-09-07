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

/// Set by the release workflow to the tag being built, e.g. `v1.2.0`.
const RELEASE_TAG_VAR: &str = "BATTERY_TRAY_RELEASE";

/// Which kind of build this is, and what it should call itself.
///
/// A release build is one made from a tag by the workflow, and names itself
/// after that tag. Anything else is a development build, and names itself
/// after the commit it came from -- so a binary someone sends you can always
/// be traced back to the source it was built from, which a bare `0.1.0`
/// shared by every local build cannot.
struct Build {
    /// `release` or `dev`.
    channel: &'static str,
    /// What the About page shows.
    display: String,
    /// The four numeric fields `VERSIONINFO` requires.
    quad: String,
}

fn describe_build() -> Build {
    println!("cargo:rerun-if-env-changed={RELEASE_TAG_VAR}");
    let pkg = env!("CARGO_PKG_VERSION");

    let quad_from = |v: &str| {
        let mut parts: Vec<String> = v
            .trim_start_matches('v')
            .split(['.', '-', '+'])
            .take_while(|p| p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty())
            .map(|p| p.to_string())
            .collect();
        parts.truncate(4);
        while parts.len() < 4 {
            parts.push("0".into());
        }
        parts.join(",")
    };

    let tag = std::env::var(RELEASE_TAG_VAR).ok().filter(|t| !t.trim().is_empty());
    match tag {
        Some(tag) => {
            let tag = tag.trim().to_string();
            Build { channel: "release", quad: quad_from(&tag), display: tag }
        }
        None => {
            let commit = git_commit();
            let display = match &commit {
                Some(c) => format!("{pkg}-dev ({c})"),
                // No git and no tag: a source drop built by hand.
                None => format!("{pkg}-dev"),
            };
            Build { channel: "dev", quad: quad_from(pkg), display }
        }
    }
}

/// The short commit id, if this is a git checkout with git available.
///
/// Failure is expected and not an error: source archives have no `.git`, and
/// build machines need not have git installed. The build just falls back to
/// naming itself after the crate version alone.
fn git_commit() -> Option<String> {
    // Rebuild when HEAD moves, so a dev build never reports a stale commit.
    let git_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.git");
    if git_dir.join("HEAD").exists() {
        println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
        // On a branch, HEAD is a pointer; the ref it names is what actually
        // changes on commit.
        if let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD")) {
            if let Some(r) = head.trim().strip_prefix("ref: ") {
                println!("cargo:rerun-if-changed={}", git_dir.join(r).display());
            }
        }
    }

    let out = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let id = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!id.is_empty()).then_some(id)
}

/// A `VERSIONINFO` block, which is what Explorer's Details tab and Task
/// Manager read to name a process. Without one the app shows as a bare
/// filename with nothing beside it.
fn version_info(build: &Build) -> String {
    let quad = &build.quad;
    let v = &build.display;
    format!(
        r#"1 VERSIONINFO
FILEVERSION {quad}
PRODUCTVERSION {quad}
FILEOS 0x4L
FILETYPE 0x1L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Antoinenz"
      VALUE "FileDescription", "BatteryTray - battery time remaining"
      VALUE "FileVersion", "{v}"
      VALUE "InternalName", "battery-tray"
      VALUE "LegalCopyright", "Copyright (c) 2026 Antoinenz. MIT licence."
      VALUE "OriginalFilename", "battery-tray.exe"
      VALUE "ProductName", "BatteryTray"
      VALUE "ProductVersion", "{v}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    )
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../battery-core/src/glyph.rs");

    // Handed to the crate so the About page and the resource agree.
    let build = describe_build();
    println!("cargo:rustc-env=BUILD_VERSION={}", build.display);
    println!("cargo:rustc-env=BUILD_CHANNEL={}", build.channel);

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
    // the process manifest; then the version block.
    let rc = format!(
        "1 ICON \"{}\"\n1 24 \"{}\"\n{}",
        rc_literal(&ico_path),
        rc_literal(&manifest_path),
        version_info(&build)
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
