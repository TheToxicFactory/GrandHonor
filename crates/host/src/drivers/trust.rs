use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use windows::Win32::Foundation::HWND;
use windows::Win32::Security::WinTrust::{
    WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_DATA_PROVIDER_FLAGS,
    WINTRUST_FILE_INFO, WTD_CHOICE_FILE, WTD_REVOKE_WHOLECHAIN, WTD_UI_NONE,
    WinVerifyTrust,
};
use windows_core::{HSTRING, PCWSTR};

pub fn compute_sha256_hex(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65_536];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{b:02X}");
    }
    Ok(hex)
}

/// Valid Authenticode signature (trusted chain, file unmodified)?
pub fn verify_authenticode(path: &Path) -> bool {
    authenticode_signer(path).is_some()
}

/// Publishers whose signature is accepted for a package HONOR lists without a SHA-256.
const HONOR_SIGNERS: [&str; 1] = ["honordevicecoltd"];

/// True when the file's valid signature belongs to HONOR (Honor Device Co., Ltd.).
pub fn is_honor_signed(path: &Path) -> bool {
    authenticode_signer(path).is_some_and(|name| {
        let key: String = name.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_ascii_lowercase();
        HONOR_SIGNERS.contains(&key.as_str())
    })
}

/// Verifies the Authenticode signature and returns the signer's name (certificate subject
/// display name), or None when the signature is missing or invalid.
pub fn authenticode_signer(path: &Path) -> Option<String> {
    use windows::Win32::Security::Cryptography::{CERT_NAME_SIMPLE_DISPLAY_TYPE, CertGetNameStringW};
    use windows::Win32::Security::WinTrust::{
        WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData,
    };
    let wide = HSTRING::from(path.as_os_str());
    let mut file_info = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(wide.as_ptr()),
        ..Default::default()
    };
    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_WHOLECHAIN,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 {
            pFile: &mut file_info,
        },
        dwStateAction: WTD_STATEACTION_VERIFY,
        // Revocation is checked (a stolen, revoked certificate must fail); the root is exempt.
        dwProvFlags: WINTRUST_DATA_PROVIDER_FLAGS(0x0000_0080),
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let status = unsafe {
        WinVerifyTrust(HWND::default(), &mut action, &mut data as *mut WINTRUST_DATA as *mut std::ffi::c_void)
    };
    let name = (status == 0).then(|| unsafe {
        let prov = WTHelperProvDataFromStateData(data.hWVTStateData);
        if prov.is_null() {
            return String::new();
        }
        let signer = WTHelperGetProvSignerFromChain(prov, 0, false, 0);
        if signer.is_null() || (*signer).csCertChain == 0 || (*signer).pasCertChain.is_null() {
            return String::new();
        }
        let cert = (*(*signer).pasCertChain).pCert;
        if cert.is_null() {
            return String::new();
        }
        let mut buf = [0u16; 256];
        let len = CertGetNameStringW(cert, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, None, Some(&mut buf)) as usize;
        String::from_utf16_lossy(&buf[..len.saturating_sub(1).min(buf.len())])
    });
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        let _ = WinVerifyTrust(HWND::default(), &mut action, &mut data as *mut WINTRUST_DATA as *mut std::ffi::c_void);
    }
    name
}