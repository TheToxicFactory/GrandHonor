use grandhonor_core::url_guard::validate_redirect_url;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use url::Url;
use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_NO_PROXY, WINHTTP_DISABLE_REDIRECTS, WINHTTP_FLAG_SECURE,
    WINHTTP_OPEN_REQUEST_FLAGS, WINHTTP_OPTION_DISABLE_FEATURE, WINHTTP_QUERY_CONTENT_LENGTH,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_LOCATION, WINHTTP_QUERY_STATUS_CODE,
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts,
};
use windows_core::{HSTRING, PCWSTR};

pub const MAX_PACKAGE_SIZE: u64 = 2 * 1024 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

struct WinHttpHandle(*mut std::ffi::c_void);
impl Drop for WinHttpHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

pub struct HttpResponseData {
    pub status: u16,
    pub body: Vec<u8>,
    /// GitHub API `X-RateLimit-Reset` (Unix seconds), when the server sent it.
    pub rate_limit_reset: Option<u64>,
}

/// A response header by name (WINHTTP_QUERY_CUSTOM), for small values.
fn query_custom_header(req: &WinHttpHandle, name: &str) -> Option<String> {
    let name_h = HSTRING::from(name);
    let mut buf = [0u16; 128];
    let mut len = (buf.len() * 2) as u32;
    unsafe {
        WinHttpQueryHeaders(
            req.0,
            windows::Win32::Networking::WinHttp::WINHTTP_QUERY_CUSTOM,
            &name_h,
            Some(buf.as_mut_ptr() as *mut std::ffi::c_void),
            &mut len,
            std::ptr::null_mut(),
        )
    }
    .ok()?;
    Some(String::from_utf16_lossy(&buf[..(len as usize / 2).min(buf.len())]).trim().to_string())
}

fn open_session(timeout_ms: i32) -> Result<WinHttpHandle, String> {
    let ua = HSTRING::from(concat!("GrandHonor/", env!("CARGO_PKG_VERSION")));
    let raw = unsafe {
        WinHttpOpen(
            &ua,
            WINHTTP_ACCESS_TYPE_NO_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        )
    };
    if raw.is_null() {
        return Err("WinHttpOpen failed".to_string());
    }
    let session = WinHttpHandle(raw);
    unsafe {
        WinHttpSetTimeouts(session.0, timeout_ms, timeout_ms, timeout_ms, timeout_ms)
            .map_err(|e| format!("WinHttpSetTimeouts failed: {e}"))?;
    }
    Ok(session)
}

fn open_request(
    session: &WinHttpHandle,
    method: &str,
    url: &Url,
    disable_redirects: bool,
) -> Result<(WinHttpHandle, WinHttpHandle), String> {
    let host = url.host_str().ok_or("Missing URL host")?;
    let port = url.port_or_known_default().unwrap_or(443);
    let host_h = HSTRING::from(host);
    let conn_raw = unsafe { WinHttpConnect(session.0, &host_h, port, 0) };
    if conn_raw.is_null() {
        return Err(format!("WinHttpConnect({host}) failed"));
    }
    let conn = WinHttpHandle(conn_raw);

    let mut path_and_query = url.path().to_string();
    if path_and_query.is_empty() {
        path_and_query.push('/');
    }
    if let Some(q) = url.query() {
        path_and_query.push('?');
        path_and_query.push_str(q);
    }

    let verb_h = HSTRING::from(method);
    let path_h = HSTRING::from(path_and_query);
    let flags = if url.scheme() == "https" {
        WINHTTP_FLAG_SECURE
    } else {
        WINHTTP_OPEN_REQUEST_FLAGS(0)
    };
    let req_raw = unsafe {
        WinHttpOpenRequest(
            conn.0,
            &verb_h,
            &path_h,
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            flags,
        )
    };
    if req_raw.is_null() {
        return Err("WinHttpOpenRequest failed".to_string());
    }
    let req = WinHttpHandle(req_raw);

    if disable_redirects {
        let val = WINHTTP_DISABLE_REDIRECTS.to_le_bytes();
        unsafe { WinHttpSetOption(Some(req.0), WINHTTP_OPTION_DISABLE_FEATURE, Some(&val)) }
            .map_err(|e| format!("Failed to disable HTTP redirects: {e}"))?;
    }

    Ok((conn, req))
}

fn query_status_code(req: &WinHttpHandle) -> Result<u16, String> {
    let mut code = 0u32;
    let mut len = std::mem::size_of::<u32>() as u32;
    unsafe {
        WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut code as *mut u32 as *mut std::ffi::c_void),
            &mut len,
            std::ptr::null_mut(),
        )
    }
    .map_err(|e| format!("WinHttpQueryHeaders(status) failed: {e}"))?;
    Ok(code as u16)
}

