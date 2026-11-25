//! Client for the desktop instance's local, write-only card entry point.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::Value;
use windows::Win32::Foundation::{ERROR_PIPE_BUSY, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
use windows::Win32::Security::{
    EqualSid, GetTokenInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION;
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

pub const MAX_REQUEST_BYTES: usize = 32 * 1024;
const MAX_REPLY_BYTES: usize = 1024;
const CONNECT_WAIT: Duration = Duration::from_secs(3);
const CONNECT_RETRY: Duration = Duration::from_millis(40);
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x100_0000_01b3;

/// Both the CLI and the desktop core derive the same name from their config
/// directory and Windows session. The name is routing information, not authentication.
pub fn name(config: &Path) -> io::Result<String> {
    let mut session = 0;
    // SAFETY: session is valid output storage and the process is our own.
    unsafe { ProcessIdToSessionId(std::process::id(), &mut session) }.map_err(io::Error::other)?;
    let path = std::path::absolute(config)?;
    let normalized = path.to_string_lossy().replace('\\', "/").to_lowercase();
    let hash = normalized
        .trim_end_matches('/')
        .bytes()
        .fold(FNV_OFFSET, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME));
    Ok(format!("LiveListen.Cards.v1.{session}.{hash:016x}"))
}

fn connect(config: &Path) -> Result<File, &'static str> {
    let endpoint = format!(r"\\.\pipe\{}", name(config).map_err(|_| "card_pipe_unavailable")?);
    let deadline = Instant::now() + CONNECT_WAIT;
    loop {
        match OpenOptions::new().read(true).write(true).security_qos_flags(SECURITY_IDENTIFICATION.0).open(&endpoint) {
            Ok(pipe) => return Ok(pipe),
            Err(e)
                if (e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32))
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(CONNECT_RETRY);
            }
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return Err("card_pipe_access_denied"),
            Err(_) => return Err("card_pipe_unavailable"),
        }
    }
}

fn owned_by_current_user(pipe: &File) -> bool {
    let mut token = HANDLE::default();
    let mut user_info = [0usize; 32];
    let mut size = 0;
    let mut owner = PSID::default();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: all outputs are suitably aligned storage. The owner SID borrows
    // from descriptor, which is freed after the comparison; the token is owned.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let token = OwnedHandle::from_raw_handle(token.0);
        if GetTokenInformation(
            HANDLE(token.as_raw_handle()),
            TokenUser,
            Some(user_info.as_mut_ptr().cast()),
            std::mem::size_of_val(&user_info) as u32,
            &mut size,
        )
        .is_err()
        {
            return false;
        }
        let user = &*user_info.as_ptr().cast::<TOKEN_USER>();
        let result = GetSecurityInfo(
            HANDLE(pipe.as_raw_handle()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            None,
            None,
            Some(&mut descriptor),
        )
        .is_ok()
            && !owner.0.is_null()
            && EqualSid(owner, user.User.Sid).is_ok();
        if !descriptor.0.is_null() {
            LocalFree(Some(HLOCAL(descriptor.0)));
        }
        result
    }
}

/// Writes at most one request. Once writing begins, any transport failure has an
/// unknown outcome and the caller must not resend it automatically.
pub fn add(config: &Path, request: &[u8]) -> Result<Value, &'static str> {
    if request.len() > MAX_REQUEST_BYTES {
        return Err("request_too_large");
    }
    let mut pipe = connect(config)?;
    if !owned_by_current_user(&pipe) {
        return Err("card_pipe_access_denied");
    }
    let unknown = |_| "card_outcome_unknown";
    pipe.write_all(&(request.len() as u32).to_le_bytes()).map_err(unknown)?;
    pipe.write_all(request).map_err(unknown)?;
    let mut size = [0; 4];
    pipe.read_exact(&mut size).map_err(unknown)?;
    let size = u32::from_le_bytes(size) as usize;
    if size == 0 || size > MAX_REPLY_BYTES {
        return Err("card_outcome_unknown");
    }
    let mut reply = vec![0; size];
    pipe.read_exact(&mut reply).map_err(unknown)?;
    serde_json::from_slice(&reply).map_err(|_| "card_outcome_unknown")
}
