//! C ABI for the WinUI shell.
//!
//! ```c
//! typedef void (*ll_event)(const char *json, void *user);
//! LLHandle *ll_start(const char *config_dir /* UTF-8 or NULL */, ll_event callback, void *user);
//! char *ll_call(LLHandle *handle, const char *request_json);   // free with ll_free
//! void ll_free(char *text);
//! void ll_destroy(LLHandle *handle);
//! const char *ll_version(void);
//! ```
//!
//! Events arrive on an engine thread; the callback must return quickly and copy the string.

use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;

use live_listen_core::engine::{Engine, EventSink, Options, VERSION};

pub type EventCallback = extern "C" fn(json: *const c_char, user: *mut c_void);

/// Opaque engine handle owned by the caller between `ll_start` and `ll_destroy`.
pub struct Handle {
    engine: Engine,
}

/// The caller's context pointer; the caller keeps it valid until `ll_destroy` returns.
#[derive(Clone, Copy)]
struct User(*mut c_void);

// SAFETY: the pointer is only handed back to the caller's callback, never dereferenced here.
unsafe impl Send for User {}
// SAFETY: as above; the caller's callback is responsible for its own synchronization.
unsafe impl Sync for User {}

impl User {
    fn get(self) -> *mut c_void {
        self.0
    }
}

static VERSION_C: &[u8] = concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes();

fn to_c(text: String) -> *mut c_char {
    // JSON from serde_json never contains NUL; fall back to an error object just in case.
    CString::new(text)
        .unwrap_or_else(|_| CString::new("{\"ok\":false,\"error\":\"encoding\"}").unwrap_or_default())
        .into_raw()
}

/// Starts the engine. Returns NULL on failure.
///
/// # Safety
/// `config_dir` is NULL or a NUL-terminated UTF-8 string; `callback` stays callable and
/// `user` stays valid until `ll_destroy` returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_start(
    config_dir: *const c_char,
    callback: Option<EventCallback>,
    user: *mut c_void,
) -> *mut Handle {
    let dir = if config_dir.is_null() {
        None
    } else {
        // SAFETY: the caller passes a NUL-terminated string.
        let text = unsafe { CStr::from_ptr(config_dir) }.to_string_lossy().into_owned();
        if text.is_empty() { None } else { Some(PathBuf::from(text)) }
    };
    let user = User(user);
    let result = catch_unwind(AssertUnwindSafe(move || {
        let sink: EventSink = Arc::new(move |json: &str| {
            if let (Some(cb), Ok(text)) = (callback, CString::new(json)) {
                cb(text.as_ptr(), user.get());
            }
        });
        // ABI tests use isolated data and must not install global input hooks or inherit
        // the interactive application's audio/provider search paths.
        #[cfg(test)]
        let options = Options { config_dir: dir, ..Options::default() };
        #[cfg(not(test))]
        let options = Options::for_app(dir);
        Engine::start(options, sink)
    }));
    match result {
        Ok(Ok(engine)) => Box::into_raw(Box::new(Handle { engine })),
        _ => std::ptr::null_mut(),
    }
}

/// Runs one JSON command and returns the JSON reply (free it with `ll_free`).
///
/// # Safety
/// `handle` comes from `ll_start` and is not destroyed; `request` is NUL-terminated UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_call(handle: *mut Handle, request: *const c_char) -> *mut c_char {
    if handle.is_null() || request.is_null() {
        return to_c("{\"ok\":false,\"error\":\"bad_request\"}".into());
    }
    // SAFETY: checked for NULL; the caller guarantees the handle is live.
    let handle = unsafe { &*handle };
    // SAFETY: checked for NULL; the caller passes a NUL-terminated string.
    let request = unsafe { CStr::from_ptr(request) }.to_string_lossy().into_owned();
    let reply = catch_unwind(AssertUnwindSafe(|| handle.engine.call(&request)))
        .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"internal\"}".into());
    to_c(reply)
}

/// Frees a string returned by `ll_call`.
///
/// # Safety
/// `text` is NULL or a pointer returned by `ll_call` that has not been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_free(text: *mut c_char) {
    if !text.is_null() {
        // SAFETY: the pointer came from CString::into_raw in `to_c`.
        drop(unsafe { CString::from_raw(text) });
    }
}

/// Stops the engine and frees the handle. No events are delivered after it returns.
///
/// # Safety
/// `handle` is NULL or comes from `ll_start` and is destroyed only once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_destroy(handle: *mut Handle) {
    if !handle.is_null() {
        // SAFETY: the pointer came from Box::into_raw in `ll_start`.
        let handle = unsafe { Box::from_raw(handle) };
        let _ = catch_unwind(AssertUnwindSafe(move || drop(handle)));
    }
}

/// Version string, valid for the life of the process.
#[unsafe(no_mangle)]
pub extern "C" fn ll_version() -> *const c_char {
    debug_assert_eq!(VERSION.len() + 1, VERSION_C.len());
    VERSION_C.as_ptr().cast()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static EVENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

    extern "C" fn on_event(json: *const c_char, _user: *mut c_void) {
        // SAFETY: the engine passes a valid NUL-terminated string for the duration of the call.
        let text = unsafe { CStr::from_ptr(json) }.to_string_lossy().into_owned();
        EVENTS.lock().unwrap().push(text);
    }

    #[test]
    fn start_call_and_destroy() {
        let dir = std::env::temp_dir().join(format!("ll-ffi-{}", std::process::id()));
        let dir_c = CString::new(dir.to_string_lossy().as_bytes()).unwrap();
        // SAFETY: valid arguments; the handle is destroyed below.
        unsafe {
            let handle = ll_start(dir_c.as_ptr(), Some(on_event), std::ptr::null_mut());
            assert!(!handle.is_null());
            let request = CString::new("{\"cmd\":\"status\"}").unwrap();
            let reply = ll_call(handle, request.as_ptr());
            let text = CStr::from_ptr(reply).to_string_lossy().into_owned();
            ll_free(reply);
            assert!(text.contains("\"ok\":true"), "{text}");
            assert!(text.contains("\"version\""));
            ll_destroy(handle);
            let version = CStr::from_ptr(ll_version()).to_string_lossy().into_owned();
            assert_eq!(version, VERSION);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