fn query_content_length(req: &WinHttpHandle) -> Option<u64> {
    let mut buf = [0u16; 64];
    let mut len = (buf.len() * 2) as u32;
    unsafe {
        WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_CONTENT_LENGTH,
            PCWSTR::null(),
            Some(buf.as_mut_ptr() as *mut std::ffi::c_void),
            &mut len,
            std::ptr::null_mut(),
        )
    }
    .ok()?;
    let wchars = (len as usize) / 2;
    String::from_utf16_lossy(&buf[..wchars])
        .trim()
        .parse::<u64>()
        .ok()
}

fn query_location_header(req: &WinHttpHandle) -> Option<String> {
    let mut len = 0u32;
    let _ = unsafe {
        WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_LOCATION,
            PCWSTR::null(),
            None,
            &mut len,
            std::ptr::null_mut(),
        )
    };
    if len < 2 || len > 16384 {
        return None;
    }
    let mut buf = vec![0u16; (len as usize).div_ceil(2)];
    unsafe {
        WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_LOCATION,
            PCWSTR::null(),
            Some(buf.as_mut_ptr() as *mut std::ffi::c_void),
            &mut len,
            std::ptr::null_mut(),
        )
    }
    .ok()?;
    let count = (len as usize / 2).min(buf.len());
    Some(
        String::from_utf16_lossy(&buf[..count])
            .trim_end_matches('\0')
            .to_string(),
    )
}

/// Transport failures worth another try (the HONOR CDNs drop connections now and then):
/// invalid server response, connection reset/aborted, timeout, cannot connect, name not resolved.
fn is_transient(err: &str) -> bool {
    ["0x80072F78", "0x80072EFE", "0x80072EFF", "0x80072EE2", "0x80072EFD", "0x80072EE7"]
        .iter()
        .any(|code| err.contains(code))
}

/// Runs `op` up to three times, waiting 1.5 s and then 4 s after a transient transport error.
fn with_retries<T>(what: &str, mut op: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    let mut attempt = 0;
    loop {
        match op() {
            Err(err) if is_transient(&err) && attempt < 2 => {
                attempt += 1;
                crate::log::info(&format!("{what}: {err}; retry {attempt} of 2"));
                std::thread::sleep(std::time::Duration::from_millis(if attempt == 1 { 1_500 } else { 4_000 }));
            }
            other => return other,
        }
    }
}

pub fn get_text(url_str: &str, timeout_ms: i32) -> Result<HttpResponseData, String> {
    with_retries("GET", || get_text_once(url_str, timeout_ms))
}

fn get_text_once(url_str: &str, timeout_ms: i32) -> Result<HttpResponseData, String> {
    let url = Url::parse(url_str).map_err(|e| e.to_string())?;
    let session = open_session(timeout_ms)?;
    let (_conn, req) = open_request(&session, "GET", &url, false)?;
    unsafe {
        WinHttpSendRequest(req.0, None, None, 0, 0, 0)
            .map_err(|e| format!("WinHttpSendRequest failed: {e}"))?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut())
            .map_err(|e| format!("WinHttpReceiveResponse failed: {e}"))?;
    }
    let status = query_status_code(&req)?;
    let rate_limit_reset = query_custom_header(&req, "X-RateLimit-Reset").and_then(|v| v.parse().ok());
    let body = read_all_bytes(&req, 32 * 1024 * 1024)?;
    Ok(HttpResponseData { status, body, rate_limit_reset })
}

pub fn post_json(url_str: &str, json_body: &[u8], timeout_ms: i32) -> Result<HttpResponseData, String> {
    with_retries("POST", || post_json_once(url_str, json_body, timeout_ms))
}

fn post_json_once(url_str: &str, json_body: &[u8], timeout_ms: i32) -> Result<HttpResponseData, String> {
    let url = Url::parse(url_str).map_err(|e| e.to_string())?;
    let session = open_session(timeout_ms)?;
    let (_conn, req) = open_request(&session, "POST", &url, false)?;
    let headers: Vec<u16> = "Content-Type: application/json; charset=utf-8\r\n"
        .encode_utf16()
        .collect();
    unsafe {
        WinHttpSendRequest(
            req.0,
            Some(&headers),
            Some(json_body.as_ptr() as *const std::ffi::c_void),
            json_body.len() as u32,
            json_body.len() as u32,
            0,
        )
        .map_err(|e| format!("WinHttpSendRequest(POST) failed: {e}"))?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut())
            .map_err(|e| format!("WinHttpReceiveResponse(POST) failed: {e}"))?;
    }
    let status = query_status_code(&req)?;
    let body = read_all_bytes(&req, 32 * 1024 * 1024)?;
    Ok(HttpResponseData { status, body, rate_limit_reset: None })
}

