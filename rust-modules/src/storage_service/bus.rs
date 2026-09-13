//! LS2 ownership and bounded outgoing calls. ABI uses the existing real NDK LS2 header,
//! also exercised by the packaged 0.0.4/0.0.5 helper probe on webOS 4.10.2.
use super::wire::{ErrorCode, MAX_FRAME};
use libc::{c_char, c_int, c_ulong, c_void};
use serde_json::Value;
use std::{
    ffi::{CStr, CString},
    ptr,
    time::{Duration, Instant},
};

#[repr(C)]
struct LSError {
    error_code: c_int,
    message: *mut c_char,
    file: *const c_char,
    line: c_int,
    func: *const c_char,
    padding: *mut c_void,
    magic: c_ulong,
}
type Callback = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> bool;
#[link(name = "luna-service2")]
unsafe extern "C" {
    fn LSErrorInit(error: *mut LSError) -> bool;
    fn LSErrorFree(error: *mut LSError);
    fn LSRegister(name: *const c_char, handle: *mut *mut c_void, error: *mut LSError) -> bool;
    fn LSUnregister(handle: *mut c_void, error: *mut LSError) -> bool;
    fn LSGmainAttach(handle: *mut c_void, main_loop: *mut c_void, error: *mut LSError) -> bool;
    fn LSCallOneReply(
        handle: *mut c_void,
        uri: *const c_char,
        payload: *const c_char,
        callback: Callback,
        context: *mut c_void,
        token: *mut c_ulong,
        error: *mut LSError,
    ) -> bool;
    fn LSCallCancel(handle: *mut c_void, token: c_ulong, error: *mut LSError) -> bool;
    fn LSMessageGetPayload(message: *mut c_void) -> *const c_char;
}
#[link(name = "glib-2.0")]
unsafe extern "C" {
    fn g_main_loop_new(context: *mut c_void, running: c_int) -> *mut c_void;
    fn g_main_loop_unref(main_loop: *mut c_void);
    fn g_main_context_iteration(context: *mut c_void, may_block: c_int) -> c_int;
}
struct Error(LSError);
impl Error {
    fn new() -> Self {
        unsafe {
            let mut e = std::mem::zeroed();
            LSErrorInit(&mut e);
            Self(e)
        }
    }
}
impl Drop for Error {
    fn drop(&mut self) {
        unsafe {
            LSErrorFree(&mut self.0);
        }
    }
}

pub struct Bus {
    handle: *mut c_void,
    main_loop: *mut c_void,
}
impl Bus {
    pub fn register(name: &str) -> Result<Self, ErrorCode> {
        let name = CString::new(name).map_err(|_| ErrorCode::Invalid)?;
        let mut error = Error::new();
        unsafe {
            let main_loop = g_main_loop_new(ptr::null_mut(), 0);
            if main_loop.is_null() {
                return Err(ErrorCode::Unavailable);
            }
            let mut handle = ptr::null_mut();
            if !LSRegister(name.as_ptr(), &mut handle, &mut error.0) {
                g_main_loop_unref(main_loop);
                return Err(ErrorCode::Unavailable);
            }
            let bus = Self { handle, main_loop };
            if !LSGmainAttach(handle, main_loop, &mut error.0) {
                return Err(ErrorCode::Unavailable);
            }
            Ok(bus)
        }
    }
    pub fn pump(&self) {
        unsafe {
            g_main_context_iteration(ptr::null_mut(), 0);
        }
    }
    pub fn call(&mut self, uri: &str, payload: &Value) -> Result<Value, ErrorCode> {
        let uri = CString::new(uri).map_err(|_| ErrorCode::Invalid)?;
        let payload = serde_json::to_vec(payload).map_err(|_| ErrorCode::Invalid)?;
        if payload.len() > MAX_FRAME {
            return Err(ErrorCode::Invalid);
        }
        let payload = CString::new(payload).map_err(|_| ErrorCode::Invalid)?;
        let mut reply: Option<Result<Value, ErrorCode>> = None;
        let mut token = 0;
        let mut error = Error::new();
        if !unsafe {
            LSCallOneReply(
                self.handle,
                uri.as_ptr(),
                payload.as_ptr(),
                receive,
                &mut reply as *mut _ as *mut c_void,
                &mut token,
                &mut error.0,
            )
        } {
            return Err(ErrorCode::Unavailable);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while reply.is_none() && Instant::now() < deadline {
            self.pump();
            std::thread::sleep(Duration::from_millis(2));
        }
        if reply.is_none() {
            // LS2 removes the callback before stack context is released. If cancellation fails,
            // terminate the helper rather than expose a dangling callback context.
            if !unsafe { LSCallCancel(self.handle, token, &mut error.0) } {
                std::process::exit(1);
            }
        }
        reply.unwrap_or(Err(ErrorCode::Timeout))
    }
}
unsafe extern "C" fn receive(_: *mut c_void, message: *mut c_void, context: *mut c_void) -> bool {
    let target = &mut *(context as *mut Option<Result<Value, ErrorCode>>);
    let text = LSMessageGetPayload(message);
    *target = Some(if text.is_null() {
        Err(ErrorCode::Unavailable)
    } else {
        let len = libc::strnlen(text, MAX_FRAME + 1);
        if len > MAX_FRAME {
            Err(ErrorCode::Invalid)
        } else {
            serde_json::from_slice(CStr::from_ptr(text).to_bytes()).map_err(|_| ErrorCode::Invalid)
        }
    });
    true
}
impl Drop for Bus {
    fn drop(&mut self) {
        let mut error = Error::new();
        unsafe {
            LSUnregister(self.handle, &mut error.0);
            g_main_loop_unref(self.main_loop);
        }
    }
}
