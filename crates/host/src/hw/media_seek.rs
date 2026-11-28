use super::com::ComGuard;
use crate::osd::{self, Icon};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession, GlobalSystemMediaTransportControlsSessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus,
};
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation8, IUIAutomation2, IUIAutomationRangeValuePattern, TreeScope_Descendants,
    UIA_ControlTypePropertyId, UIA_RangeValuePatternId, UIA_SliderControlTypeId,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
use windows_core::PWSTR;

const TICKS_PER_SECOND: f64 = 10_000_000.;
const PREVIEW_INTERVAL: Duration = Duration::from_millis(33);
const INACTIVITY: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Dragging,
    Released,
    Cancelled,
}

struct ScrubState {
    delta: i32,
    changed: Instant,
    phase: Phase,
}

#[derive(Debug, PartialEq)]
enum NextFrame {
    Preview(i32),
    Seek(i32),
    Wait(Duration),
    Stop,
}

impl ScrubState {
    fn next_frame(&self, now: Instant, preview: Option<(i32, Instant)>) -> NextFrame {
        let idle = now.duration_since(self.changed);
        if idle >= INACTIVITY || self.phase == Phase::Cancelled {
            return NextFrame::Stop;
        }
        if self.phase == Phase::Released {
            return if self.delta == 0 {
                NextFrame::Stop
            } else {
                NextFrame::Seek(self.delta)
            };
        }
        if let Some((previous, at)) = preview {
            if previous == self.delta {
                return NextFrame::Wait(INACTIVITY - idle);
            }
            let elapsed = now.duration_since(at);
            if elapsed < PREVIEW_INTERVAL {
                return NextFrame::Wait(PREVIEW_INTERVAL - elapsed);
            }
        }
        NextFrame::Preview(self.delta)
    }
}

struct Scrub {
    window: isize,
    state: Mutex<ScrubState>,
    input: Condvar,
}

static ACTIVE: Mutex<Option<Arc<Scrub>>> = Mutex::new(None);
static WORKING: AtomicBool = AtomicBool::new(false);

pub fn update(delta: i32) {
    let Ok(mut active) = ACTIVE.lock() else {
        return;
    };
    if let Some(scrub) = &*active {
        if let Ok(mut state) = scrub.state.lock()
            && state.phase == Phase::Dragging
        {
            state.changed = Instant::now();
            if state.delta != delta {
                state.delta = delta;
                scrub.input.notify_one();
            }
        }
        return;
    }
    let window = unsafe { GetForegroundWindow() };
    if window.is_invalid() {
        return;
    }
    if WORKING.swap(true, Ordering::AcqRel) {
        return;
    }
    let scrub = Arc::new(Scrub {
        window: window.0 as isize,
        state: Mutex::new(ScrubState {
            delta,
            changed: Instant::now(),
            phase: Phase::Dragging,
        }),
        input: Condvar::new(),
    });
    let worker = scrub.clone();
    if std::thread::Builder::new()
        .name("media-scrub".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            track(&worker);
            if let Ok(mut active) = ACTIVE.lock() {
                WORKING.store(false, Ordering::Release);
                // Keep a failed lookup from starting another worker on every movement.
                if worker
                    .state
                    .lock()
                    .is_ok_and(|state| state.phase != Phase::Dragging)
                {
                    active.take();
                }
            }
        })
        .is_ok()
    {
        *active = Some(scrub);
    } else {
        WORKING.store(false, Ordering::Release);
    }
}

pub fn finish() {
    stop(Phase::Released);
}

pub fn cancel() {
    stop(Phase::Cancelled);
}

fn stop(phase: Phase) {
    let Ok(mut active) = ACTIVE.lock() else {
        return;
    };
    if !WORKING.load(Ordering::Acquire) {
        active.take();
        return;
    }
    if let Some(scrub) = active.as_ref()
        && let Ok(mut state) = scrub.state.lock()
        && state.phase != Phase::Cancelled
    {
        state.phase = phase;
        scrub.input.notify_one();
    }
}

