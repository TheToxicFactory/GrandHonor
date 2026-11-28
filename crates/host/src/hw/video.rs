//! Display → Video: Intel's driver video enhancements (sharpness, noise reduction, adaptive
//! contrast, skin tone) for apps that play video through the Windows video processor (Media
//! Player and other Windows players). Browsers present video their own way: no effect there. Talks to Intel's Graphics Control Library,
//! `ControlLib.dll`, which the Intel graphics driver installs in System32; GrandHonor ships no
//! Intel code. The driver keeps the values itself across reboots, so nothing is stored here
//! and nothing runs in the background.
//!
//! Layouts follow `igcl_api.h` (IGCL 1.1) for x64 and were checked against the compiled
//! header: getset 120 bytes, custom noise reduction / contrast struct 84 bytes.

use std::ffi::c_void;
use std::sync::OnceLock;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows_core::{HSTRING, PCSTR};

const SUCCESS: u32 = 0;
/// The driver has no value of its own yet: the feature is at its off state.
const DATA_NOT_FOUND: u32 = 0x4000_0014;
const VALUE_TYPE_UINT32: u32 = 3;
const VALUE_TYPE_CUSTOM: u32 = 5;
const IMPL_VERSION: u32 = (1 << 16) | 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    NoiseReduction = 1,
    Sharpness = 2,
    AdaptiveContrast = 3,
    SkinTone = 7,
}

impl Feature {
    fn custom(self) -> bool {
        matches!(self, Feature::NoiseReduction | Feature::AdaptiveContrast)
    }
}

/// What the driver applies: off, a level, or (noise reduction only) automatic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Level {
    pub enabled: bool,
    pub value: u32,
    pub auto: bool,
}

#[repr(C)]
struct InitArgs {
    size: u32,
    version: u8,
    app_version: u32,
    flags: u32,
    supported_version: u32,
    application_uid: [u8; 16],
}

#[repr(C)]
struct FeatureDetails {
    size: u32,
    version: u8,
    feature_type: u32,
    value_type: u32,
    value: [u32; 6],
    custom_value_size: i32,
    custom_value: *mut c_void,
    reserved: [u32; 16],
}

#[repr(C)]
struct FeatureCaps {
    size: u32,
    version: u8,
    count: u32,
    details: *mut FeatureDetails,
    reserved: [u32; 16],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PropertyUint {
    enable: bool,
    value: u32,
}

#[repr(C)]
struct GetSet {
    size: u32,
    version: u8,
    feature_type: u32,
    application_name: *mut u8,
    application_name_length: i8,
    set: bool,
    value_type: u32,
    value: PropertyUint,
    custom_value_size: i32,
    custom_value: *mut c_void,
    reserved: [u32; 16],
}

/// Noise reduction and adaptive contrast share this shape: a level and a second switch
/// (noise reduction: automatic level; contrast: coexistence with the display's own).
#[repr(C)]
#[derive(Clone, Copy)]
struct LevelAndFlag {
    size: u32,
    version: u8,
    level: PropertyUint,
    flag: bool,
    reserved: [u32; 16],
}

type InitFn = unsafe extern "C" fn(*mut InitArgs, *mut *mut c_void) -> u32;
type CloseFn = unsafe extern "C" fn(*mut c_void) -> u32;
type EnumerateFn = unsafe extern "C" fn(*mut c_void, *mut u32, *mut *mut c_void) -> u32;
type CapsFn = unsafe extern "C" fn(*mut c_void, *mut FeatureCaps) -> u32;
type GetSetFn = unsafe extern "C" fn(*mut c_void, *mut GetSet) -> u32;

struct Api {
    init: InitFn,
    close: CloseFn,
    enumerate: EnumerateFn,
    caps: CapsFn,
    getset: GetSetFn,
}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        // System32 only: the driver's copy, never one next to the program or in PATH.
        let lib: HMODULE = LoadLibraryExW(&HSTRING::from("ControlLib.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
        let get = |name: &[u8]| GetProcAddress(lib, PCSTR(name.as_ptr()));
        Some(Api {
            init: std::mem::transmute::<_, InitFn>(get(b"ctlInit\0")?),
            close: std::mem::transmute::<_, CloseFn>(get(b"ctlClose\0")?),
            enumerate: std::mem::transmute::<_, EnumerateFn>(get(b"ctlEnumerateDevices\0")?),
            caps: std::mem::transmute::<_, CapsFn>(get(b"ctlGetSupportedVideoProcessingCapabilities\0")?),
            getset: std::mem::transmute::<_, GetSetFn>(get(b"ctlGetSetVideoProcessingFeature\0")?),
        })
    })
    .as_ref()
}

