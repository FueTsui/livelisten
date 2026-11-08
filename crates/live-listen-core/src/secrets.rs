//! Secrets in Windows Credential Manager under `LiveListen/<name>`. Values are never logged
//! and never returned to the UI; the UI only learns whether a secret exists.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretError {
    Invalid,
    Store,
}

fn target(name: &str) -> Vec<u16> {
    format!("LiveListen/{name}").encode_utf16().chain(Some(0)).collect()
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128 && name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::Win32::Security::Credentials::{
        CRED_FLAGS, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW,
        CredWriteW,
    };
    use windows::core::{PCWSTR, PWSTR};

    pub fn set(name: &str, value: &str) -> Result<(), SecretError> {
        if !valid_name(name) || value.len() > 2048 {
            return Err(SecretError::Invalid);
        }
        let mut target_name = target(name);
        let mut user: Vec<u16> = "LiveListen".encode_utf16().chain(Some(0)).collect();
        let mut blob = value.as_bytes().to_vec();
        let credential = CREDENTIALW {
            Flags: CRED_FLAGS(0),
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target_name.as_mut_ptr()),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            UserName: PWSTR(user.as_mut_ptr()),
            ..Default::default()
        };
        // SAFETY: every pointer in `credential` refers to a live buffer owned by this frame.
        let result = unsafe { CredWriteW(&credential, 0) };
        blob.fill(0);
        result.map_err(|_| SecretError::Store)
    }

    pub fn get(name: &str) -> Option<String> {
        if !valid_name(name) {
            return None;
        }
        let target_name = target(name);
        let mut raw: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: `target_name` is NUL-terminated; `raw` receives a buffer freed below.
        unsafe { CredReadW(PCWSTR(target_name.as_ptr()), CRED_TYPE_GENERIC, None, &mut raw) }.ok()?;
        if raw.is_null() {
            return None;
        }
        // SAFETY: CredReadW succeeded, so `raw` points to a valid CREDENTIALW until CredFree.
        let value = unsafe {
            let c = &*raw;
            let bytes = if c.CredentialBlob.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec()
            };
            CredFree(raw as *const core::ffi::c_void);
            bytes
        };
        String::from_utf8(value).ok()
    }

    pub fn delete(name: &str) -> Result<(), SecretError> {
        if !valid_name(name) {
            return Err(SecretError::Invalid);
        }
        let target_name = target(name);
        // SAFETY: `target_name` is a NUL-terminated UTF-16 buffer.
        match unsafe { CredDeleteW(PCWSTR(target_name.as_ptr()), CRED_TYPE_GENERIC, None) } {
            Ok(()) => Ok(()),
            Err(_) if get(name).is_none() => Ok(()),
            Err(_) => Err(SecretError::Store),
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn set(_: &str, _: &str) -> Result<(), SecretError> {
        Err(SecretError::Store)
    }

    pub fn get(_: &str) -> Option<String> {
        None
    }

    pub fn delete(_: &str) -> Result<(), SecretError> {
        Err(SecretError::Store)
    }
}

pub use imp::{delete, get, set};

pub fn has(name: &str) -> bool {
    get(name).is_some()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn store_read_and_delete() {
        let name = format!("test.{}", std::process::id());
        set(&name, "value-for-test").unwrap();
        assert_eq!(get(&name).as_deref(), Some("value-for-test"));
        delete(&name).unwrap();
        assert!(get(&name).is_none());
        assert_eq!(set("bad name!", "x"), Err(SecretError::Invalid));
    }
}
