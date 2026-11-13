use super::{
    apply_user_agent, available, curl_easy_init, curl_easy_perform, curl_easy_setopt_long, curl_easy_setopt_ptr,
    curl_version_num, origin, refuse_name, require_option, require_trust_store, threaded_tls_ready,
    tls_failure_reason, Easy, Tls, CURL, CURLOPT_CAINFO, CURLOPT_CONNECTTIMEOUT, CURLOPT_NOPROGRESS, CURLOPT_NOSIGNAL,
    CURLOPT_PROGRESSFUNCTION, CURLOPT_PROTOCOLS, CURLOPT_SSL_VERIFYHOST, CURLOPT_SSL_VERIFYPEER, CURLOPT_URL,
    CURLOPT_XFERINFODATA, CURLOPT_XFERINFOFUNCTION, CURLPROTO_HTTP, CURLPROTO_HTTPS, CURL_XFERINFO_MIN_VERSION,
};
use std::ffi::CString;
use std::os::raw::{c_int, c_long, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

plx_base::dynlib! {
    raw: ["libcurl.so.4", "libcurl.so.5", "libcurl.4.dylib"] {
    fn curl_easy_send(handle: *mut CURL, buffer: *const c_void, length: usize, sent: *mut usize) -> c_int;
    fn curl_easy_recv(handle: *mut CURL, buffer: *mut c_void, length: usize, received: *mut usize) -> c_int;
    fn curl_easy_getinfo_socket = "curl_easy_getinfo"(handle: *mut CURL, info: c_int, ..., out: *mut c_int) -> c_int;
}}

const CURLOPT_CONNECT_ONLY: c_int = 141;
const CURLOPT_HTTP_VERSION: c_int = 84;
const CURL_HTTP_VERSION_1_1: c_long = 2;
const CURLINFO_ACTIVESOCKET: c_int = 0x50_0000 + 44;
const CURLE_OK: c_int = 0;
const CURLE_COULDNT_RESOLVE_HOST: c_int = 6;
const CURLE_ABORTED_BY_CALLBACK: c_int = 42;
const CURLE_AGAIN: c_int = 81;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenError {
    Unavailable,
    SerialTls,
    Address,
    Refused(&'static str),
    Connect { rc: c_int, why: Option<String> },
    NoSocket,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Io {
    Moved(usize),
    Again,
    Closed,
    Failed(c_int),
}

pub struct Socket {
    easy: Easy,
    fd: c_int,
}

pub fn open(
    _off: &plx_base::task::OffFrame,
    authority: &str,
    secure: Option<Tls<'_>>,
    connect_timeout_s: c_long,
    cancel: &AtomicBool,
) -> Result<Socket, OpenError> {
    if !available() || !bound() {
        return Err(OpenError::Unavailable);
    }
    if !threaded_tls_ready() {
        return Err(OpenError::SerialTls);
    }
    let tls = secure.is_some();
    let address = format!("{}://{authority}/", if tls { "https" } else { "http" });
    if refuse_name(origin::url_host(&address), connect_timeout_s) {
        return Err(OpenError::Connect { rc: CURLE_COULDNT_RESOLVE_HOST, why: None });
    }
    let url = CString::new(address).map_err(|_| OpenError::Address)?;
    let bundle = match &secure {
        Some(Tls::CaBundle(path)) => Some(CString::new(*path).map_err(|_| OpenError::Address)?),
        Some(Tls::Pinned(_)) => return Err(OpenError::Refused("CURLOPT_PINNEDPUBLICKEY")),
        Some(Tls::Ca) | None => None,
    };
    unsafe {
        let handle = curl_easy_init();
        if handle.is_null() {
            return Err(OpenError::Unavailable);
        }
        let easy = Easy(handle);
        let protocols = if tls { CURLPROTO_HTTPS } else { CURLPROTO_HTTP };
        let require = |rc: c_int, option: &'static str| require_option(rc, option).map_err(|_| OpenError::Refused(option));
        require(curl_easy_setopt_long(easy.0, CURLOPT_PROTOCOLS, protocols), "CURLOPT_PROTOCOLS")?;
        if tls {
            require(curl_easy_setopt_long(easy.0, CURLOPT_SSL_VERIFYPEER, 1), "CURLOPT_SSL_VERIFYPEER")?;
            require(curl_easy_setopt_long(easy.0, CURLOPT_SSL_VERIFYHOST, 2), "CURLOPT_SSL_VERIFYHOST")?;
        }
        if let Some(bundle) = &bundle {
            require_trust_store(curl_easy_setopt_ptr(easy.0, CURLOPT_CAINFO, bundle.as_ptr() as *const c_void))
                .map_err(|_| OpenError::Refused("CURLOPT_CAINFO"))?;
        }
        require(curl_easy_setopt_long(easy.0, CURLOPT_HTTP_VERSION, CURL_HTTP_VERSION_1_1), "CURLOPT_HTTP_VERSION")?;
        require(curl_easy_setopt_long(easy.0, CURLOPT_CONNECT_ONLY, 1), "CURLOPT_CONNECT_ONLY")?;
        curl_easy_setopt_ptr(easy.0, CURLOPT_URL, url.as_ptr() as *const c_void);
        curl_easy_setopt_long(easy.0, CURLOPT_NOSIGNAL, 1);
        curl_easy_setopt_long(easy.0, CURLOPT_CONNECTTIMEOUT, connect_timeout_s);
        apply_user_agent(easy.0);
        let watching = watch_cancel(easy.0, cancel);
        let rc = curl_easy_perform(easy.0);
        if watching {
            curl_easy_setopt_long(easy.0, CURLOPT_NOPROGRESS, 1);
        }
        if rc == CURLE_ABORTED_BY_CALLBACK && cancel.load(Ordering::Acquire) {
            return Err(OpenError::Cancelled);
        }
        if rc != CURLE_OK {
            return Err(OpenError::Connect { rc, why: tls_failure_reason(easy.0, rc) });
        }
        let mut fd: c_int = -1;
        if curl_easy_getinfo_socket(easy.0, CURLINFO_ACTIVESOCKET, &mut fd) != CURLE_OK || fd < 0 {
            return Err(OpenError::NoSocket);
        }
        Ok(Socket { easy, fd })
    }
}

impl Socket {
    pub fn fd(&self) -> c_int {
        self.fd
    }

    pub fn send(&mut self, bytes: &[u8]) -> Io {
        let mut sent = 0usize;
        let rc = unsafe { curl_easy_send(self.easy.0, bytes.as_ptr() as *const c_void, bytes.len(), &mut sent) };
        match rc {
            CURLE_OK if sent > 0 => Io::Moved(sent),
            CURLE_OK | CURLE_AGAIN => Io::Again,
            rc => Io::Failed(rc),
        }
    }

    pub fn recv(&mut self, into: &mut [u8]) -> Io {
        let mut received = 0usize;
        let rc = unsafe { curl_easy_recv(self.easy.0, into.as_mut_ptr() as *mut c_void, into.len(), &mut received) };
        match rc {
            CURLE_OK if received == 0 => Io::Closed,
            CURLE_OK => Io::Moved(received),
            CURLE_AGAIN => Io::Again,
            rc => Io::Failed(rc),
        }
    }
}

fn bound() -> bool {
    static BOUND: OnceLock<bool> = OnceLock::new();
    *BOUND.get_or_init(|| {
        let ok = raw::load(None).ok();
        if !ok {
            plx_base::eventlog::log("net: this libcurl lacks curl_easy_send/curl_easy_recv; no WebSocket");
        }
        ok
    })
}

unsafe fn watch_cancel(easy: *mut CURL, cancel: &AtomicBool) -> bool {
    let data = (cancel as *const AtomicBool) as *const c_void;
    let version = curl_version_num();
    if curl_easy_setopt_long(easy, CURLOPT_NOPROGRESS, 0) != CURLE_OK {
        return false;
    }
    let xferinfo = (version == 0 || version >= CURL_XFERINFO_MIN_VERSION)
        && curl_easy_setopt_ptr(easy, CURLOPT_XFERINFOFUNCTION, cancel_xferinfo_cb as *const c_void) == CURLE_OK;
    let installed = (xferinfo
        || curl_easy_setopt_ptr(easy, CURLOPT_PROGRESSFUNCTION, cancel_progress_cb as *const c_void) == CURLE_OK)
        && curl_easy_setopt_ptr(easy, CURLOPT_XFERINFODATA, data) == CURLE_OK;
    if !installed {
        curl_easy_setopt_long(easy, CURLOPT_NOPROGRESS, 1);
    }
    installed
}

extern "C" fn cancel_xferinfo_cb(clientp: *mut c_void, _dltotal: i64, _dlnow: i64, _ultotal: i64, _ulnow: i64) -> c_int {
    cancelled(clientp)
}

extern "C" fn cancel_progress_cb(clientp: *mut c_void, _dltotal: f64, _dlnow: f64, _ultotal: f64, _ulnow: f64) -> c_int {
    cancelled(clientp)
}

fn cancelled(clientp: *mut c_void) -> c_int {
    if clientp.is_null() {
        return 1;
    }
    c_int::from(unsafe { &*(clientp as *const AtomicBool) }.load(Ordering::Acquire))
}
