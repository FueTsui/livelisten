//! Embeds the product attribution in the shipped Rust CLI and native DLL.

#![allow(linker_messages)]

use std::{env, path::PathBuf};

mod windows_resources;
use windows_resources::{FileVersion, add_version, link_resource, manifest_fields, new_resource};

fn main() {
    println!("cargo:rerun-if-changed=../../scripts/build_native_metadata.rs");
    println!("cargo:rerun-if-changed=../../scripts/windows_resources.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc")
    {
        return;
    }
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../../setup/install-manifest.json");
    let field = manifest_fields(&manifest);
    let (description, filename, internal_name, link_arg, is_dll) = match env::var("CARGO_PKG_NAME").unwrap().as_str() {
        "live-listen-cli" => {
            ("LiveListen CLI", "live-listen.exe", "live-listen", "rustc-link-arg-bin=live-listen", false)
        }
        "live-listen-ffi" => {
            ("LiveListen native core", "livelisten_native.dll", "livelisten_native", "rustc-link-arg-cdylib", true)
        }
        name => panic!("No Windows version metadata for {name}"),
    };
    let mut res = new_resource();
    add_version(
        &mut res,
        &field(&["name", "en"]),
        &field(&["publisher"]),
        &env::var("CARGO_PKG_VERSION").unwrap(),
        FileVersion { description, filename, internal_name, language: 0x0409, is_dll },
    );
    link_resource(&res, link_arg);
}