fn track(scrub: &Scrub) {
    let _com = ComGuard::mta();
    let window = HWND(scrub.window as *mut _);
    let Some(target) = Target::find(window) else {
        return;
    };
    let mut preview = None;
    loop {
        let Ok(state) = scrub.state.lock() else {
            return;
        };
        if unsafe { GetForegroundWindow() } != window {
            return;
        }
        match state.next_frame(Instant::now(), preview) {
            NextFrame::Preview(delta) => {
                drop(state);
                target.preview(delta);
                preview = Some((delta, Instant::now()));
            }
            NextFrame::Seek(delta) => {
                drop(state);
                commit(scrub, window, &target, delta);
                return;
            }
            NextFrame::Wait(wait) => {
                if scrub.input.wait_timeout(state, wait).is_err() {
                    return;
                }
            }
            NextFrame::Stop => return,
        }
    }
}

fn commit(scrub: &Scrub, window: HWND, target: &Target, delta: i32) {
    let still_valid = || {
        let foreground = unsafe { GetForegroundWindow() };
        foreground == window
            && scrub
                .state
                .lock()
                .is_ok_and(|state| state.phase == Phase::Released)
    };
    if !still_valid() {
        return;
    }
    match target.seek(delta) {
        Some(true) => target.preview(delta),
        Some(false) if matches!(target, Target::Session { .. }) && still_valid() => {
            if let Some(fallback) = Target::accessible_timeline(window)
                && still_valid()
                && fallback.seek(delta) == Some(true)
            {
                fallback.preview(delta);
            }
        }
        _ => {}
    }
}

enum Target {
    Session {
        session: GlobalSystemMediaTransportControlsSession,
        range: Range,
    },
    Slider {
        slider: IUIAutomationRangeValuePattern,
        range: Range,
    },
}

#[derive(Clone, Copy)]
struct Range {
    start: f64,
    end: f64,
    origin: f64,
}

impl Range {
    fn new(start: f64, end: f64, origin: f64) -> Option<Self> {
        (start.is_finite() && end.is_finite() && origin.is_finite() && end > start).then(|| Self {
            start,
            end,
            origin: origin.clamp(start, end),
        })
    }

    fn position(self, delta: i32, maximum_span: f64) -> f64 {
        let width = ((self.end - self.start) * 0.5).min(maximum_span);
        (self.origin + f64::from(delta) / 1000. * width).clamp(self.start, self.end)
    }

    fn fraction(self, value: f64) -> f32 {
        ((value - self.start) / (self.end - self.start)) as f32
    }
}

impl Target {
    fn find(window: HWND) -> Option<Self> {
        // Match the foreground program before seeking a system media session. Spotify playing
        // in the background must not take over a gesture in a different window.
        let source = foreground_executable(window);
        source
            .as_deref()
            .and_then(Self::system_session)
            .or_else(|| Self::accessible_timeline(window))
    }

    fn system_session(executable: &str) -> Option<Self> {
        let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync().ok()?;
        if !wait(|| manager.Status().is_ok_and(|status| status.0 == 0)) {
            let _ = manager.Cancel();
            return None;
        }
        let session = manager.GetResults().ok()?.GetCurrentSession().ok()?;
        if !source_matches(
            executable,
            &session.SourceAppUserModelId().ok()?.to_string(),
        ) {
            return None;
        }
        let playback = session.GetPlaybackInfo().ok()?;
        if !playback.Controls().ok()?.IsPlaybackPositionEnabled().ok()? {
            return None;
        }
        let timeline = session.GetTimelineProperties().ok()?;
        let start = timeline.MinSeekTime().ok()?.Duration as f64;
        let end = timeline.MaxSeekTime().ok()?.Duration as f64;
        let mut position = timeline.Position().ok()?.Duration as f64;
        if playback.PlaybackStatus().ok()?
            == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing
            && let Ok(updated) = timeline.LastUpdatedTime()
            && updated.UniversalTime > 0
        {
            const WINDOWS_EPOCH: i64 = 116_444_736_000_000_000;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_nanos()
                / 100;
            let elapsed = (WINDOWS_EPOCH + now as i64).saturating_sub(updated.UniversalTime);
            let rate = playback
                .PlaybackRate()
                .and_then(|value| value.Value())
                .unwrap_or(1.);
            position += elapsed.max(0) as f64 * rate;
        }
        Some(Self::Session {
            session,
            range: Range::new(start, end, position)?,
        })
    }

