//! Builds the Windows resources of the setup program (icon, version information and the
//! application manifest) as a `.res` file and links it into the `setup` binary. Writing the
//! format directly avoids depending on the Windows SDK resource compiler.
//!
//! The manifest matters: without `asInvoker`, Windows guesses from the file name that a
//! "Setup" program needs administrator rights and asks for elevation.

// The host toolchain here is windows-gnu, whose linker warns about MSVC import libraries
// ("corrupt .drectve") while linking this script; the warning is harmless.
#![allow(linker_messages)]

use std::path::PathBuf;

#[path = "../scripts/windows_resources.rs"]
mod windows_resources;
use windows_resources::{FileVersion, add_version, entry, link_resource, manifest_fields, new_resource};

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_MANIFEST: u16 = 24;

fn main() {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../scripts/windows_resources.rs");
    let field = manifest_fields(&dir.join("install-manifest.json"));
    let (id, name, publisher) = (field(&["id"]), field(&["name", "zh"]), field(&["publisher"]));
    let icon = dir.join(field(&["icon"]));
    println!("cargo:rerun-if-changed={}", icon.display());
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();

    let mut res = new_resource();
    let ico = std::fs::read(&icon).expect("the icon in install-manifest.json is missing");
    add_icon(&mut res, &ico);
    add_version(
        &mut res,
        &name,
        &publisher,
        &version,
        FileVersion {
            description: &format!("{name} 安装程序"),
            filename: &format!("{id}-Setup-{version}-x64.exe"),
            internal_name: "setup",
            language: 0x0804,
            is_dll: false,
        },
    );
    entry(&mut res, RT_MANIFEST, 1, app_manifest(&id, &version).as_bytes(), 0x0030);
    link_resource(&res, "rustc-link-arg-bin=setup");
}

/// Splits an `.ico` file into RT_ICON images and the RT_GROUP_ICON (id 1) that lists them.
fn add_icon(res: &mut Vec<u8>, ico: &[u8]) {
    let u16_at = |at: usize| u16::from_le_bytes([ico[at], ico[at + 1]]);
    let u32_at = |at: usize| u32::from_le_bytes(ico[at..at + 4].try_into().unwrap());
    assert!(ico.len() > 6 && u16_at(2) == 1, "not an icon file");
    let count = u16_at(4);
    let mut group = Vec::new();
    group.extend([0, 0, 1, 0]);
    group.extend(count.to_le_bytes());
    for i in 0..count {
        let at = 6 + 16 * usize::from(i);
        let (size, offset) = (u32_at(at + 8) as usize, u32_at(at + 12) as usize);
        entry(res, RT_ICON, i + 1, &ico[offset..offset + size], 0x1010);
        group.extend(&ico[at..at + 12]); // width, height, colors, reserved, planes, bit count, size
        group.extend((i + 1).to_le_bytes());
    }
    entry(res, RT_GROUP_ICON, 1, &group, 0x1030);
}

fn app_manifest(id: &str, version: &str) -> String {
    let numeric: Vec<&str> = version.split(['-', '+']).next().unwrap_or("0.0.0").split('.').collect();
    let assembly_version = format!("{}.0", numeric.join("."));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="{id}.Setup" version="{assembly_version}" processorArchitecture="amd64"/>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security><requestedPrivileges><requestedExecutionLevel level="asInvoker" uiAccess="false"/></requestedPrivileges></security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application><supportedOS Id="{{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}}"/></application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>
"#
    )
}
