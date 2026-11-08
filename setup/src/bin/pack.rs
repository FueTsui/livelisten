//! Packs an app folder into the setup program (build step, REL-01):
//!
//! ```text
//! pack --stub <setup.exe> --source <app folder> --manifest <install-manifest.json>
//!      --version <x.y.z> --out <App-Setup-x.y.z-x64.exe>
//! ```
//!
//! Every file is compressed, its SHA-256 recorded, and the finished package is opened again
//! and every file restored and compared before the command reports success.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;

use live_listen_setup::codec::{self, Method};
use live_listen_setup::hash;
use live_listen_setup::manifest::{self, FORMAT, FileEntry, Packed, Product, Role};
use live_listen_setup::payload::{Package, Writer};
use windows::Win32::Foundation::FreeLibrary;
use windows::Win32::System::LibraryLoader::{
    FindResourceW, LOAD_LIBRARY_AS_DATAFILE, LOAD_LIBRARY_AS_IMAGE_RESOURCE, LoadLibraryExW, LoadResource,
    LockResource, SizeofResource,
};
use windows::core::{HSTRING, PCWSTR};

/// Size, SHA-256, method and packed bytes of one file, or why it could not be read.
type PackedFile = Result<(u64, String, Method, Vec<u8>), String>;

fn resource(module: windows::Win32::Foundation::HMODULE, kind: u16, id: u16) -> Option<Vec<u8>> {
    // SAFETY: reads one resource of a module loaded as data; the bytes are copied out.
    unsafe {
        let found = FindResourceW(Some(module), PCWSTR(id as usize as *const u16), PCWSTR(kind as usize as *const u16));
        if found.is_invalid() {
            return None;
        }
        let data = LoadResource(Some(module), found).ok()?;
        let size = SizeofResource(Some(module), found) as usize;
        let pointer = LockResource(data) as *const u8;
        (!pointer.is_null()).then(|| std::slice::from_raw_parts(pointer, size).to_vec())
    }
}

/// The setup program must run without elevation, scale per monitor and carry its icon.
fn check_stub(path: &Path) -> Result<(), String> {
    // SAFETY: loads the file as a data image only (no code runs) and frees it below.
    let module = unsafe {
        LoadLibraryExW(
            &HSTRING::from(path.as_os_str()),
            None,
            LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE,
        )
    }
    .map_err(|e| format!("cannot read the setup program: {e}"))?;
    let manifest = resource(module, 24, 1).map(|b| String::from_utf8_lossy(&b).into_owned());
    let icon = resource(module, 14, 1);
    let version = resource(module, 16, 1);
    // SAFETY: frees the module loaded above.
    let _ = unsafe { FreeLibrary(module) };
    let manifest = manifest.ok_or("the setup program has no application manifest")?;
    if !manifest.contains(r#"level="asInvoker""#) || !manifest.contains("PerMonitorV2") {
        return Err("the setup manifest must request asInvoker and PerMonitorV2".into());
    }
    if icon.is_none() || version.is_none() {
        return Err("the setup program lacks its icon or version information".into());
    }
    Ok(())
}

fn collect(root: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            let path = entry.path();
            if kind.is_symlink() {
                return Err(format!("links are not packed: {}", path.display()));
            }
            if kind.is_dir() {
                stack.push(path);
                continue;
            }
            let relative = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            if !manifest::safe_relative(&relative) {
                return Err(format!("unsafe file name: {relative}"));
            }
            files.push((relative, path));
        }
    }
    files.sort();
    Ok(files)
}

fn run(options: &HashMap<String, String>) -> Result<serde_json::Value, String> {
    let get = |key: &str| options.get(key).ok_or_else(|| format!("--{key} is required"));
    let (stub_path, source, out) =
        (PathBuf::from(get("stub")?), PathBuf::from(get("source")?), PathBuf::from(get("out")?));
    let version = get("version")?.clone();
    let mut product: Product = serde_json::from_slice(&std::fs::read(get("manifest")?).map_err(|e| e.to_string())?)
        .map_err(|e| format!("install manifest: {e}"))?;
    product.validate()?;
    product.icon = None;
    check_stub(&stub_path)?;
    let stub = std::fs::read(&stub_path).map_err(|e| e.to_string())?;
    let files = collect(&source)?;
    if files.is_empty() {
        return Err("the source folder is empty".into());
    }

    // Compress in parallel, write in order.
    let packed: Vec<Mutex<Option<PackedFile>>> = files.iter().map(|_| Mutex::new(None)).collect();
    let next = Mutex::new(0usize);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(16);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let index = {
                        let mut next = next.lock().unwrap();
                        let index = *next;
                        *next += 1;
                        index
                    };
                    let Some((_, path)) = files.get(index) else { break };
                    let result = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display())).and_then(|data| {
                        let digest = hash::hex(&hash::digest(&data).map_err(|e| e.to_string())?);
                        let (method, bytes) = codec::pack(&data).map_err(|e| format!("{}: {e}", path.display()))?;
                        Ok((data.len() as u64, digest, method, bytes))
                    });
                    *packed[index].lock().unwrap() = Some(result);
                }
            });
        }
    });

    let mut writer = Writer::create(&out, &stub).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut entries = Vec::new();
    for ((relative, _), slot) in files.iter().zip(&packed) {
        let (size, sha256, method, bytes) = slot.lock().unwrap().take().expect("every file was packed")?;
        let offset = writer.add(&bytes).map_err(|e| e.to_string())?;
        entries.push(FileEntry { path: relative.clone(), size, sha256, offset, packed: bytes.len() as u64, method });
    }
    let manifest = Packed { format: FORMAT, role: Role::Installer, version, product, files: entries };
    writer.finish(&manifest).map_err(|e| e.to_string())?;

    let mut package = Package::open(&out, &mut |_, _| {}).map_err(|e| format!("verification: {e}"))?;
    for entry in &manifest.files {
        package.read(entry).map_err(|e| format!("verification: {e}"))?;
    }
    let setup_size = std::fs::metadata(&out).map_err(|e| e.to_string())?.len();
    Ok(serde_json::json!({
        "output": out,
        "version": manifest.version,
        "files": manifest.files.len(),
        "installedBytes": manifest.total_size(),
        "setupBytes": setup_size,
        "sha256": hash::hex(&hash::file_digest(&out).map_err(|e| e.to_string())?),
        "verified": true,
    }))
}

fn main() -> ExitCode {
    let mut options = HashMap::new();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let Some(key) = flag.strip_prefix("--") else {
            eprintln!("unexpected argument {flag:?}");
            return ExitCode::from(2);
        };
        let Some(value) = args.next() else {
            eprintln!("--{key} needs a value");
            return ExitCode::from(2);
        };
        options.insert(key.to_string(), value);
    }
    match run(&options) {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(problem) => {
            eprintln!("pack failed: {problem}");
            ExitCode::from(1)
        }
    }
}