    fn accessible_timeline(window: HWND) -> Option<Self> {
        unsafe {
            let ui: IUIAutomation2 =
                CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER).ok()?;
            ui.SetConnectionTimeout(200).ok()?;
            ui.SetTransactionTimeout(300).ok()?;
            let root = ui.ElementFromHandle(window).ok()?;
            let condition = ui
                .CreatePropertyCondition(
                    UIA_ControlTypePropertyId,
                    &UIA_SliderControlTypeId.0.into(),
                )
                .ok()?;
            let sliders = root.FindAll(TreeScope_Descendants, &condition).ok()?;
            let mut found = None;
            for index in 0..sliders.Length().ok()?.min(24) {
                let Ok(element) = sliders.GetElement(index) else {
                    continue;
                };
                if !element
                    .CurrentIsEnabled()
                    .is_ok_and(|enabled| enabled.as_bool())
                {
                    continue;
                }
                let name = element
                    .CurrentName()
                    .unwrap_or_default()
                    .to_string()
                    .to_lowercase();
                let class = element.CurrentClassName().unwrap_or_default().to_string();
                if !is_timeline(&name, &class) {
                    continue;
                }
                let Ok(slider) = element
                    .GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(UIA_RangeValuePatternId)
                else {
                    continue;
                };
                if !slider
                    .CurrentIsReadOnly()
                    .is_ok_and(|readonly| !readonly.as_bool())
                {
                    continue;
                }
                let range = Range::new(
                    slider.CurrentMinimum().ok()?,
                    slider.CurrentMaximum().ok()?,
                    slider.CurrentValue().ok()?,
                )?;
                if found.is_some() {
                    return None;
                }
                found = Some(Self::Slider { slider, range });
            }
            found
        }
    }

    fn position(&self, delta: i32) -> (Range, f64) {
        let (range, limit) = match self {
            Self::Session { range, .. } => (*range, 120. * TICKS_PER_SECOND),
            Self::Slider { range, .. } => (*range, f64::INFINITY),
        };
        (range, range.position(delta, limit))
    }

    fn preview(&self, delta: i32) {
        let (range, position) = self.position(delta);
        let fraction = range.fraction(position);
        let caption = match self {
            Self::Session { .. } => format!(
                "{} / {}",
                clock(position - range.start),
                clock(range.end - range.start)
            ),
            Self::Slider { .. } => format!("{:.0}%", 100. * fraction),
        };
        osd::show_progress(Icon::Display, &caption, fraction);
    }

    // A timeout leaves the outcome unknown. Retrying could seek the same video twice.
    fn seek(&self, delta: i32) -> Option<bool> {
        let (range, position) = self.position(delta);
        if position == range.origin {
            return Some(true);
        }
        match self {
            Self::Session { session, .. } => {
                let action = session
                    .TryChangePlaybackPositionAsync(position.round() as i64)
                    .ok()?;
                if !wait(|| action.Status().is_ok_and(|status| status.0 == 0)) {
                    let _ = action.Cancel();
                    return None;
                }
                action.GetResults().ok()
            }
            Self::Slider { slider, .. } => Some(unsafe { slider.SetValue(position) }.is_ok()),
        }
    }
}

