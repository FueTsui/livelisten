//! SHA-256 through Windows CNG, fast in debug builds too.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use windows::Win32::Security::Cryptography::{
    BCRYPT_HASH_HANDLE, BCRYPT_SHA256_ALG_HANDLE, BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash, BCryptHashData,
};

pub struct Sha256(BCRYPT_HASH_HANDLE);

fn cng(status: windows::Win32::Foundation::NTSTATUS) -> io::Result<()> {
    status.ok().map_err(|e| io::Error::other(format!("SHA-256 failed: {e}")))
}

impl Sha256 {
    pub fn new() -> io::Result<Sha256> {
        let mut handle = BCRYPT_HASH_HANDLE::default();
        // SAFETY: the SHA-256 pseudo-handle needs no opened provider; `handle` receives a new
        // hash object whose memory CNG manages.
        cng(unsafe { BCryptCreateHash(BCRYPT_SHA256_ALG_HANDLE, &mut handle, None, None, 0) })?;
        Ok(Sha256(handle))
    }

    pub fn update(&mut self, data: &[u8]) -> io::Result<()> {
        for chunk in data.chunks(1 << 30) {
            // SAFETY: the handle is a live hash object and `chunk` is readable for its length.
            cng(unsafe { BCryptHashData(self.0, chunk, 0) })?;
        }
        Ok(())
    }

    pub fn finish(self) -> io::Result<[u8; 32]> {
        let mut out = [0u8; 32];
        // SAFETY: the handle is live and `out` has the 32 bytes a SHA-256 digest needs.
        cng(unsafe { BCryptFinishHash(self.0, &mut out, 0) })?;
        Ok(out)
    }
}

impl Drop for Sha256 {
    fn drop(&mut self) {
        // SAFETY: the handle came from BCryptCreateHash and is destroyed exactly once.
        let _ = unsafe { BCryptDestroyHash(self.0) };
    }
}

pub fn digest(data: &[u8]) -> io::Result<[u8; 32]> {
    let mut hash = Sha256::new()?;
    hash.update(data)?;
    hash.finish()
}

pub fn file_digest(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new()?;
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return hash.finish();
        }
        hash.update(&buffer[..read])?;
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_known_digests() {
        assert_eq!(hex(&digest(b"").unwrap()), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(&digest(b"abc").unwrap()), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
