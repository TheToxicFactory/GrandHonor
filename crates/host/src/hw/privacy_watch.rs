//! Notices when an app starts using the microphone or the camera, so GrandHonor can say "the
//! microphone is off, press F7" when you start a call with it switched off, as HONOR OSD did.
//!
//! Both sources are Windows callbacks; nothing is polled:
//! - Microphone: every active capture endpoint's `IAudioSessionManager2` reports new capture
//!   sessions, and each session's `IAudioSessionEvents::OnStateChanged` reports when it becomes
//!   active again (an app reuses its session for the next call).
//! - Camera: Media Foundation's `IMFSensorActivityMonitor` reports which processes stream from a
//!   camera; a process that starts streaming counts as a new use.
//!
//! The registering thread keeps the COM objects alive and then sleeps for the life of the host;
//! callbacks arrive on Windows' own (MTA) threads.

use super::com::ComGuard;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use windows::Win32::Media::Audio::{
    AudioSessionDisconnectReason, AudioSessionState, AudioSessionStateActive, DEVICE_STATE_ACTIVE,
    IAudioSessionControl, IAudioSessionEvents, IAudioSessionEvents_Impl, IAudioSessionManager2,
    IAudioSessionNotification, IAudioSessionNotification_Impl, IMMDeviceEnumerator, MMDeviceEnumerator,
    eCapture,
};
use windows::Win32::Media::MediaFoundation::{
    IMFSensorActivitiesReport, IMFSensorActivitiesReportCallback, IMFSensorActivitiesReportCallback_Impl,
    MF_VERSION, MFCreateSensorActivityMonitor, MFSTARTUP_LITE, MFStartup,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows_core::{BOOL, GUID, PCWSTR, Ref, implement};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    Microphone,
    Camera,
}

type Handler = Arc<dyn Fn(Device) + Send + Sync>;

/// Starts watching. `on_start` runs on a short-lived thread when an app begins to use a device.
pub fn start(on_start: impl Fn(Device) + Send + Sync + 'static) {
    let handler: Handler = Arc::new(on_start);
    let _ = std::thread::Builder::new()
        .name("privacy-watch".into())
        .stack_size(256 * 1024)
        .spawn(move || run(handler));
}

fn fire(handler: &Handler, device: Device) {
    let handler = handler.clone();
    let _ = std::thread::Builder::new()
        .name("privacy-notice".into())
        .stack_size(128 * 1024)
        .spawn(move || handler(device));
}

fn run(handler: Handler) {
    let _com = ComGuard::mta();
    // Kept alive for as long as this thread sleeps.
    let mut keep: Vec<windows_core::IUnknown> = Vec::new();
    let sessions: Arc<Mutex<Vec<(IAudioSessionControl, IAudioSessionEvents)>>> = Arc::default();

    match watch_microphones(&handler, &sessions) {
        Ok(objects) => keep.extend(objects),
        Err(err) => crate::log::error(&format!("Microphone use cannot be watched: {err}")),
    }
    match watch_cameras(&handler) {
        Ok(object) => keep.push(object),
        Err(err) => crate::log::error(&format!("Camera use cannot be watched: {err}")),
    }
    if keep.is_empty() {
        return;
    }
    loop {
        std::thread::park();
    }
}

// ---- Microphone ------------------------------------------------------------------------------

#[implement(IAudioSessionEvents)]
struct SessionEvents {
    handler: Handler,
}

impl IAudioSessionEvents_Impl for SessionEvents_Impl {
    fn OnDisplayNameChanged(&self, _: &PCWSTR, _: *const GUID) -> windows_core::Result<()> {
        Ok(())
    }
    fn OnIconPathChanged(&self, _: &PCWSTR, _: *const GUID) -> windows_core::Result<()> {
        Ok(())
    }
    fn OnSimpleVolumeChanged(&self, _: f32, _: BOOL, _: *const GUID) -> windows_core::Result<()> {
        Ok(())
    }
    fn OnChannelVolumeChanged(&self, _: u32, _: *const f32, _: u32, _: *const GUID) -> windows_core::Result<()> {
        Ok(())
    }
    fn OnGroupingParamChanged(&self, _: *const GUID, _: *const GUID) -> windows_core::Result<()> {
        Ok(())
    }
    fn OnStateChanged(&self, state: AudioSessionState) -> windows_core::Result<()> {
        if state == AudioSessionStateActive {
            fire(&self.handler, Device::Microphone);
        }
        Ok(())
    }
    fn OnSessionDisconnected(&self, _: AudioSessionDisconnectReason) -> windows_core::Result<()> {
        Ok(())
    }
}