/// One IGCL session for one call: opened, used on the first adapter with video features,
/// closed. Calls are rare (opening the Display tab, a click), so nothing is kept open.
fn with_adapter<T>(run: impl FnOnce(&Api, *mut c_void) -> Option<T>) -> Option<T> {
    let api = api()?;
    unsafe {
        let mut args = InitArgs { size: size_of::<InitArgs>() as u32, version: 0, app_version: IMPL_VERSION, flags: 0, supported_version: 0, application_uid: [0; 16] };
        let mut handle: *mut c_void = std::ptr::null_mut();
        if (api.init)(&mut args, &mut handle) != SUCCESS || handle.is_null() {
            return None;
        }
        let mut count = 0u32;
        let mut adapters = [std::ptr::null_mut::<c_void>(); 4];
        let mut result = None;
        if (api.enumerate)(handle, &mut count, std::ptr::null_mut()) == SUCCESS && count > 0 {
            count = count.min(adapters.len() as u32);
            if (api.enumerate)(handle, &mut count, adapters.as_mut_ptr()) == SUCCESS {
                for adapter in adapters.iter().take(count as usize).copied() {
                    let mut caps = FeatureCaps { size: size_of::<FeatureCaps>() as u32, version: 0, count: 0, details: std::ptr::null_mut(), reserved: [0; 16] };
                    if (api.caps)(adapter, &mut caps) == SUCCESS && caps.count > 0 {
                        result = run(api, adapter);
                        break;
                    }
                }
            }
        }
        (api.close)(handle);
        result
    }
}

/// The video enhancements this laptop's driver offers (empty without an Intel driver).
pub fn supported() -> Vec<Feature> {
    static CACHE: OnceLock<Vec<Feature>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            with_adapter(|api, adapter| unsafe {
                let mut caps = FeatureCaps { size: size_of::<FeatureCaps>() as u32, version: 0, count: 0, details: std::ptr::null_mut(), reserved: [0; 16] };
                (api.caps)(adapter, &mut caps);
                let mut details: Vec<FeatureDetails> = (0..caps.count)
                    .map(|_| FeatureDetails {
                        size: size_of::<FeatureDetails>() as u32,
                        version: 0,
                        feature_type: 0,
                        value_type: 0,
                        value: [0; 6],
                        custom_value_size: 0,
                        custom_value: std::ptr::null_mut(),
                        reserved: [0; 16],
                    })
                    .collect();
                caps.details = details.as_mut_ptr();
                if (api.caps)(adapter, &mut caps) != SUCCESS {
                    return None;
                }
                let all = [Feature::Sharpness, Feature::NoiseReduction, Feature::AdaptiveContrast, Feature::SkinTone];
                Some(all.into_iter().filter(|f| details.iter().any(|d| d.feature_type == *f as u32)).collect())
            })
            .unwrap_or_default()
        })
        .clone()
}

fn call(feature: Feature, set: Option<Level>) -> Option<Level> {
    with_adapter(|api, adapter| unsafe {
        let mut name = [0u8; 1];
        let mut custom = LevelAndFlag { size: size_of::<LevelAndFlag>() as u32, version: 0, level: PropertyUint::default(), flag: false, reserved: [0; 16] };
        let value = set.map(|level| PropertyUint { enable: level.enabled, value: level.value }).unwrap_or_default();
        if let Some(level) = set {
            custom.level = value;
            // Only noise reduction has "automatic"; contrast's second switch stays off.
            custom.flag = feature == Feature::NoiseReduction && level.auto;
        }
        let mut request = GetSet {
            size: size_of::<GetSet>() as u32,
            version: 0,
            feature_type: feature as u32,
            application_name: name.as_mut_ptr(),
            application_name_length: 0,
            set: false,
            value_type: if feature.custom() { VALUE_TYPE_CUSTOM } else { VALUE_TYPE_UINT32 },
            value: PropertyUint::default(),
            custom_value_size: 0,
            custom_value: std::ptr::null_mut(),
            reserved: [0; 16],
        };
        if feature.custom() {
            request.custom_value_size = size_of::<LevelAndFlag>() as i32;
            request.custom_value = (&mut custom as *mut LevelAndFlag).cast();
        }
        if let Some(level) = set {
            request.set = true;
            if !feature.custom() {
                request.value = value;
            }
            return ((api.getset)(adapter, &mut request) == SUCCESS).then_some(level);
        }
        match (api.getset)(adapter, &mut request) {
            SUCCESS => {
                let v = if feature.custom() { custom.level } else { request.value };
                let auto = feature == Feature::NoiseReduction && custom.flag;
                Some(Level { enabled: v.enable, value: v.value, auto })
            }
            DATA_NOT_FOUND => Some(Level { enabled: false, value: 0, auto: false }),
            _ => None,
        }
    })
}

/// What the driver applies now; `None` when it could not be read.
pub fn get(feature: Feature) -> Option<Level> {
    call(feature, None)
}

pub fn set(feature: Feature, level: Level) -> Result<(), String> {
    call(feature, Some(level)).map(|_| ()).ok_or_else(|| {
        grandhonor_core::locale::t(
            "Драйвер Intel не принял настройку видео",
            "The Intel driver did not accept the video setting",
            "Intel 驱动未接受该视频设置",
        )
        .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_match_the_intel_header() {
        // Sizes and offsets printed from igcl_api.h compiled with MSVC x64.
        assert_eq!(size_of::<InitArgs>(), 36);
        assert_eq!(size_of::<GetSet>(), 120);
        assert_eq!(std::mem::offset_of!(GetSet, value), 32);
        assert_eq!(std::mem::offset_of!(GetSet, custom_value), 48);
        assert_eq!(size_of::<LevelAndFlag>(), 84);
        assert_eq!(std::mem::offset_of!(LevelAndFlag, flag), 16);
        assert_eq!(size_of::<FeatureCaps>(), 88);
        assert_eq!(size_of::<FeatureDetails>(), 120);
        assert_eq!(std::mem::offset_of!(FeatureDetails, custom_value), 48);
    }
}