pub fn head_package(url: &Url, timeout_ms: i32) -> Result<u16, String> {
    let session = open_session(timeout_ms)?;
    let mut current = url.clone();
    for _ in 0..=MAX_REDIRECTS {
        let (_conn, req) = open_request(&session, "HEAD", &current, true)?;
        unsafe {
            WinHttpSendRequest(req.0, None, None, 0, 0, 0)
                .map_err(|e| format!("WinHttpSendRequest(HEAD) failed: {e}"))?;
            WinHttpReceiveResponse(req.0, std::ptr::null_mut())
                .map_err(|e| format!("WinHttpReceiveResponse(HEAD) failed: {e}"))?;
        }
        let status = query_status_code(&req)?;
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let location = query_location_header(&req)
                .ok_or("The driver package redirected without a Location header.")?;
            current = validate_redirect_url(&current, &location)
                .ok_or("The driver package redirected outside the trusted HTTPS sources.")?;
            continue;
        }
        return Ok(status);
    }
    Err("Too many driver package redirects.".to_string())
}

pub fn download_package_to_file(
    url: &Url,
    dest: &Path,
    expected_size: u64,
    timeout_ms: i32,
) -> Result<u16, String> {
    download_to_file_with(url, dest, expected_size, timeout_ms, validate_redirect_url)
}

/// Downloads with manual redirects, each checked by `allow_redirect` (HONOR or GitHub rules).
/// A dropped connection restarts the download (up to three attempts).
pub fn download_to_file_with(
    url: &Url,
    dest: &Path,
    expected_size: u64,
    timeout_ms: i32,
    allow_redirect: fn(&Url, &str) -> Option<Url>,
) -> Result<u16, String> {
    with_retries("Download", || download_once(url, dest, expected_size, timeout_ms, allow_redirect))
}

fn download_once(
    url: &Url,
    dest: &Path,
    expected_size: u64,
    timeout_ms: i32,
    allow_redirect: fn(&Url, &str) -> Option<Url>,
) -> Result<u16, String> {
    if expected_size > MAX_PACKAGE_SIZE {
        return Err("The driver package is too large.".to_string());
    }
    let session = open_session(timeout_ms)?;
    let mut current = url.clone();
    for _ in 0..=MAX_REDIRECTS {
        let (_conn, req) = open_request(&session, "GET", &current, true)?;
        unsafe {
            WinHttpSendRequest(req.0, None, None, 0, 0, 0)
                .map_err(|e| format!("WinHttpSendRequest(GET) failed: {e}"))?;
            WinHttpReceiveResponse(req.0, std::ptr::null_mut())
                .map_err(|e| format!("WinHttpReceiveResponse(GET) failed: {e}"))?;
        }
        let status = query_status_code(&req)?;
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let location = query_location_header(&req)
                .ok_or("The driver package redirected without a Location header.")?;
            current = allow_redirect(&current, &location)
                .ok_or("The download redirected outside the trusted HTTPS sources.")?;
            continue;
        }
        if status >= 400 {
            return Ok(status);
        }
        if let Some(total) = query_content_length(&req)
            && total > MAX_PACKAGE_SIZE
        {
            return Err("The driver package is too large.".to_string());
        }
        let mut file = File::create(dest).map_err(|e| e.to_string())?;
        let mut written = 0u64;
        let mut buf = vec![0u8; 65_536];
        loop {
            let mut available = 0u32;
            unsafe { WinHttpQueryDataAvailable(req.0, &mut available) }
                .map_err(|e| format!("WinHttpQueryDataAvailable failed: {e}"))?;
            if available == 0 {
                break;
            }
            let to_read = (available as usize).min(buf.len()) as u32;
            let mut read = 0u32;
            unsafe {
                WinHttpReadData(
                    req.0,
                    buf.as_mut_ptr() as *mut std::ffi::c_void,
                    to_read,
                    &mut read,
                )
            }
            .map_err(|e| format!("WinHttpReadData failed: {e}"))?;
            if read == 0 {
                break;
            }
            written += read as u64;
            if written > MAX_PACKAGE_SIZE {
                return Err("The driver package exceeded the download limit.".to_string());
            }
            file.write_all(&buf[..read as usize])
                .map_err(|e| e.to_string())?;
        }
        file.flush().map_err(|e| e.to_string())?;
        return Ok(status);
    }
    Err("Too many driver package redirects.".to_string())
}

fn read_all_bytes(req: &WinHttpHandle, max_bytes: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut buf = [0u8; 32_768];
    loop {
        let mut available = 0u32;
        unsafe { WinHttpQueryDataAvailable(req.0, &mut available) }
            .map_err(|e| format!("WinHttpQueryDataAvailable failed: {e}"))?;
        if available == 0 {
            break;
        }
        let to_read = (available as usize).min(buf.len()) as u32;
        let mut read = 0u32;
        unsafe {
            WinHttpReadData(
                req.0,
                buf.as_mut_ptr() as *mut std::ffi::c_void,
                to_read,
                &mut read,
            )
        }
        .map_err(|e| format!("WinHttpReadData failed: {e}"))?;
        if read == 0 {
            break;
        }
        if out.len() + read as usize > max_bytes {
            return Err("Response body exceeded memory limit.".to_string());
        }
        out.extend_from_slice(&buf[..read as usize]);
    }
    Ok(out)
}