fn foreground_executable(window: HWND) -> Option<String> {
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(window, Some(&mut pid));
    }
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = [0u16; 1024];
    let mut length = buffer.len() as u32;
    let result = unsafe {
        let result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        );
        let _ = CloseHandle(process);
        result
    };
    result.ok()?;
    let path = String::from_utf16_lossy(&buffer[..length as usize]);
    path.rsplit(['\\', '/']).next().map(str::to_ascii_lowercase)
}

fn source_matches(executable: &str, source: &str) -> bool {
    let source = source.trim().to_ascii_lowercase();
    let basename = source.rsplit(['\\', '/']).next().unwrap_or_default();
    if basename == executable {
        return true;
    }
    match executable {
        "msedge.exe" => {
            source == "msedge"
                || source.starts_with("microsoft.microsoftedge.")
                || source.starts_with("msedge.")
        }
        "chrome.exe" => source == "chrome" || source.starts_with("chrome."),
        "firefox.exe" => source == "firefox" || source.starts_with("mozilla.firefox"),
        "spotify.exe" => source == "spotify" || source.starts_with("spotifyab.spotifymusic_"),
        _ => executable
            .strip_suffix(".exe")
            .is_some_and(|stem| stem == source),
    }
}

fn is_timeline(name: &str, class: &str) -> bool {
    class
        .split_ascii_whitespace()
        .any(|token| token == "ytp-progress-bar")
        || matches!(
            name.trim(),
            "seek slider"
                | "playback position"
                | "seek bar"
                | "video progress"
                | "позиция воспроизведения"
                | "ползунок перемотки"
                | "положение воспроизведения"
                | "播放进度"
                | "进度滑块"
        )
}

fn clock(ticks: f64) -> String {
    let seconds = (ticks.max(0.) / TICKS_PER_SECOND) as u64;
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

fn wait(mut pending: impl FnMut() -> bool) -> bool {
    let limit = Instant::now() + Duration::from_millis(350);
    while pending() {
        if Instant::now() >= limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(8));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displacement_is_relative_to_the_start_of_the_gesture_and_clamped() {
        let range = Range::new(10., 110., 20.).unwrap();
        assert_eq!(range.position(500, 100.), 45.);
        assert_eq!(range.position(-500, 100.), 10.);
        assert_eq!(range.position(0, 100.), 20.);
        assert_eq!(range.fraction(45.), 0.35);
        assert!(Range::new(10., 10., 10.).is_none());
    }

    #[test]
    fn moving_previews_and_release_seeks_to_the_latest_position() {
        let start = Instant::now();
        let mut state = ScrubState {
            delta: 51,
            changed: start,
            phase: Phase::Dragging,
        };
        assert_eq!(state.next_frame(start, None), NextFrame::Preview(51));

        let preview = Some((51, start));
        state.delta = -47;
        let moved = start + Duration::from_millis(9);
        state.changed = moved;
        assert_eq!(
            state.next_frame(moved, preview),
            NextFrame::Wait(PREVIEW_INTERVAL - Duration::from_millis(9))
        );
        assert_eq!(
            state.next_frame(start + PREVIEW_INTERVAL, preview),
            NextFrame::Preview(-47)
        );

        state.phase = Phase::Released;
        assert_eq!(state.next_frame(moved, preview), NextFrame::Seek(-47));
        state.delta = 0;
        assert_eq!(state.next_frame(moved, preview), NextFrame::Stop);
        state.delta = -47;
        state.phase = Phase::Cancelled;
        assert_eq!(state.next_frame(moved, preview), NextFrame::Stop);
    }

    #[test]
    fn foreground_sources_and_timeline_labels_are_specific() {
        assert!(source_matches("msedge.exe", "MSEdge"));
        assert!(source_matches("chrome.exe", "Chrome._crx_test"));
        assert!(!source_matches(
            "chrome.exe",
            "SpotifyAB.SpotifyMusic_foo!Spotify"
        ));
        assert!(is_timeline("video progress", "slider"));
        assert!(!is_timeline("volume", "slider"));
    }
}
