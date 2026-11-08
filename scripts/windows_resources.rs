//! Writes .res files for the MSVC linker without invoking a resource compiler.

use std::path::{Path, PathBuf};

pub fn manifest_fields(manifest_path: &Path) -> impl Fn(&[&str]) -> String + use<> {
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    let text = std::fs::read_to_string(manifest_path).expect("setup/install-manifest.json is required");
    let manifest: serde_json::Value = serde_json::from_str(&text).expect("install-manifest.json is not valid JSON");
    move |path: &[&str]| -> String {
        let mut value = &manifest;
        for key in path {
            value = &value[*key];
        }
        value.as_str().unwrap_or_else(|| panic!("install-manifest.json needs {}", path.join("."))).to_string()
    }
}

pub fn new_resource() -> Vec<u8> {
    let mut res = Vec::new();
    entry(&mut res, 0, 0, &[], 0); // A .res file starts with an empty entry.
    res
}

pub fn link_resource(res: &[u8], cargo_link_arg: &str) {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("metadata.res");
    std::fs::write(&out, res).unwrap();
    println!("cargo:{cargo_link_arg}={}", out.display());
}

fn pad4(v: &mut Vec<u8>) {
    while v.len() % 4 != 0 {
        v.push(0);
    }
}

fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}

/// One resource: a 32-byte header with ordinal type and name, then the data, padded.
pub fn entry(out: &mut Vec<u8>, kind: u16, id: u16, data: &[u8], flags: u16) {
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(32u32.to_le_bytes());
    out.extend([0xFF, 0xFF]);
    out.extend(kind.to_le_bytes());
    out.extend([0xFF, 0xFF]);
    out.extend(id.to_le_bytes());
    out.extend(0u32.to_le_bytes()); // data version
    out.extend(flags.to_le_bytes());
    out.extend(0u16.to_le_bytes()); // neutral language
    out.extend(0u32.to_le_bytes()); // version
    out.extend(0u32.to_le_bytes()); // characteristics
    out.extend(data);
    pad4(out);
}

/// A VERSIONINFO node: lengths, type, key, value and children, each DWORD aligned.
fn node(key: &str, value: &[u8], value_len: u16, text: bool, children: &[Vec<u8>]) -> Vec<u8> {
    let mut v = vec![0u8; 6];
    v.extend(utf16z(key));
    pad4(&mut v);
    v.extend(value);
    for child in children {
        pad4(&mut v);
        v.extend(child);
    }
    let len = v.len() as u16;
    v[0..2].copy_from_slice(&len.to_le_bytes());
    v[2..4].copy_from_slice(&value_len.to_le_bytes());
    v[4..6].copy_from_slice(&u16::from(text).to_le_bytes());
    v
}

pub struct FileVersion<'a> {
    pub description: &'a str,
    pub filename: &'a str,
    pub internal_name: &'a str,
    pub language: u16,
    pub is_dll: bool,
}

pub fn add_version(res: &mut Vec<u8>, product: &str, publisher: &str, version: &str, file: FileVersion<'_>) {
    let parts: Vec<u32> = version.split(['.', '-', '+']).take(3).map(|p| p.parse().unwrap_or(0)).collect();
    let (major, minor, patch) = (parts[0], parts[1], parts[2]);
    let mut fixed = Vec::new();
    for value in [
        0xFEEF_04BDu32,
        0x0001_0000,
        (major << 16) | minor,
        patch << 16,
        (major << 16) | minor,
        patch << 16,
        0x3F,
        0,
        0x0004_0004,                     // VOS_NT_WINDOWS32
        if file.is_dll { 2 } else { 1 }, // VFT_DLL or VFT_APP
        0,
        0,
        0,
    ] {
        fixed.extend(value.to_le_bytes());
    }
    let string = |key: &str, value: &str| {
        let data = utf16z(value);
        node(key, &data, (data.len() / 2) as u16, true, &[])
    };
    let strings = [
        string("CompanyName", publisher),
        string("FileDescription", file.description),
        string("FileVersion", version),
        string("InternalName", file.internal_name),
        string("LegalCopyright", &format!("Copyright © 2026 {publisher}. All Rights Reserved.")),
        string("OriginalFilename", file.filename),
        string("ProductName", product),
        string("ProductVersion", version),
    ];
    let table = node(&format!("{:04X}04B0", file.language), &[], 0, true, &strings);
    let string_info = node("StringFileInfo", &[], 0, true, &[table]);
    let [lang_low, lang_high] = file.language.to_le_bytes();
    let translation = node("Translation", &[lang_low, lang_high, 0xB0, 0x04], 4, false, &[]);
    let var_info = node("VarFileInfo", &[], 0, true, &[translation]);
    let info = node("VS_VERSION_INFO", &fixed, 52, false, &[string_info, var_info]);
    entry(res, 16, 1, &info, 0x0030);
}
