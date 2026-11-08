//! The payload appended to the setup program:
//!
//! ```text
//! [setup program][packed files ...][manifest JSON][footer, 72 bytes]
//! footer = "RSETUPv1" | payload offset u64 | payload length u64 | manifest length u64
//!          | SHA-256 of the payload (packed files + manifest) | "RSETUPok"
//! ```
//!
//! [`Package::open`] hashes every payload byte before it trusts the manifest, so a damaged
//! or truncated download is rejected before anything is written.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::codec;
use crate::hash::{self, Sha256};
use crate::manifest::{FileEntry, Packed};

const MAGIC: &[u8; 8] = b"RSETUPv1";
const END: &[u8; 8] = b"RSETUPok";
pub const FOOTER_LEN: u64 = 72;

#[derive(Debug)]
pub enum PackageError {
    /// The file is not a complete, unmodified package.
    Damaged(String),
    Io(io::Error),
}

impl std::fmt::Display for PackageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageError::Damaged(reason) => write!(f, "damaged package: {reason}"),
            PackageError::Io(e) => write!(f, "cannot read the package: {e}"),
        }
    }
}

impl From<io::Error> for PackageError {
    fn from(e: io::Error) -> Self {
        PackageError::Io(e)
    }
}

pub struct Package {
    file: File,
    /// Where the payload starts, which is also the length of the setup program itself.
    pub offset: u64,
    pub manifest: Packed,
}

fn damaged(reason: impl Into<String>) -> PackageError {
    PackageError::Damaged(reason.into())
}

impl Package {
    /// Opens and verifies a package; `progress(done, total)` follows the hashing.
    pub fn open(path: &Path, progress: &mut dyn FnMut(u64, u64)) -> Result<Package, PackageError> {
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        if len < FOOTER_LEN {
            return Err(damaged("too short"));
        }
        let mut footer = [0u8; FOOTER_LEN as usize];
        file.seek(SeekFrom::Start(len - FOOTER_LEN))?;
        file.read_exact(&mut footer)?;
        if &footer[..8] != MAGIC || &footer[64..] != END {
            return Err(damaged("no payload"));
        }
        let field = |at: usize| u64::from_le_bytes(footer[at..at + 8].try_into().unwrap());
        let (offset, payload_len, manifest_len) = (field(8), field(16), field(24));
        if offset.checked_add(payload_len).and_then(|n| n.checked_add(FOOTER_LEN)) != Some(len)
            || manifest_len > payload_len
            || manifest_len > 64 << 20
        {
            return Err(damaged("inconsistent sizes"));
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut hash = Sha256::new()?;
        let mut buffer = vec![0u8; 1 << 20];
        let mut done = 0u64;
        while done < payload_len {
            let want = buffer.len().min((payload_len - done) as usize);
            file.read_exact(&mut buffer[..want])?;
            hash.update(&buffer[..want])?;
            done += want as u64;
            progress(done, payload_len);
        }
        if hash.finish()? != footer[32..64] {
            return Err(damaged("checksum mismatch"));
        }
        let mut text = vec![0u8; manifest_len as usize];
        file.seek(SeekFrom::Start(offset + payload_len - manifest_len))?;
        file.read_exact(&mut text)?;
        let manifest: Packed =
            serde_json::from_slice(&text).map_err(|e| damaged(format!("unreadable manifest: {e}")))?;
        manifest.validate(payload_len - manifest_len).map_err(damaged)?;
        Ok(Package { file, offset, manifest })
    }

    /// Reads, decompresses and hash-checks one file.
    pub fn read(&mut self, entry: &FileEntry) -> Result<Vec<u8>, PackageError> {
        let mut packed = vec![0u8; usize::try_from(entry.packed).map_err(|_| damaged("file too large"))?];
        self.file.seek(SeekFrom::Start(self.offset + entry.offset))?;
        self.file.read_exact(&mut packed)?;
        let size = usize::try_from(entry.size).map_err(|_| damaged("file too large"))?;
        let data = codec::unpack(entry.method, &packed, size).map_err(|e| damaged(format!("{}: {e}", entry.path)))?;
        if hash::hex(&hash::digest(&data)?) != entry.sha256 {
            return Err(damaged(format!("{} does not match its hash", entry.path)));
        }
        Ok(data)
    }

    /// The setup program without its payload.
    pub fn stub(&mut self) -> io::Result<Vec<u8>> {
        let mut stub = vec![0u8; self.offset as usize];
        self.file.seek(SeekFrom::Start(0))?;
        self.file.read_exact(&mut stub)?;
        Ok(stub)
    }
}

/// Writes a package: the setup program, then packed files, then the manifest and footer.
pub struct Writer {
    out: File,
    offset: u64,
    written: u64,
    hash: Sha256,
}

impl Writer {
    pub fn create(path: &Path, stub: &[u8]) -> io::Result<Writer> {
        let mut out = File::create_new(path)?;
        out.write_all(stub)?;
        Ok(Writer { out, offset: stub.len() as u64, written: 0, hash: Sha256::new()? })
    }

