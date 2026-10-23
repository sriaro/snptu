
#[path = "src/icon.rs"]
mod icon;

use std::{env, fs, path::PathBuf};

const SIZES: [u32; 9] = [16, 20, 24, 32, 40, 48, 64, 128, 256];

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
    for dll in ["mfplat.dll", "mfreadwrite.dll"] {
        println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
    }
    println!("cargo:rustc-link-arg-bins=delayimp.lib");
    println!("cargo:rustc-link-arg-bins=/CETCOMPAT");

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let ico_path = out.join("snptu.ico");
    fs::write(&ico_path, build_ico()).unwrap();

    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let parts: Vec<&str> = version.split('.').collect();
    let (maj, min, pat) = (parts[0], parts[1], parts[2]);
    let rc = format!(
        r#"1 ICON "{ico}"

1 VERSIONINFO
FILEVERSION {maj},{min},{pat},0
PRODUCTVERSION {maj},{min},{pat},0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Snptu"
      VALUE "FileDescription", "Snptu"
      VALUE "ProductName", "Snptu"
      VALUE "InternalName", "Snptu"
      VALUE "LegalCopyright", "Copyright (C) Snptu"
      VALUE "FileVersion", "{version}"
      VALUE "ProductVersion", "{version}"
      VALUE "OriginalFilename", "Snptu.exe"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x0409, 0x04B0
  END
END
"#,
        ico = ico_path.display().to_string().replace('\\', "\\\\"),
    );
    let rc_path = out.join("snptu.rc");
    fs::write(&rc_path, rc).unwrap();
    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_optional()
        .unwrap();
}

fn build_ico() -> Vec<u8> {
    let images: Vec<(u32, Vec<u8>)> = SIZES
        .iter()
        .map(|&s| (s, icon::render(s).encode_png().expect("png encode")))
        .collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (s, png) in &images {
        out.push(if *s >= 256 { 0 } else { *s as u8 });
        out.push(if *s >= 256 { 0 } else { *s as u8 });
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in &images {
        out.extend_from_slice(png);
    }
    out
}
