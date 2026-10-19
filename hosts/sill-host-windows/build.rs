//! Two things the binaries need from the linker on Windows (MSVC):
//! - Common Controls v6 through the manifest, so the pairing window's buttons are drawn in the
//!   system's own look (without it Windows draws them as Windows 95 did);
//! - Sill.exe's icon and version (res/Sill.ico, the Mac app's icon cut to its rounded square; the
//!   version from Cargo.toml): what Explorer, Task Manager and the tray's overflow list show.
//!
//! The GNU target (a cross-check from macOS) builds without either.

fn main() {
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=res/Sill.ico");
    if !(windows && msvc) {
        return;
    }
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
    );
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let icon = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR")).join("res").join("Sill.ico");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let mut parts: Vec<u32> = version.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    parts.resize(4, 0);
    let numeric = parts.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let rc = format!(
        r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS 0x40004L
FILETYPE 0x1L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "Sill"
      VALUE "FileDescription", "Sill"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "Sill"
      VALUE "LegalCopyright", "Apache-2.0"
      VALUE "OriginalFilename", "Sill.exe"
      VALUE "ProductName", "Sill"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = icon.display().to_string().replace('\\', "\\\\"),
    );
    let rc_path = out.join("sill.rc");
    std::fs::write(&rc_path, rc).expect("sill.rc");
    embed_resource::compile_for(&rc_path, ["Sill"], embed_resource::NONE)
        .manifest_required()
        .expect("Sill.exe's icon and version resource");
}
