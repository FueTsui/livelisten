//! File compression through the Windows Compression API (cabinet.dll, Windows 8 and later).

use std::io;

use serde::{Deserialize, Serialize};
use windows::Win32::Storage::Compression::{
    COMPRESS_ALGORITHM, COMPRESS_ALGORITHM_LZMS, COMPRESS_ALGORITHM_XPRESS_HUFF, COMPRESSOR_HANDLE, CloseCompressor,
    CloseDecompressor, Compress, CreateCompressor, CreateDecompressor, DECOMPRESSOR_HANDLE, Decompress,
};

/// How one file is stored in the payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    Store,
    /// Xpress with Huffman coding: for small files.
    Xpress,
    /// LZMS: best ratio for large files.
    Lzms,
}

impl Method {
    fn algorithm(self) -> Option<COMPRESS_ALGORITHM> {
        match self {
            Method::Store => None,
            Method::Xpress => Some(COMPRESS_ALGORITHM_XPRESS_HUFF),
            Method::Lzms => Some(COMPRESS_ALGORITHM_LZMS),
        }
    }
}

/// Compresses `data`, falling back to storing it when compression does not help.
pub fn pack(data: &[u8]) -> io::Result<(Method, Vec<u8>)> {
    let method = if data.len() >= 256 * 1024 { Method::Lzms } else { Method::Xpress };
    if data.is_empty() {
        return Ok((Method::Store, Vec::new()));
    }
    let packed = compress(method, data)?;
    if packed.len() >= data.len() { Ok((Method::Store, data.to_vec())) } else { Ok((method, packed)) }
}

fn compress(method: Method, data: &[u8]) -> io::Result<Vec<u8>> {
    let algorithm = method.algorithm().expect("compress is only called for a compressing method");
    let mut handle = COMPRESSOR_HANDLE::default();
    // SAFETY: `handle` receives a new compressor; the default allocator is used.
    unsafe { CreateCompressor(algorithm, None, &mut handle) }?;
    let mut needed = 0usize;
    // SAFETY: a null output buffer asks only for the required size; `data` is readable.
    let _ = unsafe { Compress(handle, Some(data.as_ptr().cast()), data.len(), None, 0, &mut needed) };
    let mut out = vec![0u8; needed.max(64)];
    let mut written = 0usize;
    // SAFETY: `out` is writable for its full length and `data` readable for its length.
    let result = unsafe {
        Compress(handle, Some(data.as_ptr().cast()), data.len(), Some(out.as_mut_ptr().cast()), out.len(), &mut written)
    };
    // SAFETY: the handle came from CreateCompressor and is closed exactly once.
    let _ = unsafe { CloseCompressor(handle) };
    result?;
    out.truncate(written);
    Ok(out)
}

/// Restores a file of `size` bytes; any other length is an error.
pub fn unpack(method: Method, data: &[u8], size: usize) -> io::Result<Vec<u8>> {
    let Some(algorithm) = method.algorithm() else {
        if data.len() != size {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "stored file has the wrong size"));
        }
        return Ok(data.to_vec());
    };
    let mut handle = DECOMPRESSOR_HANDLE::default();
    // SAFETY: `handle` receives a new decompressor; the default allocator is used.
    unsafe { CreateDecompressor(algorithm, None, &mut handle) }?;
    let mut out = vec![0u8; size];
    let mut written = 0usize;
    // SAFETY: `out` is writable for `size` bytes and `data` readable for its length.
    let result = unsafe {
        Decompress(
            handle,
            Some(data.as_ptr().cast()),
            data.len(),
            Some(out.as_mut_ptr().cast()),
            size,
            Some(&mut written),
        )
    };
    // SAFETY: the handle came from CreateDecompressor and is closed exactly once.
    let _ = unsafe { CloseDecompressor(handle) };
    result.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("damaged compressed file: {e}")))?;
    if written != size {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "compressed file has the wrong size"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_method() {
        let text: Vec<u8> = (0..400_000u32).flat_map(|i| (i % 251).to_le_bytes()).collect();
        for data in [&b""[..], b"x", &text[..1000], &text[..]] {
            let (method, packed) = pack(data).unwrap();
            assert_eq!(unpack(method, &packed, data.len()).unwrap(), data);
        }
        assert_eq!(pack(&text).unwrap().0, Method::Lzms);
        let (method, mut packed) = pack(&text[..1000]).unwrap();
        assert_eq!(method, Method::Xpress);
        packed.truncate(packed.len() / 2);
        assert!(unpack(method, &packed, 1000).is_err(), "a cut stream is rejected");
        assert!(unpack(Method::Store, b"abc", 4).is_err());
    }
}
