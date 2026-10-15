//! Links the icon and version info into the Windows executable (GPUI links
//! its manifest: per-monitor DPI, common controls v6).

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let icon = dir.join("../../assets/icon/windows/tern.ico");
    println!("cargo:rerun-if-changed={}", icon.display());

    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts: Vec<&str> = version.split(['.', '-', '+']).take(3).collect();
    parts.resize(3, "0");
    let commas = format!("{},0", parts.join(","));
    // A path as an .rc string literal.
    let lit = |p: &Path| format!("\"{}\"", p.display().to_string().replace('\\', "\\\\"));
    // GPUI loads icon 1 as the window icon.
    let rc = format!(
        r#"1 ICON {icon}
1 VERSIONINFO
FILEVERSION {commas}
PRODUCTVERSION {commas}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904B0"
        BEGIN
            VALUE "CompanyName", "Tern"
            VALUE "FileDescription", "Tern"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "tern"
            VALUE "OriginalFilename", "tern.exe"
            VALUE "ProductName", "Tern"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#,
        icon = lit(&icon),
    );
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("tern.rc");
    std::fs::write(&out, rc).unwrap();
    embed_resource::compile(&out, embed_resource::NONE).manifest_optional().unwrap();
}
