use super::com::ComGuard;
use super::wmi::set_mic_mute_led;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{
    AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE_ACTIVE, IMMDeviceEnumerator, MMDeviceEnumerator,
    eCapture, eConsole, eRender,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows_core::{GUID, implement};

const E_NOTFOUND_HRESULT: i32 = -2147023728i32; // 0x80070490

/// One Windows volume step on the current default playback device.
pub fn step_output_volume(up: bool) -> bool {
    let _com = ComGuard::mta();
    let volume = unsafe {
        CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER)
            .and_then(|enumerator| enumerator.GetDefaultAudioEndpoint(eRender, eConsole))
            .and_then(|device| device.Activate::<IAudioEndpointVolume>(CLSCTX_INPROC_SERVER, None))
    };
    volume.is_ok_and(|volume| unsafe {
        if up { volume.VolumeStepUp(&GUID::zeroed()) } else { volume.VolumeStepDown(&GUID::zeroed()) }
    }.is_ok())
}

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeCallback {
    led_state: Arc<AtomicI32>,
    disposed: Arc<AtomicBool>,
}

impl IAudioEndpointVolumeCallback_Impl for VolumeCallback_Impl {
    fn OnNotify(&self, pnotify: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> windows_core::Result<()> {
        if pnotify.is_null() || self.disposed.load(Ordering::Acquire) {
            return Ok(());
        }
        let muted = unsafe { (*pnotify).bMuted.as_bool() };
        let desired = if muted { 1 } else { 0 };
        if self.led_state.load(Ordering::Acquire) != desired {
            let state_ref = self.led_state.clone();
            thread::spawn(move || {
                update_led(&state_ref, muted, false);
            });
        }
        Ok(())
    }
}

struct AudioState {
    enumerator: Option<IMMDeviceEnumerator>,
    volume: Option<IAudioEndpointVolume>,
    callback: Option<IAudioEndpointVolumeCallback>,
    endpoint_id: Option<String>,
    failure_logged: bool,
}

pub struct MicMuteService {
    gate: Mutex<AudioState>,
    led_state: Arc<AtomicI32>,
    disposed: Arc<AtomicBool>,
}

impl MicMuteService {
    pub fn new() -> Self {
        Self {
            gate: Mutex::new(AudioState {
                enumerator: None,
                volume: None,
                callback: None,
                endpoint_id: None,
                failure_logged: false,
            }),
            led_state: Arc::new(AtomicI32::new(-1)),
            disposed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn sync(&self) {
        let _com = ComGuard::mta();
        let Ok(mut state) = self.gate.lock() else {
            return;
        };
        if self.disposed.load(Ordering::Acquire) {
            return;
        }
        let Some(volume) = self.resolve(&mut state) else {
            return;
        };
        if let Ok(muted) = unsafe { volume.GetMute() } {
            update_led(&self.led_state, muted.as_bool(), true);
        }
    }

    pub fn toggle(&self) -> Option<bool> {
        let _com = ComGuard::mta();
        let Ok(mut state) = self.gate.lock() else {
            return None;
        };
        if self.disposed.load(Ordering::Acquire) {
            return None;
        }
        let volume = self.resolve(&mut state)?;
        let muted = match unsafe { volume.GetMute() } {
            Ok(v) => v.as_bool(),
            Err(_) => {
                release_volume(&mut state);
                return None;
            }
        };
        let target = !muted;
        if unsafe { volume.SetMute(target, &GUID::zeroed()) }.is_err() {
            release_volume(&mut state);
            return None;
        }
        // Like HONOR's "All microphones disabled": every other active capture device (a headset,
        // a USB or Bluetooth mic) follows the built-in one, so nothing keeps listening.
        let others = state
            .enumerator
            .as_ref()
            .map(|e| set_all_capture_mute(e, target, state.endpoint_id.as_deref()))
            .unwrap_or(0);
        update_led(&self.led_state, target, false);
        crate::log::info(&format!(
            "Microphone {} by Fn key ({} more device(s))",
            if target { "muted" } else { "unmuted" },
            others
        ));
        Some(target)
    }

    /// Current mute state of the default microphone, if there is one.
    pub fn is_muted(&self) -> Option<bool> {
        let _com = ComGuard::mta();
        let mut state = self.gate.lock().ok()?;
        if self.disposed.load(Ordering::Acquire) {
            return None;
        }
        let volume = self.resolve(&mut state)?;
        unsafe { volume.GetMute() }.ok().map(|v| v.as_bool())
    }

    fn resolve(&self, state: &mut AudioState) -> Option<IAudioEndpointVolume> {
        if state.enumerator.is_none() {
            match unsafe {
                CoCreateInstance::<_, IMMDeviceEnumerator>(
                    &MMDeviceEnumerator,
                    None,
                    CLSCTX_INPROC_SERVER,
                )
            } {
                Ok(enum_obj) => state.enumerator = Some(enum_obj),
                Err(err) => {
                    log_once(state, &format!("Microphone mute is unavailable: {err}"));
                    return None;
                }
            }
        }
        let enumerator = state.enumerator.as_ref()?;
        let device = match unsafe { enumerator.GetDefaultAudioEndpoint(eCapture, eConsole) } {
            Ok(dev) => dev,
            Err(err) => {
                if err.code().0 != E_NOTFOUND_HRESULT {
                    log_once(
                        state,
                        &format!("No default capture endpoint (0x{:08X})", err.code().0 as u32),
                    );
                }
                release_volume(state);
                return None;
            }
        };

        let id = unsafe {
            match device.GetId() {
                Ok(pwstr) => {
                    let s = pwstr.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(pwstr.as_ptr() as *const _));
                    s
                }
                Err(_) => {
                    release_volume(state);
                    return None;
                }
            }
        };

        if let Some(ref existing) = state.volume
            && state.endpoint_id.as_deref() == Some(id.as_str())
        {
            return Some(existing.clone());
        }

        release_volume(state);

        let volume: IAudioEndpointVolume = match unsafe { device.Activate(CLSCTX_INPROC_SERVER, None) }
        {
            Ok(v) => v,
            Err(_) => {
                log_once(state, "Could not activate the capture endpoint volume");
                return None;
            }
        };

        let callback: IAudioEndpointVolumeCallback = VolumeCallback {
            led_state: self.led_state.clone(),
            disposed: self.disposed.clone(),
        }
        .into();

        if unsafe { volume.RegisterControlChangeNotify(&callback) }.is_ok() {
            state.callback = Some(callback);
        }
        state.volume = Some(volume.clone());
        state.endpoint_id = Some(id);
        state.failure_logged = false;
        Some(volume)
    }
}

impl Drop for MicMuteService {
    fn drop(&mut self) {
        self.disposed.store(true, Ordering::Release);
        if let Ok(mut state) = self.gate.lock() {
            release_volume(&mut state);
            state.enumerator = None;
        }
    }
}

/// Sets mute on every active capture endpoint except `skip_id`. Returns how many changed.
fn set_all_capture_mute(enumerator: &IMMDeviceEnumerator, mute: bool, skip_id: Option<&str>) -> usize {
    let Ok(devices) = (unsafe { enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) }) else {
        return 0;
    };
    let count = unsafe { devices.GetCount() }.unwrap_or(0);
    let mut changed = 0;
    for i in 0..count {
        let Ok(device) = (unsafe { devices.Item(i) }) else { continue };
        let id = unsafe {
            device.GetId().ok().map(|p| {
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.as_ptr() as *const _));
                s
            })
        };
        if id.is_some() && id.as_deref() == skip_id {
            continue;
        }
        let Ok(volume) = (unsafe { device.Activate::<IAudioEndpointVolume>(CLSCTX_INPROC_SERVER, None) }) else {
            continue;
        };
        if unsafe { volume.SetMute(mute, &GUID::zeroed()) }.is_ok() {
            changed += 1;
        }
    }
    changed
}

fn release_volume(state: &mut AudioState) {
    if let (Some(vol), Some(cb)) = (&state.volume, &state.callback) {
        unsafe {
            let _ = vol.UnregisterControlChangeNotify(cb);
        }
    }
    state.callback = None;
    state.volume = None;
    state.endpoint_id = None;
}

fn update_led(led_state: &AtomicI32, muted: bool, force: bool) {
    let target = if muted { 1 } else { 0 };
    if led_state.swap(target, Ordering::AcqRel) == target && !force {
        return;
    }
    if !set_mic_mute_led(muted) {
        let _ = led_state.compare_exchange(target, -1, Ordering::AcqRel, Ordering::Acquire);
    }
}

fn log_once(state: &mut AudioState, msg: &str) {
    if !state.failure_logged {
        state.failure_logged = true;
        crate::log::error(msg);
    }
}
