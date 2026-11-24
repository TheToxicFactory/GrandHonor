use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::ptr;
use std::sync::Once;
use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, RPC_E_TOO_LATE, S_FALSE, S_OK};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED, CoCreateInstance,
    CoInitializeEx, CoInitializeSecurity, CoSetProxyBlanket, CoUninitialize, EOAC_NONE,
    RPC_C_AUTHN_LEVEL_CALL, RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE, SAFEARRAY,
};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayCreateVector, SafeArrayGetLBound, SafeArrayGetUBound,
    SafeArrayUnaccessData,
};
use windows::Win32::System::Variant::{
    VARIANT, VT_ARRAY, VT_BOOL, VT_BSTR, VT_EMPTY, VT_I1, VT_I2, VT_I4, VT_I8, VT_NULL, VT_UI1,
    VT_UI2, VT_UI4, VT_UI8, VariantClear,
};
use windows::Win32::System::Wmi::{
    IEnumWbemClassObject, IWbemClassObject, IWbemLocator, IWbemServices,
    WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_INFINITE, WbemLocator,
};
use windows_core::{BSTR, HRESULT, PCWSTR};

const RPC_C_AUTHN_WINNT: u32 = 10;
const RPC_C_AUTHZ_NONE: u32 = 0;

static SECURITY_INIT: Once = Once::new();

pub fn ensure_com_security() {
    SECURITY_INIT.call_once(|| unsafe {
        let hr = CoInitializeSecurity(
            None,
            -1,
            None,
            None,
            RPC_C_AUTHN_LEVEL_DEFAULT,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
            None,
        );
        if let Err(err) = hr
            && err.code() != RPC_E_TOO_LATE
        {
            crate::log::error_with("CoInitializeSecurity failed", &err);
        }
    });
}

pub struct ComGuard {
    should_uninit: bool,
}

impl ComGuard {
    pub fn mta() -> Self {
        Self::init(COINIT_MULTITHREADED)
    }

    pub fn sta() -> Self {
        Self::init(COINIT_APARTMENTTHREADED)
    }

    fn init(coinit: windows::Win32::System::Com::COINIT) -> Self {
        let hr: HRESULT = unsafe { CoInitializeEx(None, coinit) };
        if hr == S_OK || hr == S_FALSE {
            ensure_com_security();
            Self {
                should_uninit: true,
            }
        } else {
            if hr != RPC_E_CHANGED_MODE {
                crate::log::error(&format!("CoInitializeEx failed: 0x{:08X}", hr.0 as u32));
            }
            ensure_com_security();
            Self {
                should_uninit: false,
            }
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.should_uninit {
            unsafe { CoUninitialize() };
        }
    }
}

pub fn connect_wmi(namespace: &str) -> windows_core::Result<IWbemServices> {
    unsafe {
        let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)?;
        let ns = BSTR::from(namespace);
        let services = locator.ConnectServer(
            &ns,
            &BSTR::new(),
            &BSTR::new(),
            &BSTR::new(),
            0,
            &BSTR::new(),
            None,
        )?;
        CoSetProxyBlanket(
            &services,
            RPC_C_AUTHN_WINNT,
            RPC_C_AUTHZ_NONE,
            None,
            RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        )?;
        Ok(services)
    }
}

pub fn set_proxy_blanket_enum(enumerator: &IEnumWbemClassObject) {
    unsafe {
        let _ = CoSetProxyBlanket(
            enumerator,
            RPC_C_AUTHN_WINNT,
            RPC_C_AUTHZ_NONE,
            None,
            RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        );
    }
}

pub fn wmi_query(
    services: &IWbemServices,
    wql: &str,
) -> windows_core::Result<Vec<IWbemClassObject>> {
    unsafe {
        let lang = BSTR::from("WQL");
        let query = BSTR::from(wql);
        let enumerator = services.ExecQuery(
            &lang,
            &query,
            WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
            None,
        )?;
        set_proxy_blanket_enum(&enumerator);
        let mut results = Vec::new();
        loop {
            let mut batch: [Option<IWbemClassObject>; 16] = Default::default();
            let mut returned = 0u32;
            let hr = enumerator.Next(WBEM_INFINITE, &mut batch, &mut returned);
            for obj in batch.into_iter().take(returned as usize).flatten() {
                results.push(obj);
            }
            if returned == 0 || hr.0 != 0 {
                break;
            }
        }
        Ok(results)
    }
}

pub fn wmi_instances(
    services: &IWbemServices,
    class_name: &str,
) -> windows_core::Result<Vec<IWbemClassObject>> {
    unsafe {
        let filter = BSTR::from(class_name);
        let enumerator = services.CreateInstanceEnum(
            &filter,
            WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
            None,
        )?;
        set_proxy_blanket_enum(&enumerator);
        let mut results = Vec::new();
        loop {
            let mut batch: [Option<IWbemClassObject>; 8] = Default::default();
            let mut returned = 0u32;
            let hr = enumerator.Next(WBEM_INFINITE, &mut batch, &mut returned);
            for obj in batch.into_iter().take(returned as usize).flatten() {
                results.push(obj);
            }
            if returned == 0 || hr.0 != 0 {
                break;
            }
        }
        Ok(results)
    }
}

pub struct OwnedVariant(pub VARIANT);

impl Default for OwnedVariant {
    fn default() -> Self {
        Self(VARIANT::default())
    }
}

impl Drop for OwnedVariant {
    fn drop(&mut self) {
        unsafe {
            let _ = VariantClear(&mut self.0);
        }
    }
}