#[implement(IAudioSessionNotification)]
struct NewSessions {
    handler: Handler,
    sessions: Arc<Mutex<Vec<(IAudioSessionControl, IAudioSessionEvents)>>>,
}

impl IAudioSessionNotification_Impl for NewSessions_Impl {
    fn OnSessionCreated(&self, session: Ref<'_, IAudioSessionControl>) -> windows_core::Result<()> {
        if let Some(session) = session.as_ref() {
            follow(session, &self.handler, &self.sessions);
            if unsafe { session.GetState() }.is_ok_and(|s| s == AudioSessionStateActive) {
                fire(&self.handler, Device::Microphone);
            }
        }
        Ok(())
    }
}

/// Subscribes to a session's state changes (so a reused session counts when it starts again).
fn follow(session: &IAudioSessionControl, handler: &Handler, sessions: &Mutex<Vec<(IAudioSessionControl, IAudioSessionEvents)>>) {
    let events: IAudioSessionEvents = SessionEvents { handler: handler.clone() }.into();
    if unsafe { session.RegisterAudioSessionNotification(&events) }.is_ok()
        && let Ok(mut list) = sessions.lock()
    {
        list.push((session.clone(), events));
    }
}

fn watch_microphones(
    handler: &Handler,
    sessions: &Arc<Mutex<Vec<(IAudioSessionControl, IAudioSessionEvents)>>>,
) -> windows_core::Result<Vec<windows_core::IUnknown>> {
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER)? };
    let devices = unsafe { enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)? };
    let mut keep = vec![windows_core::Interface::cast(&enumerator)?];
    for i in 0..unsafe { devices.GetCount()? } {
        let Ok(device) = (unsafe { devices.Item(i) }) else { continue };
        let Ok(manager) = (unsafe { device.Activate::<IAudioSessionManager2>(CLSCTX_INPROC_SERVER, None) }) else {
            continue;
        };
        let notify: IAudioSessionNotification =
            NewSessions { handler: handler.clone(), sessions: sessions.clone() }.into();
        if unsafe { manager.RegisterSessionNotification(&notify) }.is_err() {
            continue;
        }
        // Listing the sessions once is what turns the notifications on; follow the existing ones.
        if let Ok(list) = unsafe { manager.GetSessionEnumerator() } {
            for j in 0..unsafe { list.GetCount() }.unwrap_or(0) {
                if let Ok(session) = unsafe { list.GetSession(j) } {
                    follow(&session, handler, sessions);
                }
            }
        }
        keep.push(windows_core::Interface::cast(&manager)?);
        keep.push(windows_core::Interface::cast(&notify)?);
    }
    crate::log::info(&format!("Watching microphone use on {} device(s)", (keep.len() - 1) / 2));
    Ok(keep)
}

// ---- Camera ----------------------------------------------------------------------------------

#[implement(IMFSensorActivitiesReportCallback)]
struct CameraActivity {
    handler: Handler,
    streaming: Mutex<HashSet<u32>>,
}

impl IMFSensorActivitiesReportCallback_Impl for CameraActivity_Impl {
    fn OnActivitiesReport(&self, report: Ref<'_, IMFSensorActivitiesReport>) -> windows_core::Result<()> {
        let Some(report) = report.as_ref() else { return Ok(()) };
        let mut now = HashSet::new();
        for i in 0..unsafe { report.GetCount() }.unwrap_or(0) {
            let Ok(device) = (unsafe { report.GetActivityReport(i) }) else { continue };
            for j in 0..unsafe { device.GetProcessCount() }.unwrap_or(0) {
                let Ok(process) = (unsafe { device.GetProcessActivity(j) }) else { continue };
                if unsafe { process.GetStreamingState() }.is_ok_and(|s| s.as_bool())
                    && let Ok(pid) = unsafe { process.GetProcessId() }
                {
                    now.insert(pid);
                }
            }
        }
        let started = match self.streaming.lock() {
            Ok(mut previous) => {
                let started = now.difference(&previous).next().is_some();
                *previous = now;
                started
            }
            Err(_) => false,
        };
        if started {
            fire(&self.handler, Device::Camera);
        }
        Ok(())
    }
}

fn watch_cameras(handler: &Handler) -> windows_core::Result<windows_core::IUnknown> {
    unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE)? };
    let callback: IMFSensorActivitiesReportCallback =
        CameraActivity { handler: handler.clone(), streaming: Mutex::new(HashSet::new()) }.into();
    let monitor = unsafe { MFCreateSensorActivityMonitor(&callback)? };
    unsafe { monitor.Start()? };
    windows_core::Interface::cast(&monitor)
}