    fn push(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.write_all(bytes)?;
        self.hash.update(bytes)?;
        self.written += bytes.len() as u64;
        Ok(())
    }

    /// Appends packed bytes and returns their payload offset.
    pub fn add(&mut self, packed: &[u8]) -> io::Result<u64> {
        let at = self.written;
        self.push(packed)?;
        Ok(at)
    }

    pub fn finish(mut self, manifest: &Packed) -> io::Result<()> {
        manifest.validate(self.written).map_err(io::Error::other)?;
        let text = serde_json::to_vec(manifest).map_err(io::Error::other)?;
        self.push(&text)?;
        let digest = self.hash.finish()?;
        let mut footer = Vec::with_capacity(FOOTER_LEN as usize);
        footer.extend(MAGIC);
        footer.extend(self.offset.to_le_bytes());
        footer.extend(self.written.to_le_bytes());
        footer.extend((text.len() as u64).to_le_bytes());
        footer.extend(digest);
        footer.extend(END);
        self.out.write_all(&footer)?;
        self.out.sync_all()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::manifest::{FORMAT, Role};

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("setup-payload-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(crate) fn build(path: &Path, files: &[(&str, Vec<u8>)]) {
        let mut writer = Writer::create(path, b"MZ fake setup program").unwrap();
        let mut entries = Vec::new();
        for (name, data) in files {
            let (method, packed) = codec::pack(data).unwrap();
            let offset = writer.add(&packed).unwrap();
            entries.push(FileEntry {
                path: (*name).into(),
                size: data.len() as u64,
                sha256: hash::hex(&hash::digest(data).unwrap()),
                offset,
                packed: packed.len() as u64,
                method,
            });
        }
        let product = crate::manifest::tests::product();
        writer
            .finish(&Packed { format: FORMAT, role: Role::Installer, version: "1.0.0".into(), product, files: entries })
            .unwrap();
    }

    #[test]
    fn verifies_round_trip_and_rejects_any_damage() {
        let dir = temp("roundtrip");
        let path = dir.join("setup.exe");
        let big: Vec<u8> = (0..300_000u32).flat_map(|i| (i / 7).to_le_bytes()).collect();
        build(&path, &[("Demo.exe", b"program".to_vec()), ("data/big.bin", big.clone())]);
        let mut package = Package::open(&path, &mut |_, _| {}).unwrap();
        assert_eq!(package.offset, 21);
        assert_eq!(package.stub().unwrap(), b"MZ fake setup program");
        let entries = package.manifest.files.clone();
        assert_eq!(package.read(&entries[1]).unwrap(), big);

        let original = std::fs::read(&path).unwrap();
        for at in [30usize, original.len() / 2, original.len() - 80, original.len() - 1] {
            let mut bad = original.clone();
            bad[at] ^= 0x40;
            std::fs::write(&path, &bad).unwrap();
            assert!(matches!(Package::open(&path, &mut |_, _| {}), Err(PackageError::Damaged(_))), "byte {at}");
        }
        std::fs::write(&path, &original[..original.len() - 10]).unwrap();
        assert!(matches!(Package::open(&path, &mut |_, _| {}), Err(PackageError::Damaged(_))), "truncated");
        let _ = std::fs::remove_dir_all(dir);
    }
}