impl OwnedVariant {
    pub fn from_u32(val: u32) -> Self {
        let mut v = VARIANT::default();
        unsafe {
            let inner = &mut *v.Anonymous.Anonymous;
            inner.vt = VT_I4;
            inner.Anonymous.lVal = val as i32;
        }
        Self(v)
    }

    pub fn from_u8_slice(bytes: &[u8]) -> windows_core::Result<Self> {
        unsafe {
            let psa = SafeArrayCreateVector(VT_UI1, 0, bytes.len() as u32);
            if psa.is_null() {
                return Err(windows_core::Error::from_win32());
            }
            let mut data_ptr: *mut c_void = ptr::null_mut();
            if let Err(err) = SafeArrayAccessData(psa, &mut data_ptr) {
                let _ = windows::Win32::System::Ole::SafeArrayDestroy(psa);
                return Err(err);
            }
            ptr::copy_nonoverlapping(bytes.as_ptr(), data_ptr as *mut u8, bytes.len());
            SafeArrayUnaccessData(psa)?;

            let mut v = VARIANT::default();
            let inner = &mut *v.Anonymous.Anonymous;
            inner.vt = windows::Win32::System::Variant::VARENUM(VT_ARRAY.0 | VT_UI1.0);
            inner.Anonymous.parray = psa;
            Ok(Self(v))
        }
    }

    pub fn is_null_or_empty(&self) -> bool {
        let vt = unsafe { self.0.Anonymous.Anonymous.vt };
        vt == VT_EMPTY || vt == VT_NULL
    }

    pub fn as_string(&self) -> Option<String> {
        unsafe {
            let vt = self.0.Anonymous.Anonymous.vt;
            if vt == VT_BSTR {
                let bstr: &ManuallyDrop<BSTR> = &self.0.Anonymous.Anonymous.Anonymous.bstrVal;
                Some(bstr.to_string())
            } else {
                None
            }
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        unsafe {
            let vt = self.0.Anonymous.Anonymous.vt;
            let u = &self.0.Anonymous.Anonymous.Anonymous;
            match vt {
                v if v == VT_I1 => Some(u.cVal as i64),
                v if v == VT_UI1 => Some(u.bVal as i64),
                v if v == VT_I2 => Some(u.iVal as i64),
                v if v == VT_UI2 => Some(u.uiVal as i64),
                v if v == VT_I4 => Some(u.lVal as i64),
                v if v == VT_UI4 => Some(u.ulVal as i64),
                v if v == VT_I8 => Some(u.llVal),
                v if v == VT_UI8 => Some(u.ullVal as i64),
                v if v == VT_BOOL => Some(if u.boolVal.as_bool() { 1 } else { 0 }),
                v if v == VT_BSTR => u.bstrVal.to_string().trim().parse::<i64>().ok(),
                _ => None,
            }
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        unsafe {
            let vt = self.0.Anonymous.Anonymous.vt;
            let u = &self.0.Anonymous.Anonymous.Anonymous;
            match vt {
                v if v == VT_UI8 => Some(u.ullVal),
                v if v == VT_UI4 => Some(u.ulVal as u64),
                v if v == VT_I4 && u.lVal >= 0 => Some(u.lVal as u64),
                v if v == VT_I8 && u.llVal >= 0 => Some(u.llVal as u64),
                v if v == VT_UI2 => Some(u.uiVal as u64),
                v if v == VT_UI1 => Some(u.bVal as u64),
                v if v == VT_BSTR => u.bstrVal.to_string().trim().parse::<u64>().ok(),
                _ => self.as_i64().and_then(|v| u64::try_from(v).ok()),
            }
        }
    }

    pub fn as_bytes(&self) -> Option<Vec<u8>> {
        unsafe {
            let vt = self.0.Anonymous.Anonymous.vt;
            if vt.0 != (VT_ARRAY.0 | VT_UI1.0) {
                return None;
            }
            let psa: *mut SAFEARRAY = self.0.Anonymous.Anonymous.Anonymous.parray;
            if psa.is_null() {
                return None;
            }
            let lbound = SafeArrayGetLBound(psa, 1).ok()?;
            let ubound = SafeArrayGetUBound(psa, 1).ok()?;
            let len = (ubound - lbound + 1).max(0) as usize;
            let mut ptr_data: *mut c_void = ptr::null_mut();
            SafeArrayAccessData(psa, &mut ptr_data).ok()?;
            let mut out = vec![0u8; len];
            if len > 0 {
                ptr::copy_nonoverlapping(ptr_data as *const u8, out.as_mut_ptr(), len);
            }
            let _ = SafeArrayUnaccessData(psa);
            Some(out)
        }
    }
}

pub fn get_prop(obj: &IWbemClassObject, name: &str) -> windows_core::Result<OwnedVariant> {
    let mut val = OwnedVariant::default();
    let wide = windows_core::HSTRING::from(name);
    unsafe {
        obj.Get(PCWSTR(wide.as_ptr()), 0, &mut val.0, None, None)?;
    }
    Ok(val)
}

pub fn get_prop_string(obj: &IWbemClassObject, name: &str) -> Option<String> {
    let val = get_prop(obj, name).ok()?;
    if val.is_null_or_empty() {
        return None;
    }
    val.as_string()
}

pub fn get_prop_u64(obj: &IWbemClassObject, name: &str) -> Option<u64> {
    let val = get_prop(obj, name).ok()?;
    if val.is_null_or_empty() {
        return None;
    }
    val.as_u64()
}

pub fn put_prop(
    obj: &IWbemClassObject,
    name: &str,
    val: &OwnedVariant,
) -> windows_core::Result<()> {
    let wide = windows_core::HSTRING::from(name);
    unsafe { obj.Put(PCWSTR(wide.as_ptr()), 0, &val.0, 0) }
}
