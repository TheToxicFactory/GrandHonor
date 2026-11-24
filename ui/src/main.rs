#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use gpui::{
    Animation, AnimationExt, AnyElement, App, Application, Bounds, Context, Div, ElementId,
    FocusHandle, Font, FontFallbacks, FontFeatures, FontStyle, FontWeight, ImageSource,
    IntoElement, KeyDownEvent, ObjectFit, RenderImage, SharedString, Stateful, Styled, TitlebarOptions, Window, WindowAppearance,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, linear_color_stop,
    img, linear_gradient, point, prelude::*, px, relative, rgb, rgba, size,
};
use grandhonor_core::locale::{AppLanguage, set_language_override, t};
use grandhonor_core::protocol::{
    ControlItem, PANEL_TELEMETRY_COMMAND, PANEL_TELEMETRY_INTERVAL_MS, PanelState,
    TelemetrySummary, now_dotnet_ticks,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

mod glass;

fn window_hwnd(window: &Window) -> Option<isize> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(raw) => Some(raw.hwnd.get()),
        _ => None,
    }
}
use serde::de::DeserializeOwned;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::mem::size_of;
use std::sync::Arc;
use std::time::Duration;

const FLYOUT_WIDTH: f32 = 464.0;
const FLYOUT_HEIGHT: f32 = 624.0;
const EDGE_MARGIN: f32 = 12.0;

/// Holds a reading steady: a new value is shown only when it differs by at least `step` or
/// has persisted for `PERSIST` samples (1.5 s at 0.5 s), so sensor noise of ±1 does not make the
/// figure dance. A missing reading clears it at once: no stale value is ever kept on screen.
#[derive(Default, Clone, Copy)]
struct Hold {
    shown: Option<f64>,
    candidate: Option<f64>,
    seen: u8,
}

const PERSIST: u8 = 3;

impl Hold {
    fn update(&mut self, value: Option<f64>, step: f64) -> Option<f64> {
        let Some(v) = value else {
            *self = Self::default();
            return None;
        };
        match self.shown {
            None => self.shown = Some(v),
            Some(shown) if (v - shown).abs() >= step => {
                self.shown = Some(v);
                self.candidate = None;
                self.seen = 0;
            }
            Some(shown) if (v - shown).abs() < f64::EPSILON => {
                self.candidate = None;
                self.seen = 0;
            }
            Some(_) => {
                if self.candidate.is_some_and(|c| (c - v).abs() < step / 2.0) {
                    self.seen += 1;
                } else {
                    self.candidate = Some(v);
                    self.seen = 1;
                }
                if self.seen >= PERSIST {
                    self.shown = self.candidate;
                    self.candidate = None;
                    self.seen = 0;
                }
            }
        }
        self.shown
    }
}

/// Exponential smoothing for watts, then a Hold on the smoothed value.
#[derive(Default, Clone, Copy)]
struct Smooth {
    average: Option<f64>,
    hold: Hold,
}

impl Smooth {
    fn update(&mut self, value: Option<f64>, step: f64) -> Option<f64> {
        let Some(v) = value else {
            *self = Self::default();
            return None;
        };
        let avg = match self.average {
            // A jump bigger than 3 W (load starts or stops) is taken at once.
            Some(a) if (v - a).abs() < 3.0 => a + (v - a) * 0.35,
            _ => v,
        };
        self.average = Some(avg);
        self.hold.update(Some((avg * 10.0).round() / 10.0), step)
    }
}

/// Steadies the Power tab readings between samples.
#[derive(Default)]
struct Stabilizer {
    cpu_temp: Hold,
    battery_temp: Hold,
    fan1: Hold,
    fan2: Hold,
    cpu_watts: Smooth,
    power_watts: Smooth,
}

impl Stabilizer {
    fn apply(&mut self, mut t: TelemetrySummary) -> TelemetrySummary {
        let int = |v: Option<f64>| v.map(|x| x.round() as i32);
        t.cpu_temp = int(self.cpu_temp.update(t.cpu_temp.map(f64::from), 2.0));
        t.battery_temp = int(self.battery_temp.update(t.battery_temp.map(f64::from), 2.0));
        // Fans: steps of 10 rpm, hold within 100 rpm; 0 (stopped) is shown at once.
        let fan = |hold: &mut Hold, v: Option<i32>| match v {
            Some(0) => {
                *hold = Hold::default();
                hold.update(Some(0.0), 100.0).map(|x| x as i32)
            }
            other => int(hold.update(other.map(|r| (f64::from(r) / 10.0).round() * 10.0), 100.0)),
        };
        t.fan1_rpm = fan(&mut self.fan1, t.fan1_rpm);
        t.fan2_rpm = fan(&mut self.fan2, t.fan2_rpm);
        t.cpu_watts = self.cpu_watts.update(t.cpu_watts, 0.5);
        t.power_watts = self.power_watts.update(t.power_watts, 0.5);
        t
    }
}

struct Panel {
    snapshot: Option<PanelState>,
    path: Vec<String>,
    busy: bool,
    polling: bool,
    pending_command: Option<Pending>,
    /// Bumped by every request; the slow-host timer compares it.
    request_seq: u64,
    /// No answer within the time limit: a banner says the host is slow.
    host_slow: bool,
    /// The panel lost focus while the host showed a dialog: take it back afterwards.
    refocus_after_busy: bool,
    /// The command whose control shows the busy state (only that one dims).
    busy_command: Option<u32>,
    disconnected: bool,
    was_active: bool,
    focus: FocusHandle,
    /// Switches and choices that changed with the last snapshot (they animate once).
    motion: Motion,
    generation: u64,
    /// Tab the selection pill slides from, and a counter that restarts that slide.
    tab_from: Option<usize>,
    tab_generation: u64,
    /// Frosted copy of the desktop behind the panel (Glass), see `glass.rs`.
    glass: Option<Arc<RenderImage>>,
    glass_tried: bool,
    stable: Stabilizer,
    /// The look (and Glass edge colour) whose window frame colour is applied.
    framed_look: Option<(Look, Option<u32>)>,
    /// Average colour (0xRRGGBB) along the edge of the frosted backdrop.
    glass_edge: Option<u32>,
    /// Position (0–1) of the slider knob while it is dragged, and the stop last sent.
    slider_drag: Option<f32>,
    slider_sent: Option<u32>,
    /// True while the pointer holds the knob; afterwards `slider_drag` keeps the released
    /// stop until the host's next snapshot shows it, so the knob does not jump back.
    slider_active: bool,
    /// Track bounds from the last paint and the stop commands of the slider being dragged.
    slider_bounds: std::rc::Rc<std::cell::Cell<Option<Bounds<gpui::Pixels>>>>,
    /// Stop commands of the slider in the latest snapshot (filled while rendering).
    slider_commands: std::rc::Rc<std::cell::RefCell<Vec<Option<u32>>>>,
    /// Scroll position of every page, so "Back" returns to where you were.
    scroll: HashMap<String, gpui::ScrollHandle>,
}

/// A command waiting for the host to be free.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Pending {
    /// A control's command from the snapshot of this generation (ids change between snapshots,
    /// so it is dropped when a newer snapshot arrived first).
    Command { id: u32, generation: u64 },
    /// A slider stop, resolved against the newest snapshot when it is sent.
    Stop(usize),
}

enum Reply {
    Panel(PanelState),
    Telemetry(TelemetrySummary),
}

impl Panel {
    fn start_telemetry(&mut self, cx: &mut Context<Self>) {
        let timer = cx.background_executor().clone();
        cx.spawn(async move |panel, cx| {
            loop {
                timer
                    .timer(Duration::from_millis(PANEL_TELEMETRY_INTERVAL_MS))
                    .await;
                let keep_polling = panel.update(cx, |panel, cx| {
                    if panel.path.len() == 1
                        && panel.selected().and_then(|item| item.kind.as_deref()) == Some("battery")
                    {
                        panel.request(PANEL_TELEMETRY_COMMAND, cx);
                        if let Some(telemetry) = panel.snapshot.as_ref().map(|state| &state.telemetry)
                            && telemetry.sampled_ticks.is_some_and(|time| !recent_sample(time))
                        {
                            cx.notify();
                        }
                    }
                    !panel.disconnected
                });
                if !keep_polling.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
    }

    /// The pointer is at `x` while it holds the slider: move the knob there and apply the stop
    /// under it when that stop changes.
    fn slider_move(&mut self, x: gpui::Pixels, cx: &mut Context<Self>) {
        let Some(bounds) = self.slider_bounds.get() else { return };
        let fraction = slider_fraction(x, bounds);
        if self.slider_drag != Some(fraction) {
            self.slider_drag = Some(fraction);
            self.slider_apply(fraction, cx);
            cx.notify();
        }
    }

    fn slider_apply(&mut self, fraction: f32, cx: &mut Context<Self>) {
        let count = self.slider_commands.borrow().len();
        let stop = slider_stop(fraction, count);
        if self.slider_sent != Some(stop as u32) {
            self.slider_sent = Some(stop as u32);
            self.request_stop(stop, cx);
        }
    }

    /// Sends a slider stop now, or queues it (newest wins) while the host is busy; the stop is
    /// looked up in the newest snapshot when it is sent.
    fn request_stop(&mut self, stop: usize, cx: &mut Context<Self>) {
        if self.busy || self.polling {
            self.pending_command = Some(Pending::Stop(stop));
        } else {
            let cmd = self.slider_commands.borrow().get(stop).copied().flatten();
            if let Some(cmd) = cmd {
                self.request(cmd, cx);
            }
        }
    }

    /// The pointer let go: the knob settles on the nearest stop and keeps it until the host's
    /// next snapshot shows that stop (no jump back).
    fn slider_finish(&mut self, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.slider_active) {
            return;
        }
        if let Some(fraction) = self.slider_drag {
            let count = self.slider_commands.borrow().len().max(2);
            self.slider_drag = Some(slider_stop(fraction, count) as f32 / (count - 1) as f32);
            self.slider_apply(fraction, cx);
        }
        self.slider_sent = None;
        if !self.busy && !self.polling && self.pending_command.is_none() {
            self.slider_drag = None;
        }
        cx.notify();
    }

    fn request(&mut self, command: u32, cx: &mut Context<Self>) {
        if self.busy || self.disconnected {
            return;
        }
        let telemetry_only = command == PANEL_TELEMETRY_COMMAND;
        if self.polling {
            if !telemetry_only {
                self.pending_command.get_or_insert(Pending::Command { id: command, generation: self.generation });
            }
            return;
        }
        self.polling = telemetry_only;
        self.busy = !telemetry_only;
        if self.busy {
            self.busy_command = Some(command);
            // Quick commands never show a busy state; a slow one does after 250 ms.
            let timer = cx.background_executor().clone();
            cx.spawn(async move |panel, cx| {
                timer.timer(Duration::from_millis(250)).await;
                let _ = panel.update(cx, |panel, cx| {
                    if panel.busy {
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        // No answer in time: say so (the answer may still come).
        self.request_seq += 1;
        let seq = self.request_seq;
        let limit = if telemetry_only { 4 } else { 10 };
        let timer = cx.background_executor().clone();
        cx.spawn(async move |panel, cx| {
            timer.timer(Duration::from_secs(limit)).await;
            let _ = panel.update(cx, |panel, cx| {
                if panel.request_seq == seq && (panel.busy || panel.polling) {
                    panel.host_slow = true;
                    cx.notify();
                }
            });
        })
        .detach();

        let req = cx.background_executor().spawn(async move {
            if telemetry_only {
                exchange(command).map(Reply::Telemetry)
            } else {
                exchange(command).map(Reply::Panel)
            }
        });
        cx.spawn(async move |panel, cx| {
            let result = req.await;
            let _ = panel.update(cx, |panel, cx| {
                panel.busy = false;
                panel.polling = false;
                panel.host_slow = false;
                match result {
                    Ok(Reply::Panel(snapshot)) => {
                        if !panel.slider_active && panel.pending_command.is_none() {
                            panel.slider_drag = None;
                        }
                        // A language switch renames every tab: keep the open one by its kind.
                        let open_kind = panel.tab_kind();
                        set_language_override(
                            snapshot.language.as_deref().and_then(AppLanguage::from_code),
                        );
                        panel.motion = panel
                            .snapshot
                            .as_ref()
                            .map(|old| Motion::between(&old.items, &snapshot.items))
                            .unwrap_or_default();
                        panel.generation += 1;
                        let mut snapshot = snapshot;
                        match panel.snapshot.as_ref().map(|old| old.telemetry.clone()) {
                            Some(old) if old.sampled_ticks >= snapshot.telemetry.sampled_ticks => {
                                snapshot.telemetry = TelemetrySummary {
                                    // Settings shown in the grid (limit, profile) follow the snapshot.
                                    charge_label: snapshot.telemetry.charge_label.clone(),
                                    mode_label: snapshot.telemetry.mode_label.clone(),
                                    performance_active: snapshot.telemetry.performance_active,
                                    hunter_active: snapshot.telemetry.hunter_active,
                                    ..old
                                };
                            }
                            _ => snapshot.telemetry = panel.stable.apply(snapshot.telemetry),
                        }
                        let job = snapshot.job_running;
                        panel.snapshot = Some(snapshot);
                        if job {
                            let timer = cx.background_executor().clone();
                            cx.spawn(async move |panel, cx| {
                                timer.timer(Duration::from_secs(1)).await;
                                let _ = panel.update(cx, |panel, cx| {
                                    if !panel.busy && !panel.polling {
                                        panel.request(0, cx);
                                    }
                                });
                            })
                            .detach();
                        }
                        let renamed = panel.selected().is_none();
                        while !panel.path.is_empty() && panel.selected().is_none() {
                            panel.path.pop();
                        }
                        if renamed
                            && panel.path.is_empty()
                            && let Some(tab) = panel.snapshot.as_ref().and_then(|s| {
                                s.items.iter().find(|item| item.kind == open_kind && open_kind.is_some())
                            })
                        {
                            panel.path.push(tab.text.clone());
                        }
                        if panel.path.is_empty()
                            && let Some(first) = panel.snapshot.as_ref().and_then(|s| {
                                // GRANDHONOR_OPEN_TAB=<kind> opens a specific tab first
                                // (battery, keyboard, touchpad, drivers, settings).
                                let wanted = std::env::var("GRANDHONOR_OPEN_TAB").ok();
                                s.items
                                    .iter()
                                    .find(|item| item.kind.is_some() && item.kind == wanted)
                                    .or_else(|| s.items.first())
                            })
                        {
                            panel.path.push(first.text.clone());
                        }
                    }
                    Ok(Reply::Telemetry(telemetry)) => {
                        if let Some(snapshot) = &mut panel.snapshot {
                            // An answer older than what is shown (a slow WMI call) is dropped,
                            // but a command waiting behind it still runs.
                            let mut unchanged = true;
                            if telemetry.sampled_ticks >= snapshot.telemetry.sampled_ticks {
                                // Redraw only when a steadied reading changed.
                                let telemetry = panel.stable.apply(telemetry);
                                let mut old = snapshot.telemetry.clone();
                                let mut new = telemetry.clone();
                                old.sampled_ticks = None;
                                new.sampled_ticks = None;
                                unchanged = old == new;
                                snapshot.telemetry = telemetry;
                            }
                            if unchanged && panel.pending_command.is_none() {
                                return;
                            }
                        }
                    }
                    Err(_) => panel.disconnected = true,
                }
                match panel.pending_command.take() {
                    Some(Pending::Command { id, generation }) if generation == panel.generation => panel.request(id, cx),
                    Some(Pending::Command { .. }) => {} // the page changed under it
                    Some(Pending::Stop(stop)) => panel.request_stop(stop, cx),
                    None => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn tab_kind(&self) -> Option<String> {
        let name = self.path.first()?;
        let items = &self.snapshot.as_ref()?.items;
        items.iter().find(|item| &item.text == name)?.kind.clone()
    }

    fn selected(&self) -> Option<&ControlItem> {
        let mut items = self.snapshot.as_ref()?.items.as_slice();
        let mut selected = None;
        for name in &self.path {
            let item = items.iter().find(|it| it.text == *name)?;
            selected = Some(item);
            items = item.children.as_deref().unwrap_or_default();
        }
        selected
    }
}

fn exchange<T: DeserializeOwned>(command: u32) -> io::Result<T> {
    let mut output = io::stdout().lock();
    writeln!(output, "{command}")?;
    output.flush()?;
    drop(output);

    let mut response = String::new();
    io::stdin().read_line(&mut response)?;
    serde_json::from_str(&response).map_err(io::Error::other)
}

// ---- Design system ---------------------------------------------------------------------------
//
// Two looks share one structure (System → Style): Graphite is opaque OLED black with hairline
// separators; Glass lets the Windows acrylic backdrop through translucent, rim-lit surfaces.
// Type is Inter (text) and Inter Display with tabular figures (readings), embedded in the panel.

const TEXT_FONT: &str = "Inter";
const DISPLAY_FONT: &str = "Inter Display";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    Graphite,
    Glass,
}

impl Look {
    fn from_code(code: Option<&str>) -> Self {
        match code {
            Some("glass") => Self::Glass,
            _ => Self::Graphite,
        }
    }
}

/// Every colour is 0xRRGGBBAA.
#[derive(Clone, Copy)]
struct Palette {
    glass: bool,
    dark: bool,
    /// Window fill. In Glass it is the tint laid over the frosted desktop copy.
    backdrop: u32,
    /// Glass only: opaque fill used until (or if) the desktop copy is unavailable.
    glass_base: u32,
    /// Grouped-list and sensor surfaces.
    group: u32,
    /// Glass rim; transparent in Graphite (no border, elevation comes from the surface tone).
    rim: u32,
    elevated: u32,
    hover: u32,
    separator: u32,
    text: u32,
    muted: u32,
    tertiary: u32,
    accent: u32,
    accent_text: u32,
    accent_soft: u32,
    focus: u32,
    track: u32,
    danger: u32,
    seg_track: u32,
    seg_selected: u32,
    tab_selected: u32,
    /// Corner radius of grouped surfaces: 14 px in Graphite, 20 px in Glass.
    radius: f32,
}

impl Palette {
    fn for_window(window: &Window, look: Look) -> Self {
        let dark = matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let accent = system_accent(dark);
        let base = Self {
            glass: false,
            dark,
            backdrop: 0,
            glass_base: if dark { 0x1c1c20ff } else { 0xeeeef3ff },
            group: 0,
            rim: 0,
            elevated: 0,
            hover: 0,
            separator: 0,
            text: 0,
            muted: 0,
            tertiary: 0,
            accent: (accent << 8) | 0xff,
            accent_text: (contrast_on(accent) << 8) | 0xff,
            accent_soft: (accent << 8) | if dark { 0x2e } else { 0x1f },
            focus: (accent << 8) | 0xc0,
            track: 0,
            danger: 0,
            seg_track: 0,
            seg_selected: 0,
            tab_selected: 0,
            radius: 14.0,
        };
        match (look, dark) {
            (Look::Graphite, true) => Self {
                backdrop: 0x000000ff,
                group: 0x141416ff,
                rim: 0x00000000,
                elevated: 0x262629ff,
                hover: 0xffffff0d,
                separator: 0xffffff14,
                text: 0xf5f5f7ff,
                muted: 0x8e8e93ff,
                tertiary: 0x636368ff,
                track: 0x39393dff,
                danger: 0xff6961ff,
                seg_track: 0x141416ff,
                seg_selected: 0x303034ff,
                tab_selected: 0x1c1c1fff,
                ..base
            },
            (Look::Graphite, false) => Self {
                backdrop: 0xf2f2f7ff,
                group: 0xffffffff,
                rim: 0x00000000,
                elevated: 0xf2f2f7ff,
                hover: 0x0000000a,
                separator: 0x3c3c432e,
                text: 0x1d1d1fff,
                muted: 0x6e6e73ff,
                tertiary: 0xa1a1a6ff,
                track: 0xe3e3e8ff,
                danger: 0xd70015ff,
                seg_track: 0x7676801f,
                seg_selected: 0xffffffff,
                tab_selected: 0xffffffff,
                ..base
            },
            (Look::Glass, true) => Self {
                glass: true,
                backdrop: 0x15151aa3,
                group: 0xffffff17,
                rim: 0xffffff2e,
                elevated: 0xffffff1f,
                hover: 0xffffff0f,
                separator: 0xffffff17,
                text: 0xf7f7f9ff,
                muted: 0xb4b4bbff,
                tertiary: 0x9696a0ff,
                track: 0xffffff29,
                danger: 0xff7a72ff,
                seg_track: 0xffffff10,
                seg_selected: 0xffffff2b,
                tab_selected: 0xffffff1c,
                radius: 20.0,
                ..base
            },
            (Look::Glass, false) => Self {
                glass: true,
                backdrop: 0xf7f7fab3,
                group: 0xffffffa8,
                rim: 0xffffffd9,
                elevated: 0xffffffcc,
                hover: 0x0000000a,
                separator: 0x3c3c4324,
                text: 0x1d1d1fff,
                muted: 0x55555bff,
                tertiary: 0x76767cff,
                track: 0x7878802e,
                danger: 0xd70015ff,
                seg_track: 0x7676801a,
                seg_selected: 0xffffffeb,
                tab_selected: 0xffffffb8,
                radius: 20.0,
                ..base
            },
        }
    }

    /// A grouped surface: tone fill in Graphite; translucent fill with a lit rim in Glass.
    fn surface<E: Styled>(&self, el: E) -> E {
        if self.glass {
            // A faint top-to-bottom sheen stands in for the specular edge of real glass.
            let top = (self.group & 0xffff_ff00) | ((self.group & 0xff) + if self.dark { 0x08 } else { 0x10 }).min(0xff);
            el.bg(linear_gradient(180., linear_color_stop(rgba(top), 0.), linear_color_stop(rgba(self.group), 1.)))
                .border_1()
                .border_color(rgba(self.rim))
        } else {
            el.bg(rgba(self.group))
        }
    }

    /// Segmented controls and tab pills: capsules in Glass, rounded rectangles in Graphite.
    fn pill_radius(&self, height: f32) -> f32 {
        if self.glass { height / 2.0 } else { (height * 0.3).round() }
    }
}

fn contrast_on(color: u32) -> u32 {
    let linear = |shift: u32| {
        let channel = ((color >> shift) & 255) as f32 / 255.0;
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * linear(16) + 0.7152 * linear(8) + 0.0722 * linear(0);
    if luminance > 0.179 {
        0x000000
    } else {
        0xffffff
    }
}

fn read_windows_registry_accent_rgb() -> Option<(u32, u32, u32)> {
    const HKEY_CURRENT_USER: isize = -2147483647;
    const KEY_READ: u32 = 0x20019;
    const REG_DWORD: u32 = 4;

    let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\DWM\0"
        .encode_utf16()
        .collect();
    let val_name: Vec<u16> = "AccentColor\0".encode_utf16().collect();
    let mut hkey: isize = 0;
    if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey) } != 0 {
        return None;
    }
    let mut ty = 0u32;
    let mut data = 0u32;
    let mut len = 4u32;
    let status = unsafe {
        RegQueryValueExW(
            hkey,
            val_name.as_ptr(),
            std::ptr::null(),
            &mut ty,
            &mut data as *mut u32 as *mut u8,
            &mut len,
        )
    };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    if status == 0 && ty == REG_DWORD {
        // HKCU\...\DWM\AccentColor is 0xAABBGGRR
        Some((data & 255, (data >> 8) & 255, (data >> 16) & 255))
    } else {
        None
    }
}

/// The Windows accent, read once per panel session (the panel lives only while it is open).
fn windows_accent_rgb() -> Option<(u32, u32, u32)> {
    static ACCENT: std::sync::OnceLock<Option<(u32, u32, u32)>> = std::sync::OnceLock::new();
    *ACCENT.get_or_init(|| {
        read_windows_registry_accent_rgb().or_else(|| {
            let mut color = 0u32;
            let mut opaque_blend = 0i32;
            if unsafe { DwmGetColorizationColor(&mut color, &mut opaque_blend) } < 0 {
                return None;
            }
            Some(((color >> 16) & 255, (color >> 8) & 255, color & 255))
        })
    })
}

fn system_accent(dark: bool) -> u32 {
    let blue = if dark { 0x0a84ff } else { 0x0071e3 };
    let Some((r, g, b)) = windows_accent_rgb() else {
        // No accent: Apple system blue.
        return blue;
    };
    // A grey or graphite accent would make an "on" switch look like an "off" one: below a
    // little colour (HSL saturation), use the system blue instead.
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let lightness = (max + min) as f32 / 510.0;
    let chroma = (max - min) as f32 / 255.0;
    let saturation = if chroma == 0.0 { 0.0 } else { chroma / (1.0 - (2.0 * lightness - 1.0).abs()).max(1e-3) };
    if saturation < 0.25 {
        return blue;
    }

    let tune = |ch: u32| -> u32 {
        if dark {
            (ch + (255 - ch) * 32 / 100).min(255)
        } else {
            ch * 65 / 100
        }
    };
    (tune(r) << 16) | (tune(g) << 8) | tune(b)
}

/// Inter Display with tabular figures, so changing readings never shift sideways.
fn numeric_font(weight: FontWeight) -> Font {
    Font {
        family: DISPLAY_FONT.into(),
        features: FontFeatures(Arc::new(vec![("tnum".into(), 1)])),
        fallbacks: None,
        weight,
        style: FontStyle::Normal,
    }
}

fn tabular() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}

fn glyph(code: &'static str, size_px: f32) -> Div {
    div()
        .font_family("Segoe Fluent Icons")
        .text_size(px(size_px))
        .child(code)
}

fn section_symbol(kind: Option<&str>) -> &'static str {
    match kind {
        Some("battery") => "\u{e945}",
        Some("keyboard") => "\u{e765}",
        Some("touchpad") => "\u{efa5}",
        Some("drivers" | "driver-update" | "driver" | "collection") => "\u{e896}",
        Some("settings") => "\u{e713}",
        Some("display") => "\u{e7f4}",
        Some("time") => "\u{e823}",
        Some("preset") => "\u{e8b1}",
        Some("update") => "\u{e777}",
        _ => "\u{e8b7}",
    }
}

/// Back-out easing with a small (≈4 %) overshoot: the settle of a light spring.
fn spring(t: f32) -> f32 {
    let x = t - 1.0;
    1.0 + 2.2 * x * x * x + 1.2 * x * x
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(4)
}

/// The GrandHonor mark: a graphite squircle with a lit top edge and a strict white H.
/// Same geometry as `Assets/mark.ps1` (the application icon).
fn badge_emblem(side: f32) -> Div {
    let stem_w = (side * 0.105).round().max(2.0);
    let stem_h = (side * 0.43).round();
    let bar_w = (side * 0.39).round();
    div()
        .relative()
        .flex_none()
        .w(px(side))
        .h(px(side))
        .rounded(px(side * 0.2237))
        .bg(linear_gradient(
            180.,
            linear_color_stop(rgb(0x3a3a3f), 0.),
            linear_color_stop(rgb(0x121214), 1.),
        ))
        .border_1()
        .border_color(rgba(0xffffff1f))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(bar_w - 2.0 * stem_w))
                .child(div().w(px(stem_w)).h(px(stem_h)).rounded(px(0.5)).bg(rgb(0xf5f5f7)))
                .child(div().w(px(stem_w)).h(px(stem_h)).rounded(px(0.5)).bg(rgb(0xf5f5f7))),
        )
        .child(div().absolute().w(px(bar_w)).h(px(stem_w)).bg(rgb(0xf5f5f7)))
}

fn control(
    id: impl Into<ElementId>,
    palette: Palette,
    action: impl Fn(&mut Window, &mut App) + 'static + Clone,
) -> Stateful<Div> {
    let kb_action = action.clone();
    div()
        .id(id)
        .cursor_pointer()
        .focusable()
        .tab_stop(true)
        .border_1()
        .border_color(rgba(0x00000000))
        .focus(move |el| el.border_color(rgba(palette.focus)))
        .on_click(move |_, window, cx| action(window, cx))
        .on_key_down(move |ev: &KeyDownEvent, window, cx| {
            if matches!(ev.keystroke.key.as_str(), "enter" | "space") {
                kb_action(window, cx);
                cx.stop_propagation();
            }
        })
}

/// What changed with the latest snapshot, so only real changes animate (not the first frame).
#[derive(Default)]
struct Motion {
    /// Keys (`/tab/item`) of switches and choices whose state flipped.
    flipped: HashSet<String>,
    /// Previous selected index of each segmented control, keyed by its option names.
    segment_from: HashMap<String, usize>,
}

fn item_key(prefix: &str, item: &ControlItem) -> String {
    format!("{prefix}/{}", item.text)
}

fn segment_key(container: &ControlItem) -> String {
    container
        .children
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|c| c.text.as_str())
        .collect::<Vec<_>>()
        .join("|")
}

fn collect_state(
    items: &[ControlItem],
    prefix: &str,
    flags: &mut HashMap<String, bool>,
    segments: &mut HashMap<String, usize>,
) {
    for item in items {
        let key = item_key(prefix, item);
        flags.insert(key.clone(), item.selected);
        if let Some(children) = &item.children {
            if item.kind.as_deref() == Some("segment")
                && let Some(ix) = children.iter().position(|c| c.selected)
            {
                segments.insert(segment_key(item), ix);
            }
            collect_state(children, &key, flags, segments);
        }
    }
}

impl Motion {
    fn between(old: &[ControlItem], new: &[ControlItem]) -> Self {
        let (mut old_flags, mut old_segments) = (HashMap::new(), HashMap::new());
        let (mut new_flags, mut new_segments) = (HashMap::new(), HashMap::new());
        collect_state(old, "", &mut old_flags, &mut old_segments);
        collect_state(new, "", &mut new_flags, &mut new_segments);
        let flipped = new_flags
            .iter()
            .filter(|(k, v)| old_flags.get(*k).is_some_and(|old| old != *v))
            .map(|(k, _)| k.clone())
            .collect();
        let segment_from = new_segments
            .iter()
            .filter_map(|(k, ix)| old_segments.get(k).filter(|old| *old != ix).map(|old| (k.clone(), *old)))
            .collect();
        Self { flipped, segment_from }
    }
}

/// Context every control renderer needs.
struct Ctx<'a> {
    palette: Palette,
    busy: bool,
    motion: &'a Motion,
    /// Knob position of a slider being dragged.
    slider_drag: Option<f32>,
    slider_bounds: std::rc::Rc<std::cell::Cell<Option<Bounds<gpui::Pixels>>>>,
    slider_commands: std::rc::Rc<std::cell::RefCell<Vec<Option<u32>>>>,
    /// The command being run (its control alone shows the busy state).
    busy_command: Option<u32>,
    /// Monotonic snapshot number: part of animated element ids, so a change restarts them.
    generation: u64,
}

impl Render for Panel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let look = Look::from_code(self.snapshot.as_ref().and_then(|s| s.appearance.as_deref()));
        if look == Look::Glass && !self.glass_tried {
            // Taken once, the first time Glass is shown (at open, or right after choosing it).
            self.glass_tried = true;
            self.glass = window_hwnd(window).and_then(glass::capture).map(|b| {
                self.glass_edge = Some(edge_colour(&b.bgra, b.width as usize, b.height as usize));
                let buffer = image::RgbaImage::from_raw(b.width, b.height, b.bgra);
                Arc::new(RenderImage::new(smallvec::SmallVec::from_elem(
                    image::Frame::new(buffer.unwrap_or_default()),
                    1,
                )))
            });
        }
        // Windows keeps a 1 px frame (2 px at 150 %) around the window. With no colour it is
        // black, so in Glass it takes the colour of the glass at the edge and disappears.
        let palette_now = Palette::for_window(window, look);
        let edge = (look == Look::Glass)
            .then(|| self.glass_edge.map(|c| blend_over(c, palette_now.backdrop)))
            .flatten();
        if self.framed_look != Some((look, edge)) {
            set_window_frame(window, look, edge);
            self.framed_look = Some((look, edge));
        }
        if self.refocus_after_busy && !self.busy {
            self.refocus_after_busy = false;
            window.activate_window();
        }
        let palette = Palette::for_window(window, look);
        let root = div()
            .id("grandhonor-flyout")
            .track_focus(&self.focus)
            .on_mouse_move(cx.listener(|panel, ev: &gpui::MouseMoveEvent, _, cx| {
                if panel.slider_active {
                    panel.slider_move(ev.position.x, cx);
                }
            }))
            .on_mouse_up(gpui::MouseButton::Left, cx.listener(|panel, _, _, cx| panel.slider_finish(cx)))
            .on_key_down(cx.listener(|panel, ev: &KeyDownEvent, window, cx| {
                match ev.keystroke.key.as_str() {
                    "escape" if panel.path.len() > 1 => {
                        panel.path.pop();
                        cx.notify();
                    }
                    "escape" => window.remove_window(),
                    "tab" if ev.keystroke.modifiers.shift => window.focus_prev(),
                    "tab" => window.focus_next(),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .flex()
            .flex_col()
            .size_full()
            .font(Font {
                family: TEXT_FONT.into(),
                features: FontFeatures::default(),
                // Inter covers Latin and Cyrillic; Chinese comes from the system UI font.
                fallbacks: Some(FontFallbacks::from_fonts(vec![
                    "Microsoft YaHei UI".into(),
                    "Segoe UI Variable Text".into(),
                ])),
                weight: FontWeight::NORMAL,
                style: FontStyle::Normal,
            })
            .text_size(px(13.))
            .text_color(rgba(palette.text))
            // No own border or radius: Windows rounds the window (DWMWCP_ROUND) and draws a
            // 1 px frame in DWMWA_BORDER_COLOR. A second, inner rounded border never lines up
            // with the DWM corner and leaves broken corners.
            .bg(rgba(if palette.glass { palette.glass_base } else { palette.backdrop }))
            .overflow_hidden()
            .when_some(self.glass.clone().filter(|_| palette.glass), |el, frost| {
                // The image is 1/8 of the window; stretched, its outermost texel fades into the
                // dark base. Drawing it one texel (8 px) larger on each side keeps that fade
                // outside the window.
                el.child(
                    div()
                        .absolute()
                        .top(px(-8.))
                        .left(px(-8.))
                        .right(px(-8.))
                        .bottom(px(-8.))
                        .child(img(ImageSource::Render(frost)).size_full().object_fit(ObjectFit::Fill)),
                )
            })
            .when(palette.glass, |el| {
                el.child(div().absolute().size_full().bg(rgba(palette.backdrop)))
            });

        let Some(snapshot) = &self.snapshot else {
            return root.child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(badge_emblem(44.))
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("GrandHonor"),
                    )
                    .child(div().text_size(px(12.)).text_color(rgba(palette.muted)).child(
                        if self.disconnected {
                            t("Соединение закрыто", "Connection closed", "连接已关闭")
                        } else {
                            t(
                                "Подключение к GrandHonor…",
                                "Connecting to GrandHonor…",
                                "正在连接 GrandHonor…",
                            )
                        },
                    )),
            );
        };

        let panel_entity = cx.entity().downgrade();
        let tel = &snapshot.telemetry;
        let mode_active = tel.performance_active || tel.hunter_active;
        let ctx = Ctx {
            palette,
            busy: self.busy || self.disconnected,
            motion: &self.motion,
            slider_drag: self.slider_drag,
            slider_bounds: self.slider_bounds.clone(),
            slider_commands: self.slider_commands.clone(),
            busy_command: self.busy_command.filter(|_| self.busy),
            generation: self.generation,
        };

        // Header: mark, name, device line, close.
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_2()
            .px_4()
            .pt_4()
            .pb_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .min_w_0()
                    .child(badge_emblem(32.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(1.))
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_size(px(15.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(snapshot.title.clone()),
                                    )
                                    // Only an active special profile (Performance, HUNTER) earns a
                                    // badge; the normal firmware profile needs no label.
                                    .when(!self.disconnected && mode_active, |el| {
                                        el.child(badge_pill(tel.mode_label.clone(), true, palette))
                                    }),
                            )
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(rgba(palette.muted))
                                    .truncate()
                                    .child(snapshot.subtitle.clone()),
                            ),
                    ),
            )
            .child(
                control("close-btn", palette, |window, _| window.remove_window())
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(28.))
                    .rounded_full()
                    .bg(rgba(palette.seg_track))
                    .text_color(rgba(palette.muted))
                    .hover(move |el| el.bg(rgba(palette.elevated)).text_color(rgba(palette.text)))
                    .child(glyph("\u{e8bb}", 9.)),
            );

        // Tab bar: equal segments with a pill that slides to the chosen one.
        let tab_count = snapshot.items.len().max(1);
        let selected_tab = snapshot
            .items
            .iter()
            .position(|item| self.path.first() == Some(&item.text))
            .unwrap_or(0);
        let tab_from = self.tab_from.unwrap_or(selected_tab);
        let tab_height = 52.0;
        let pill_radius = palette.pill_radius(tab_height).min(16.0);
        let mut tab_row = div().relative().flex().items_center().h(px(tab_height));
        tab_row = tab_row.child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .w(relative(1.0 / tab_count as f32))
                .child(
                    div()
                        .size_full()
                        .rounded(px(pill_radius))
                        .bg(rgba(palette.tab_selected))
                        .when(palette.glass, |el| el.border_1().border_color(rgba(palette.rim)))
                        .when(!palette.glass && !palette.dark, |el| el.shadow_sm()),
                )
                .with_animation(
                    SharedString::from(format!("tab-pill-{}", self.tab_generation)),
                    Animation::new(Duration::from_millis(340)).with_easing(spring),
                    move |el, d| {
                        let pos = tab_from as f32 + (selected_tab as f32 - tab_from as f32) * d;
                        el.left(relative(pos / tab_count as f32))
                    },
                ),
        );
        for (index, item) in snapshot.items.iter().enumerate() {
            let selected = index == selected_tab;
            let name = item.text.clone();
            let show_telemetry = item.kind.as_deref() == Some("battery");
            let entity = panel_entity.clone();
            let badge = item.badge.clone();
            tab_row = tab_row.child(
                control(("tab", index), palette, move |_, cx| {
                    let _ = entity.update(cx, |p, cx| {
                        if p.path.first() != Some(&name) || p.path.len() > 1 {
                            p.tab_from = p.snapshot.as_ref().and_then(|s| {
                                s.items.iter().position(|it| p.path.first() == Some(&it.text))
                            });
                            p.tab_generation += 1;
                        }
                        p.path = vec![name.clone()];
                        if show_telemetry {
                            p.request(PANEL_TELEMETRY_COMMAND, cx);
                        }
                        cx.notify();
                    });
                })
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(4.))
                .px_1()
                .rounded(px(pill_radius))
                .when(!selected, |el| el.hover(move |el| el.bg(rgba(palette.hover))))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_color(rgba(if selected { palette.accent } else { palette.muted }))
                        .child(glyph(section_symbol(item.kind.as_deref()), 16.))
                        .when_some(badge, |el, b| {
                            el.child(
                                div()
                                    .px(px(5.))
                                    .rounded_full()
                                    .bg(rgba(palette.accent))
                                    .text_color(rgba(palette.accent_text))
                                    .text_size(px(9.5))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(b),
                            )
                        }),
                )
                .child(
                    div()
                        .max_w_full()
                        .text_size(px(11.))
                        .text_color(rgba(if selected { palette.text } else { palette.muted }))
                        .font_weight(if selected { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                        .truncate()
                        .child(item.text.clone()),
                ),
            );
        }
        let tabs = div()
            .flex_none()
            .mx_3()
            .mb_1()
            .p(px(3.))
            .rounded(px(pill_radius + 3.0))
            .when(palette.glass, |el| palette.surface(el))
            .child(tab_row);

        // Scrollable body. The content of each page settles in once when it opens.
        let path_key = self.path.join("/");
        let mut content = div().flex().flex_col().gap_2().px_4().pt_2().pb_4();

        if self.disconnected {
            content = content.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px(px(14.))
                    .py(px(10.))
                    .rounded(px(palette.radius))
                    .bg(rgba((palette.danger & 0xFFFF_FF00) | 0x1F))
                    .text_size(px(12.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgba(palette.danger))
                    .child(glyph("\u{e7ba}", 13.))
                    .child(t(
                        "GrandHonor не отвечает. Закройте и снова откройте панель",
                        "GrandHonor is not responding. Close and reopen the panel",
                        "GrandHonor 无响应。请关闭并重新打开面板",
                    )),
            );
        }
        if self.host_slow && !self.disconnected {
            content = content.child(status_banner(
                "\u{e916}",
                t(
                    "GrandHonor отвечает медленно, подождите…",
                    "GrandHonor is taking a while, please wait…",
                    "GrandHonor 响应较慢，请稍候…",
                ),
                false,
                palette,
            ));
        }
        if let Some(notice) = self.snapshot.as_ref().and_then(|s| s.notice.as_deref()) {
            let error = self.snapshot.as_ref().is_some_and(|s| s.notice_is_error);
            content = content.child(status_banner(if error { "\u{e7ba}" } else { "\u{e946}" }, notice, error, palette));
        }
        if let Some(section) = self.selected() {
            if self.path.len() == 1 && section.kind.as_deref() == Some("battery") {
                content = content.child(telemetry_overview(tel, self.disconnected, palette));
            }
            if self.path.len() > 1 {
                let entity = panel_entity.clone();
                content = content.child(
                    div().flex().child(
                        control("back", palette, move |_, cx| {
                            let _ = entity.update(cx, |p, cx| {
                                p.path.pop();
                                cx.notify();
                            });
                        })
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .pl_1()
                        .pr_2()
                        .h(px(28.))
                        .rounded(px(8.))
                        .hover(move |el| el.bg(rgba(palette.hover)))
                        .text_size(px(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgba(palette.accent))
                        .child(glyph("\u{e76b}", 12.))
                        .child(snapshot.back.clone()),
                    ),
                );

                content = content.child(
                    div()
                        .px_1()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .font_family(DISPLAY_FONT)
                                .text_size(px(22.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(section.text.clone()),
                        )
                        .when_some(section.badge.clone(), |el, b| {
                            el.child(badge_pill(
                                b,
                                section.kind.as_deref() == Some("driver-update"),
                                palette,
                            ))
                        }),
                );

                if let Some(help) = &section.help {
                    content = content.child(
                        div()
                            .px_1()
                            .pb_1()
                            .text_size(px(12.))
                            .line_height(px(17.))
                            .text_color(rgba(palette.muted))
                            .child(help.clone()),
                    );
                }
            }

            if let Some(children) = &section.children {
                let prefix = format!("/{path_key}");
                content = render_children(content, children, &prefix, &ctx, cx);
            }
            if self.path.len() == 1 && section.kind.as_deref() == Some("settings") {
                content = content.child(about_block(palette));
            }
        }

        // Each page keeps its own scroll handle, so "Back" returns to the same place.
        let scroll = self.scroll.entry(path_key.clone()).or_default().clone();
        let pane = div()
            .id(SharedString::from(format!("pane-{path_key}")))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .child(content.with_animation(
                SharedString::from(format!("page-{path_key}")),
                Animation::new(Duration::from_millis(260)).with_easing(ease_out),
                |el, d| el.opacity(d).relative().top(px((1.0 - d) * 8.0)),
            ));

        // The whole panel settles in once when it opens: fade and a short rise.
        root.child(
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(header)
                .child(tabs)
                .child(pane)
                .with_animation(
                    "panel-enter",
                    Animation::new(Duration::from_millis(300)).with_easing(ease_out),
                    |el, d| el.opacity(d).relative().top(px((1.0 - d) * 10.0)),
                ),
        )
    }
}

/// Lays out a page: section titles, inset grouped lists (consecutive rows share one rounded
/// surface with hairline separators, like macOS System Settings), segmented controls and tiles.
fn render_children(
    mut content: Div,
    children: &[ControlItem],
    prefix: &str,
    ctx: &Ctx,
    cx: &Context<Panel>,
) -> Div {
    let mut group: Vec<(usize, &ControlItem)> = Vec::new();
    let flush = |content: Div, group: &mut Vec<(usize, &ControlItem)>| -> Div {
        if group.is_empty() {
            return content;
        }
        let rows = std::mem::take(group);
        content.child(render_group(&rows, prefix, ctx, cx))
    };
    for (index, item) in children.iter().enumerate() {
        match row_role(item) {
            Role::Row => group.push((index, item)),
            Role::Spacer => {
                content = flush(content, &mut group);
                content = content.child(div().h(px(6.)));
            }
            Role::Header => {
                content = flush(content, &mut group);
                content = content.child(
                    div()
                        .px(px(14.))
                        .pt(px(10.))
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgba(ctx.palette.muted))
                                .child(item.text.clone()),
                        )
                        .when_some(item.help.clone(), |el, help| {
                            el.child(
                                div()
                                    .text_size(px(11.5))
                                    .line_height(px(15.))
                                    .text_color(rgba(ctx.palette.tertiary))
                                    .child(help),
                            )
                        }),
                );
            }
            Role::Block => {
                content = flush(content, &mut group);
                content = content.child(render_block(item, index, prefix, ctx, cx));
            }
        }
    }
    flush(content, &mut group)
}

enum Role {
    Row,
    Header,
    Spacer,
    Block,
}

fn row_role(item: &ControlItem) -> Role {
    let kind = item.kind.as_deref();
    if item.text.is_empty() && item.children.is_none() {
        return Role::Spacer;
    }
    match kind {
        Some("group-grid" | "row-2" | "segment" | "slider" | "grid-hours" | "cta" | "primary" | "banner") => Role::Block,
        _ if item.children.is_none()
            && item.command.is_none()
            && !matches!(kind, Some("driver" | "driver-update" | "switch")) =>
        {
            Role::Header
        }
        _ => Role::Row,
    }
}

fn render_block(item: &ControlItem, index: usize, prefix: &str, ctx: &Ctx, cx: &Context<Panel>) -> AnyElement {
    match item.kind.as_deref() {
        Some("group-grid") => render_two_col_grid(item, index, prefix, ctx, cx).into_any_element(),
        Some("row-2") => render_two_col_row(item, index, prefix, ctx, cx).into_any_element(),
        Some("segment") => render_segment_bar(item, index, ctx, cx).into_any_element(),
        Some("slider") => render_slider(item, index, ctx, cx).into_any_element(),
        Some("grid-hours") => render_hour_grid(item, index, ctx, cx).into_any_element(),
        Some("banner") => render_banner(item, ctx.palette).into_any_element(),
        _ => render_cta(item, index, ctx, cx).into_any_element(),
    }
}

fn render_group(rows: &[(usize, &ControlItem)], prefix: &str, ctx: &Ctx, cx: &Context<Panel>) -> Div {
    let palette = ctx.palette;
    let radius = palette.radius;
    let mut list = palette.surface(div().flex().flex_col().rounded(px(radius)));
    let last = rows.len() - 1;
    for (pos, (index, item)) in rows.iter().enumerate() {
        if pos > 0 {
            // Hairline separator inset to the text column.
            let inset = if leading_icon(item) { 56.0 } else { 16.0 };
            list = list.child(div().h(px(1.)).ml(px(inset)).bg(rgba(palette.separator)));
        }
        let mut row = render_row(item, *index, prefix, ctx, cx);
        if pos == 0 {
            row = row.rounded_t(px(radius - 1.0));
        }
        if pos == last {
            row = row.rounded_b(px(radius - 1.0));
        }
        list = list.child(row);
    }
    list
}

fn leading_icon(item: &ControlItem) -> bool {
    matches!(item.kind.as_deref(), Some("driver" | "driver-update" | "collection"))
}

fn render_switch(on: bool, animate: bool, id: SharedString, palette: Palette) -> AnyElement {
    const W: f32 = 42.0;
    const H: f32 = 26.0;
    const KNOB: f32 = 22.0;
    let travel = W - KNOB - 4.0;
    let draw = move |d: f32| {
        let pos = if on { d } else { 1.0 - d };
        div()
            .relative()
            .flex_none()
            .w(px(W))
            .h(px(H))
            .rounded_full()
            .bg(rgba(palette.track))
            .child(
                div()
                    .absolute()
                    .size_full()
                    .rounded_full()
                    .bg(rgba(palette.accent))
                    .opacity(pos.clamp(0.0, 1.0)),
            )
            .child(
                div()
                    .absolute()
                    .top(px(2.))
                    .left(px(2.0 + travel * pos))
                    .size(px(KNOB))
                    .rounded_full()
                    .bg(rgb(0xffffff))
                    .shadow_sm(),
            )
    };
    if animate {
        div()
            .flex_none()
            .with_animation(id, Animation::new(Duration::from_millis(300)).with_easing(spring), move |el, d| {
                el.child(draw(d))
            })
            .into_any_element()
    } else {
        draw(1.0).into_any_element()
    }
}

fn render_row(item: &ControlItem, index: usize, prefix: &str, ctx: &Ctx, cx: &Context<Panel>) -> Stateful<Div> {
    let palette = ctx.palette;
    let nested = item.children.is_some();
    let kind = item.kind.as_deref();
    let is_switch = kind == Some("switch");
    let is_driver_update = kind == Some("driver-update");
    let is_danger = kind == Some("danger");
    let action = item.command;
    let id = ("item", index);
    let key = item_key(prefix, item);

    let name = item.text.clone();
    let entity = cx.entity().downgrade();
    let interactive = nested || (action.is_some() && !ctx.busy);
    // A switch or action without a command is unavailable (for example in the portable copy):
    // it looks dimmed instead of working-but-dead.
    let unavailable = !nested && action.is_none() && matches!(kind, Some("switch" | "card" | "danger"));
    let row = if interactive {
        control(id, palette, move |_, cx| {
            let _ = entity.update(cx, |p, cx| {
                if nested {
                    p.path.push(name.clone());
                    cx.notify();
                } else if let Some(cmd) = action {
                    p.request(cmd, cx);
                }
            });
        })
        .hover(move |el| el.bg(rgba(palette.hover)))
    } else {
        div().id(id).border_1().border_color(rgba(0x00000000))
    };

    row.when(unavailable, |el| el.opacity(0.45))
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px(px(15.))
        .min_h(px(48.))
        .py(px(10.))
        .flex_none()
        .when(leading_icon(item), |c| {
            c.child(
                div()
                    .flex_none()
                    .size(px(28.))
                    .rounded(px(7.))
                    .bg(rgba(if is_driver_update { palette.accent } else { palette.elevated }))
                    .text_color(rgba(if is_driver_update { palette.accent_text } else { palette.text }))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(glyph(section_symbol(kind), 14.)),
            )
        })
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_size(px(13.5))
                        .font_weight(if (item.selected && !is_switch) || is_driver_update {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::MEDIUM
                        })
                        .when(is_danger, |el| el.text_color(rgba(palette.danger)))
                        .truncate()
                        .child(item.text.clone()),
                )
                .when_some(item.help.clone(), |el, help| {
                    el.child(
                        div()
                            .text_size(px(11.5))
                            .line_height(px(15.))
                            .text_color(rgba(palette.muted))
                            .child(help),
                    )
                }),
        )
        .when_some(item.badge.clone(), |c, b| c.child(badge_pill(b, is_driver_update, palette)))
        .when(is_switch, |c| {
            let animate = ctx.motion.flipped.contains(&key);
            c.child(render_switch(
                item.selected,
                animate,
                SharedString::from(format!("switch-{key}-{}", ctx.generation)),
                palette,
            ))
        })
        .when(!is_switch && !leading_icon(item) && item.selected, |c| {
            c.child(
                div()
                    .flex_none()
                    .text_color(rgba(palette.accent))
                    .child(glyph("\u{e73e}", 13.)),
            )
        })
        .when(nested, |c| {
            c.child(
                div()
                    .flex_none()
                    .text_color(rgba(palette.tertiary))
                    .child(glyph("\u{e76c}", 11.)),
            )
        })
}

fn render_cta(item: &ControlItem, index: usize, ctx: &Ctx, cx: &Context<Panel>) -> Stateful<Div> {
    let palette = ctx.palette;
    let action = item.command;
    let id = ("cta", index);
    let entity = cx.entity().downgrade();
    // Only the pressed button shows that it works; one without a command is unavailable.
    let running = action.is_some() && ctx.busy_command == action;
    let button = if let Some(cmd) = action.filter(|_| !ctx.busy) {
        control(id, palette, move |_, cx| {
            let _ = entity.update(cx, |p, cx| p.request(cmd, cx));
        })
        .hover(|el| el.opacity(0.88))
    } else {
        div().id(id).opacity(if action.is_none() { 0.45 } else if running { 0.6 } else { 1.0 })
    };
    button
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px(px(16.))
        .py(px(12.))
        .rounded(px(if palette.glass { 22.0 } else { 12.0 }))
        .bg(rgba(palette.accent))
        .text_color(rgba(palette.accent_text))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .min_w_0()
                .child(
                    div()
                        .text_size(px(13.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(item.text.clone()),
                )
                .when_some(item.help.clone(), |el, help| {
                    el.child(div().text_size(px(11.5)).opacity(0.85).child(help))
                }),
        )
        .child(glyph(if running { "\u{e916}" } else { "\u{e72a}" }, 13.))
}

/// A one-line message above the page: an error (red) or information (accent).
fn status_banner(icon: &'static str, text: &str, error: bool, palette: Palette) -> Div {
    let colour = if error { palette.danger } else { palette.accent };
    div()
        .flex()
        .items_center()
        .gap_2()
        .px(px(14.))
        .py(px(10.))
        .rounded(px(palette.radius))
        .bg(rgba((colour & 0xFFFF_FF00) | 0x1F))
        .text_size(px(12.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgba(if error { palette.danger } else { palette.text }))
        .child(div().flex_none().text_color(rgba(colour)).child(glyph(icon, 13.)))
        .child(div().flex_1().min_w_0().child(text.to_string()))
}

fn render_banner(item: &ControlItem, palette: Palette) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px(px(14.))
        .py(px(11.))
        .rounded(px(palette.radius))
        .bg(rgba(palette.accent_soft))
        .child(
            div()
                .flex_none()
                .size(px(26.))
                .rounded_full()
                .bg(rgba(palette.accent))
                .text_color(rgba(palette.accent_text))
                .flex()
                .items_center()
                .justify_center()
                .child(glyph("\u{e946}", 13.)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(1.))
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_size(px(12.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(item.text.clone()),
                )
                .when_some(item.help.clone(), |el, help| {
                    el.child(div().text_size(px(11.5)).text_color(rgba(palette.muted)).child(help))
                }),
        )
        .when_some(item.badge.clone(), |el, b| el.child(badge_pill(b, true, palette)))
}

fn render_two_col_grid(
    container: &ControlItem,
    base_idx: usize,
    prefix: &str,
    ctx: &Ctx,
    cx: &Context<Panel>,
) -> Div {
    let palette = ctx.palette;
    let children = container.children.as_deref().unwrap_or(&[]);
    let container_key = item_key(prefix, container);
    let mut col = div().flex().flex_col().gap_2();
    for (row_idx, pair) in children.chunks(2).enumerate() {
        let mut row = div().flex().gap_2();
        for (col_idx, child) in pair.iter().enumerate() {
            let id = ("tile", base_idx * 100 + row_idx * 10 + col_idx);
            let cmd = child.command;
            let entity = cx.entity().downgrade();
            let card = if let Some(cmd_id) = cmd.filter(|_| !ctx.busy) {
                control(id, palette, move |_, cx| {
                    let _ = entity.update(cx, |p, cx| p.request(cmd_id, cx));
                })
                .hover(move |el| el.bg(rgba(palette.hover)))
            } else {
                div().id(id).border_1().border_color(rgba(0x00000000)).when(cmd.is_none(), |el| el.opacity(0.45))
            };
            let flipped = ctx.motion.flipped.contains(&item_key(&container_key, child));
            let tile = palette
                .surface(card)
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.))
                .px(px(14.))
                .py(px(12.))
                .rounded(px(palette.radius))
                .when(child.selected, |el| el.border_color(rgba(palette.accent)))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(13.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .truncate()
                                .child(child.text.clone()),
                        )
                        .when_some(child.badge.clone(), |el, b| {
                            el.child(badge_pill(b, child.selected, palette))
                        })
                        .when(child.selected && child.badge.is_none(), |el| {
                            el.child(
                                div()
                                    .flex_none()
                                    .text_color(rgba(palette.accent))
                                    .child(glyph("\u{e73e}", 12.)),
                            )
                        }),
                )
                .when_some(child.help.clone(), |el, h| {
                    el.child(
                        div()
                            .text_size(px(11.5))
                            .text_color(rgba(palette.muted))
                            .truncate()
                            .child(h),
                    )
                });
            row = row.child(if flipped && child.selected {
                tile.with_animation(
                    SharedString::from(format!("tile-{}-{}", base_idx * 100 + row_idx * 10 + col_idx, ctx.generation)),
                    Animation::new(Duration::from_millis(320)).with_easing(spring),
                    |el, d| el.opacity(0.5 + 0.5 * d.min(1.0)),
                )
                .into_any_element()
            } else {
                tile.into_any_element()
            });
        }
        if pair.len() == 1 {
            row = row.child(div().flex_1());
        }
        col = col.child(row);
    }
    col
}

fn render_two_col_row(
    container: &ControlItem,
    base_idx: usize,
    prefix: &str,
    ctx: &Ctx,
    cx: &Context<Panel>,
) -> Div {
    let children = container.children.as_deref().unwrap_or(&[]);
    let container_key = item_key(prefix, container);
    let mut row = div().flex().gap_2();
    for (idx, child) in children.iter().enumerate() {
        let radius = ctx.palette.radius - 1.0;
        row = row.child(
            ctx.palette
                .surface(div().flex_1().min_w_0().rounded(px(ctx.palette.radius)))
                .child(
                    render_row(child, base_idx * 100 + idx, &container_key, ctx, cx)
                        .rounded(px(radius)),
                ),
        );
    }
    row
}

fn render_segment_bar(container: &ControlItem, base_idx: usize, ctx: &Ctx, cx: &Context<Panel>) -> Div {
    let palette = ctx.palette;
    let children = container.children.as_deref().unwrap_or(&[]);
    let count = children.len().max(1);
    let height = 32.0;
    let radius = palette.pill_radius(height);
    let selected = children.iter().position(|c| c.selected);
    let key = segment_key(container);
    let from = ctx.motion.segment_from.get(&key).copied();

    let mut track = div().relative().flex().items_center().h(px(height));
    if let Some(to) = selected {
        let start = from.unwrap_or(to);
        let pill = div()
            .absolute()
            .top_0()
            .bottom_0()
            .w(relative(1.0 / count as f32))
            .left(relative(to as f32 / count as f32))
            .child(
                div()
                    .size_full()
                    .rounded(px(radius - 2.0))
                    .bg(rgba(palette.seg_selected))
                    .when(palette.glass, |el| el.border_1().border_color(rgba(palette.rim)))
                    .when(!palette.glass && !palette.dark, |el| el.shadow_sm()),
            );
        track = track.child(if from.is_some() {
            pill.with_animation(
                SharedString::from(format!("seg-{key}-{}", ctx.generation)),
                Animation::new(Duration::from_millis(340)).with_easing(spring),
                move |el, d| {
                    let pos = start as f32 + (to as f32 - start as f32) * d;
                    el.left(relative(pos / count as f32))
                },
            )
            .into_any_element()
        } else {
            pill.into_any_element()
        });
    }

    for (idx, child) in children.iter().enumerate() {
        let id = ("seg", base_idx * 100 + idx);
        let entity = cx.entity().downgrade();
        let cmd = child.command;
        let btn = if let Some(cmd_id) = cmd.filter(|_| !ctx.busy) {
            control(id, palette, move |_, cx| {
                let _ = entity.update(cx, |p, cx| p.request(cmd_id, cx));
            })
            .when(!child.selected, |el| el.hover(move |el| el.bg(rgba(palette.hover))))
        } else {
            div().id(id).border_1().border_color(rgba(0x00000000))
        };
        track = track.child(
            btn.flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .px(px(4.))
                .rounded(px(radius - 2.0))
                .child(
                    div()
                        .text_size(px(12.5))
                        .font_weight(if child.selected { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                        .text_color(rgba(if child.selected { palette.text } else { palette.muted }))
                        .truncate()
                        .child(child.text.clone()),
                ),
        );
    }
    div()
        .p(px(2.))
        .rounded(px(radius))
        .bg(rgba(palette.seg_track))
        .when(palette.glass, |el| el.border_1().border_color(rgba(palette.rim)))
        .child(track)
}

/// Where along the track (0–1) a pointer x position is.
fn slider_fraction(x: gpui::Pixels, bounds: Bounds<gpui::Pixels>) -> f32 {
    let width = f32::from(bounds.size.width);
    if width <= 0.0 {
        return 0.0;
    }
    (f32::from(x - bounds.origin.x) / width).clamp(0.0, 1.0)
}

/// The stop nearest to a track position, for `count` stops.
fn slider_stop(fraction: f32, count: usize) -> usize {
    ((fraction * (count.max(2) - 1) as f32).round() as usize).min(count.saturating_sub(1))
}

/// A slider: each child is one stop (left to right) whose command applies it; the first and
/// last child texts label the ends. Press anywhere on the track and drag: the knob follows the
/// pointer and the stop under it is applied as it changes (the newest wins while the host is
/// busy); on release the knob settles on the nearest stop. Left/Right arrows step when focused.
fn render_slider(container: &ControlItem, base_idx: usize, ctx: &Ctx, cx: &Context<Panel>) -> Div {
    let palette = ctx.palette;
    let children = container.children.as_deref().unwrap_or(&[]);
    let count = children.len().max(2);
    let selected = children.iter().position(|c| c.selected).unwrap_or(0);
    let commands: Vec<Option<u32>> = children.iter().map(|c| c.command).collect();
    let dragging = ctx.slider_drag.is_some();
    let fill = ctx.slider_drag.unwrap_or(selected as f32 / (count - 1) as f32);
    let knob = if dragging { 20.0 } else { 18.0 };
    let enabled = !ctx.busy || dragging;

    // The newest snapshot's stops, used when a queued stop is finally sent.
    *ctx.slider_commands.borrow_mut() = commands.clone();
    let bounds_cell = ctx.slider_bounds.clone();
    let measure = gpui::canvas(move |bounds, _, _| bounds_cell.set(Some(bounds)), |_, _, _, _| {})
        .absolute()
        .left_0()
        .right_0()
        .top_0()
        .bottom_0();
    let track = div()
        .id(("slider", base_idx))
        .relative()
        .h(px(28.))
        .mx(px(9.))
        .rounded(px(8.))
        .cursor_pointer()
        .focusable()
        .tab_stop(true)
        .focus(move |el| el.bg(rgba(palette.hover)))
        .child(measure)
        .child(div().absolute().left_0().right_0().top(px(12.)).h(px(4.)).rounded(px(2.)).bg(rgba(palette.seg_track)))
        .child(div().absolute().left_0().top(px(12.)).h(px(4.)).w(relative(fill)).rounded(px(2.)).bg(rgba(palette.accent)))
        .child(
            div()
                .absolute()
                .top(px(14. - knob / 2.))
                .left(relative(fill))
                .ml(px(-knob / 2.))
                .size(px(knob))
                .rounded(px(knob / 2.))
                .bg(rgba(0xffffffff))
                .border_1()
                .border_color(rgba(0x0000001f))
                .shadow_sm(),
        )
        .when(enabled, |el| {
            el.on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |panel, ev: &gpui::MouseDownEvent, _, cx| {
                    panel.slider_active = true;
                    panel.slider_sent = None;
                    panel.slider_move(ev.position.x, cx);
                }),
            )
            .on_mouse_up_out(gpui::MouseButton::Left, cx.listener(|panel, _, _, cx| panel.slider_finish(cx)))
            .on_key_down(cx.listener(move |panel, ev: &KeyDownEvent, _, cx| {
                // Step from where the knob is now (a queued step counts), not the last snapshot.
                let last = count - 1;
                let here = panel.slider_drag.map_or(selected, |f| slider_stop(f, count));
                let next = match ev.keystroke.key.as_str() {
                    "left" | "down" => here.saturating_sub(1),
                    "right" | "up" => (here + 1).min(last),
                    "home" => 0,
                    "end" => last,
                    _ => return,
                };
                panel.slider_drag = Some(next as f32 / last as f32);
                panel.request_stop(next, cx);
                cx.notify();
                cx.stop_propagation();
            }))
        });
    let caption = |text: &str| div().text_size(px(11.5)).text_color(rgba(palette.muted)).child(text.to_string());
    let first = children.first().map(|c| c.text.as_str()).unwrap_or("");
    let last = children.last().map(|c| c.text.as_str()).unwrap_or("");
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(track)
        .when(!first.is_empty() || !last.is_empty(), |el| {
            el.child(div().flex().justify_between().px(px(2.)).child(caption(first)).child(caption(last)))
        })
}

fn render_hour_grid(container: &ControlItem, base_idx: usize, ctx: &Ctx, cx: &Context<Panel>) -> Div {
    let palette = ctx.palette;
    let children = container.children.as_deref().unwrap_or(&[]);
    let mut grid = div().flex().flex_col().gap_2();
    for (row_idx, chunk) in children.chunks(4).enumerate() {
        let mut row = div().flex().gap_2();
        for (col_idx, child) in chunk.iter().enumerate() {
            let id = ("hr", base_idx * 100 + row_idx * 10 + col_idx);
            let cmd = child.command;
            let entity = cx.entity().downgrade();
            let cell = if let Some(cmd_id) = cmd.filter(|_| !ctx.busy) {
                control(id, palette, move |_, cx| {
                    let _ = entity.update(cx, |p, cx| p.request(cmd_id, cx));
                })
                .when(!child.selected, |el| el.hover(move |el| el.bg(rgba(palette.elevated))))
            } else {
                div().id(id).border_1().border_color(rgba(0x00000000))
            };
            row = row.child(
                cell.flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(34.))
                    .rounded(px(palette.pill_radius(34.0)))
                    .when(child.selected, |el| el.bg(rgba(palette.accent)))
                    .when(!child.selected, |el| palette.surface(el))
                    .font(Font {
                        family: TEXT_FONT.into(),
                        features: tabular(),
                        fallbacks: None,
                        weight: if child.selected { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM },
                        style: FontStyle::Normal,
                    })
                    .text_size(px(12.5))
                    .text_color(rgba(if child.selected { palette.accent_text } else { palette.text }))
                    .child(child.text.clone()),
            );
        }
        grid = grid.child(row);
    }
    grid
}

/// The end of System: name, version and licence, quietly centred like a macOS About line.
fn about_block(palette: Palette) -> Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.))
        .pt(px(18.))
        .pb(px(6.))
        .child(badge_emblem(28.))
        .child(
            div()
                .font(Font {
                    family: TEXT_FONT.into(),
                    features: tabular(),
                    fallbacks: None,
                    weight: FontWeight::SEMIBOLD,
                    style: FontStyle::Normal,
                })
                .text_size(px(12.5))
                .text_color(rgba(palette.muted))
                .child(format!("GrandHonor {}", env!("CARGO_PKG_VERSION"))),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(rgba(palette.tertiary))
                .child(t("Свободная программа · лицензия MIT", "Free software · MIT licence", "自由软件 · MIT 许可")),
        )
}

fn telemetry_overview(telemetry: &TelemetrySummary, disconnected: bool, palette: Palette) -> Div {
    let fresh = telemetry.sampled_ticks.is_some_and(recent_sample);
    let live = !disconnected && fresh;
    let status = if disconnected {
        t("Нет связи", "Disconnected", "连接已断开").to_string()
    } else if fresh {
        String::new()
    } else if telemetry.sampled_ticks.is_some() {
        t("Данные устарели", "Out of date", "数据已过期").to_string()
    } else {
        t("Датчики недоступны", "Sensors unavailable", "传感器不可用").to_string()
    };

    let reading = |value: Option<i32>| value.filter(|_| live).map(|v| v.to_string());
    let fan_note = |target: Option<i32>| {
        target.map(|rpm| format!("{} {rpm}", t("цель", "target", "目标")))
    };
    // This tile is the battery's own power flow (BatteryStatus), not total system power:
    // charging = power going into the battery; on battery = what the whole laptop draws.
    // On the charger the system's own draw is not reported without a driver, so it is not shown.
    // Fresh only, like every other reading: an old value shows a dash.
    let power = telemetry.power_watts.filter(|_| live);
    let (power_title, power_state) = match power {
        Some(w) if w > 0.05 => (t("Мощность заряда", "Charging power", "充电功率"), t("в батарею", "into battery", "充入电池")),
        Some(w) if w < -0.05 => (t("Расход", "Draw", "耗电"), t("от батареи", "on battery", "电池供电")),
        Some(_) if telemetry.ac_online == Some(true) => (t("Не заряжается", "Not charging", "未充电"), t("от сети", "on AC", "接通电源")),
        _ => (t("Батарея", "Battery", "电池"), t("без нагрузки", "idle", "空闲")),
    };
    // The tile is narrow: on the charger its wattage replaces the percent (the percent is in
    // the tray and in Windows anyway).
    let charger = telemetry.charger_watts.filter(|_| live);
    let power_note = power.map(|_| match (charger, telemetry.battery_percent) {
        (Some(w), _) => format!("{w} {}", t("Вт зарядка", "W charger", "瓦充电器")),
        (None, Some(percent)) => format!("{power_state} · {percent}%"),
        (None, None) => power_state.to_string(),
    });
    let charge = (!disconnected && telemetry.charge_label != "?")
        .then(|| telemetry.charge_label.clone());
    let rpm = t("об/мин", "rpm", "转/分");

    let cell = |label: &str, value: Option<String>, unit: &str, note: Option<String>| {
        telemetry_reading(label, value, unit, note, palette)
    };
    let divider = || div().w(px(1.)).my(px(12.)).bg(rgba(palette.separator));
    let top = div()
        .flex()
        .child(cell(
            t("Процессор", "CPU", "处理器"),
            reading(telemetry.cpu_temp),
            "°C",
            telemetry.cpu_watts.filter(|_| live).map(|w| format!("{w:.1} {}", t("Вт", "W", "瓦"))),
        ))
        .child(divider())
        .child(cell(t("Батарея, темп.", "Battery temp", "电池温度"), reading(telemetry.battery_temp), "°C", None))
        .child(divider())
        .child(cell(
            power_title,
            power.map(|w| format!("{:.1}", w.abs())),
            t("Вт", "W", "瓦"),
            power_note,
        ));
    let bottom = div()
        .flex()
        .child(cell(
            t("Вентилятор 1", "Fan 1", "风扇 1"),
            reading(telemetry.fan1_rpm),
            rpm,
            fan_note(telemetry.fan1_target).filter(|_| live),
        ))
        .child(divider())
        .child(cell(
            t("Вентилятор 2", "Fan 2", "风扇 2"),
            reading(telemetry.fan2_rpm),
            rpm,
            fan_note(telemetry.fan2_target).filter(|_| live),
        ))
        .child(divider())
        .child(cell(t("Лимит заряда", "Charge limit", "充电上限"), charge, "", None));

    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .pb_1()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .px(px(14.))
                .child(
                    div()
                        .text_size(px(12.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgba(palette.muted))
                        .child(t("Датчики", "Sensors", "传感器")),
                )
                .when(!live, |el| {
                    el.child(
                        div()
                            .text_size(px(11.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgba(if disconnected { palette.danger } else { palette.muted }))
                            .child(status),
                    )
                }),
        )
        .child(
            palette
                .surface(div().flex().flex_col().rounded(px(palette.radius)))
                .child(top)
                .child(div().h(px(1.)).mx(px(14.)).bg(rgba(palette.separator)))
                .child(bottom),
        )
}

fn telemetry_reading(
    label: &str,
    value: Option<String>,
    unit: &str,
    note: Option<String>,
    palette: Palette,
) -> Div {
    let has_value = value.is_some();
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(3.))
        .px(px(14.))
        .py(px(12.))
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgba(palette.muted))
                .truncate()
                .child(label.to_string()),
        )
        .child(
            div()
                .flex()
                .items_baseline()
                .gap(px(3.))
                .child(
                    div()
                        .font(numeric_font(FontWeight::SEMIBOLD))
                        .text_size(px(24.))
                        .line_height(px(28.))
                        .text_color(rgba(if has_value { palette.text } else { palette.tertiary }))
                        .child(value.unwrap_or_else(|| "—".into())),
                )
                .when(has_value && !unit.is_empty(), |el| {
                    el.child(
                        div()
                            .text_size(px(11.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgba(palette.muted))
                            .child(unit.to_string()),
                    )
                }),
        )
        .child(
            div()
                .h(px(14.))
                .text_size(px(10.5))
                .font(Font {
                    family: TEXT_FONT.into(),
                    features: tabular(),
                    fallbacks: None,
                    weight: FontWeight::NORMAL,
                    style: FontStyle::Normal,
                })
                .text_color(rgba(palette.tertiary))
                .truncate()
                .when_some(note.filter(|_| has_value), |el, note| el.child(note)),
        )
}

fn recent_sample(dotnet_ticks: i64) -> bool {
    let ticks_per_second = 10_000_000;
    // Six missed samples (3 s) and a reading counts as out of date.
    let max_age = 6 * PANEL_TELEMETRY_INTERVAL_MS as i64 * ticks_per_second / 1000;
    (0..max_age).contains(&now_dotnet_ticks().saturating_sub(dotnet_ticks))
}

fn badge_pill(text: String, accented: bool, palette: Palette) -> Div {
    div()
        .flex_none()
        .px(px(8.))
        .py(px(2.))
        .rounded_full()
        .bg(rgba(if accented { palette.accent_soft } else { palette.elevated }))
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgba(if accented { palette.accent } else { palette.muted }))
        .child(text)
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
struct MonitorInfo {
    cb_size: u32,
    monitor: Rect,
    work: Rect,
    flags: u32,
}

#[repr(C)]
#[derive(Default)]
struct AppBarData {
    cb_size: u32,
    hwnd: isize,
    callback_message: u32,
    edge: u32,
    rc: Rect,
    lparam: isize,
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegOpenKeyExW(
        hkey: isize,
        subkey: *const u16,
        options: u32,
        sam_desired: u32,
        phk_result: *mut isize,
    ) -> i32;
    fn RegQueryValueExW(
        hkey: isize,
        value_name: *const u16,
        reserved: *const u32,
        lp_type: *mut u32,
        lp_data: *mut u8,
        lpcb_data: *mut u32,
    ) -> i32;
    fn RegCloseKey(hkey: isize) -> i32;
}

#[link(name = "dwmapi")]
unsafe extern "system" {
    fn DwmGetColorizationColor(color: *mut u32, opaque_blend: *mut i32) -> i32;
    fn DwmSetWindowAttribute(window: isize, attribute: u32, value: *const u32, size: u32) -> i32;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn SHAppBarMessage(dw_message: u32, p_data: *mut AppBarData) -> usize;
}

#[link(name = "shcore")]
unsafe extern "system" {
    fn GetDpiForMonitor(hmonitor: isize, dpi_type: u32, dpi_x: *mut u32, dpi_y: *mut u32) -> i32;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn SystemParametersInfoW(
        ui_action: u32,
        ui_param: u32,
        pv_param: *mut Rect,
        f_win_ini: u32,
    ) -> i32;
    fn MonitorFromPoint(pt: Point, dw_flags: u32) -> isize;
    fn GetMonitorInfoW(hmonitor: isize, lpmi: *mut MonitorInfo) -> i32;
    fn GetDpiForWindow(hwnd: isize) -> u32;
    fn GetDpiForSystem() -> u32;
}

/// DWM draws a 1 px frame around the window. Glass has none (`DWMWA_COLOR_NONE`): the frosted
/// desktop copy meets the desktop directly. Graphite keeps a faint hairline so black on black
/// still has an edge.
/// Average 0xRRGGBB of the outermost pixels of a top-down BGRA image.
fn edge_colour(bgra: &[u8], w: usize, h: usize) -> u32 {
    let (mut sum, mut n) = ([0u64; 3], 0u64);
    for y in 0..h {
        for x in 0..w {
            if x == 0 || y == 0 || x + 1 == w || y + 1 == h {
                let p = (y * w + x) * 4;
                if let Some(px) = bgra.get(p..p + 3) {
                    sum[0] += u64::from(px[2]);
                    sum[1] += u64::from(px[1]);
                    sum[2] += u64::from(px[0]);
                    n += 1;
                }
            }
        }
    }
    let n = n.max(1);
    ((sum[0] / n) as u32) << 16 | ((sum[1] / n) as u32) << 8 | (sum[2] / n) as u32
}

/// `rgb` (0xRRGGBB) under the tint `rgba` (0xRRGGBBAA), as the panel draws them.
fn blend_over(rgb: u32, rgba: u32) -> u32 {
    let a = (rgba & 0xff) as f32 / 255.0;
    let mix = |shift: u32| {
        let under = ((rgb >> shift) & 0xff) as f32;
        let over = ((rgba >> (shift + 8)) & 0xff) as f32;
        (over * a + under * (1.0 - a)).round() as u32
    };
    mix(16) << 16 | mix(8) << 8 | mix(0)
}

fn set_window_frame(window: &Window, look: Look, glass_edge: Option<u32>) {
    const DWMWA_BORDER_COLOR: u32 = 34;
    const DWMWA_COLOR_NONE: u32 = 0xFFFF_FFFE;
    let Some(hwnd) = window_hwnd(window) else { return };
    let dark = matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
    // COLORREF 0x00BBGGRR.
    let color = match look {
        // COLORREF is 0x00BBGGRR.
        Look::Glass => glass_edge.map_or(DWMWA_COLOR_NONE, |c| (c & 0xff) << 16 | (c & 0xff00) | (c >> 16) & 0xff),
        Look::Graphite if dark => 0x0026_2626,
        Look::Graphite => 0x00d8_d6d4,
    };
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &color, size_of::<u32>() as u32);
    }
}

fn setup_frosted_backdrop(window: &Window) {
    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
    const DWMWCP_ROUND: u32 = 2;
    const DWMSBT_TRANSIENTWINDOW: u32 = 3;

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(raw) = handle.as_raw() else {
        return;
    };
    let hwnd = raw.hwnd.get();
    let dark: u32 = match window.appearance() {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => 1,
        _ => 0,
    };

    window.set_background_appearance(WindowBackgroundAppearance::Blurred);
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark,
            size_of::<u32>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &DWMWCP_ROUND,
            size_of::<u32>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &DWMSBT_TRANSIENTWINDOW,
            size_of::<u32>() as u32,
        );
        // The window frame colour is set by `set_window_frame` for the chosen look (none in
        // Glass); setting one here as well would override it with a dark outline.
        let _ = GetDpiForWindow(hwnd);
    }
}

fn parse_tray_rect() -> Option<Rect> {
    let raw = std::env::var("GRANDHONOR_TRAY_RECT").ok()?;
    let mut parts = raw.split(',').filter_map(|s| s.trim().parse::<i32>().ok());
    let rect = Rect {
        left: parts.next()?,
        top: parts.next()?,
        right: parts.next()?,
        bottom: parts.next()?,
    };
    (rect.right > rect.left && rect.bottom > rect.top).then_some(rect)
}

fn infer_taskbar_edge(mon: Rect, work: Rect) -> u32 {
    const ABE_LEFT: u32 = 0;
    const ABE_TOP: u32 = 1;
    const ABE_RIGHT: u32 = 2;
    const ABE_BOTTOM: u32 = 3;

    let gaps = [
        (work.top - mon.top, ABE_TOP),
        (mon.bottom - work.bottom, ABE_BOTTOM),
        (work.left - mon.left, ABE_LEFT),
        (mon.right - work.right, ABE_RIGHT),
    ];
    gaps.into_iter()
        .filter(|(g, _)| *g > 0)
        .max_by_key(|(g, _)| *g)
        .map_or(ABE_BOTTOM, |(_, edge)| edge)
}

fn compute_taskbar_flyout_bounds(cx: &App) -> Bounds<gpui::Pixels> {
    const ABM_GETTASKBARPOS: u32 = 5;
    const SPI_GETWORKAREA: u32 = 0x0030;
    const MONITOR_DEFAULTTOPRIMARY: u32 = 1;
    const MDT_EFFECTIVE_DPI: u32 = 0;
    const ABE_LEFT: u32 = 0;
    const ABE_TOP: u32 = 1;
    const ABE_RIGHT: u32 = 2;
    const ABE_BOTTOM: u32 = 3;

    let tray_rect = parse_tray_rect();
    let anchor_pt = tray_rect.map_or(Point { x: 0, y: 0 }, |r| Point {
        x: (r.left + r.right) / 2,
        y: (r.top + r.bottom) / 2,
    });
    let hmon = unsafe { MonitorFromPoint(anchor_pt, MONITOR_DEFAULTTOPRIMARY) };

    let mut mi = MonitorInfo {
        cb_size: size_of::<MonitorInfo>() as u32,
        monitor: Rect::default(),
        work: Rect::default(),
        flags: 0,
    };
    let has_mon = hmon != 0 && unsafe { GetMonitorInfoW(hmon, &mut mi) } != 0;

    let mut fallback_work = Rect::default();
    let work = if has_mon {
        mi.work
    } else if unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut fallback_work, 0) } != 0 {
        fallback_work
    } else {
        Rect::default()
    };

    if work.right > work.left && work.bottom > work.top {
        let mut dpi_x = 0u32;
        let mut dpi_y = 0u32;
        let dpi = if hmon != 0
            && unsafe { GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) } == 0
            && dpi_x >= 96
        {
            dpi_x
        } else {
            unsafe { GetDpiForSystem() }
        };
        let scale = (dpi as f32 / 96.0).max(1.0);

        let left = work.left as f32 / scale;
        let top = work.top as f32 / scale;
        let right = work.right as f32 / scale;
        let bottom = work.bottom as f32 / scale;

        let w = FLYOUT_WIDTH.min((right - left - EDGE_MARGIN * 2.0).max(360.0));
        let h = FLYOUT_HEIGHT.min((bottom - top - EDGE_MARGIN * 2.0).max(440.0));

        let mut abd = AppBarData {
            cb_size: size_of::<AppBarData>() as u32,
            ..Default::default()
        };
        let has_bar = unsafe { SHAppBarMessage(ABM_GETTASKBARPOS, &mut abd) } != 0
            && abd.rc.right > abd.rc.left
            && abd.rc.bottom > abd.rc.top;
        let edge = if has_bar {
            abd.edge
        } else if has_mon {
            infer_taskbar_edge(mi.monitor, mi.work)
        } else {
            ABE_BOTTOM
        };

        let clamp_x = |ideal: f32| ideal.clamp(left + EDGE_MARGIN, (right - w - EDGE_MARGIN).max(left + EDGE_MARGIN));
        let clamp_y = |ideal: f32| ideal.clamp(top + EDGE_MARGIN, (bottom - h - EDGE_MARGIN).max(top + EDGE_MARGIN));

        let (x, y) = match edge {
            ABE_LEFT => {
                let ideal_y = tray_rect
                    .map_or(bottom - h - EDGE_MARGIN, |r| ((r.top + r.bottom) as f32 / (2.0 * scale)) - h * 0.5);
                (left + EDGE_MARGIN, clamp_y(ideal_y))
            }
            ABE_RIGHT => {
                let ideal_y = tray_rect
                    .map_or(bottom - h - EDGE_MARGIN, |r| ((r.top + r.bottom) as f32 / (2.0 * scale)) - h * 0.5);
                (clamp_x(right - w - EDGE_MARGIN), clamp_y(ideal_y))
            }
            ABE_TOP => {
                let ideal_x = tray_rect
                    .map_or(right - w - EDGE_MARGIN, |r| ((r.left + r.right) as f32 / (2.0 * scale)) - w * 0.5);
                (clamp_x(ideal_x), top + EDGE_MARGIN)
            }
            _ => {
                let ideal_x = tray_rect
                    .map_or(right - w - EDGE_MARGIN, |r| ((r.left + r.right) as f32 / (2.0 * scale)) - w * 0.5);
                (clamp_x(ideal_x), clamp_y(bottom - h - EDGE_MARGIN))
            }
        };

        return Bounds::new(point(px(x), px(y)), size(px(w), px(h)));
    }

    let fallback = size(px(FLYOUT_WIDTH), px(FLYOUT_HEIGHT));
    cx.primary_display().map_or_else(
        || Bounds::centered(None, fallback, cx),
        |display| {
            let s = display.bounds();
            Bounds::new(
                point(
                    s.origin.x + (s.size.width - fallback.width - px(EDGE_MARGIN)).max(px(0.)),
                    s.origin.y + (s.size.height - fallback.height - px(60.)).max(px(0.)),
                ),
                fallback,
            )
        },
    )
}

/// Exits the panel when the GrandHonor host process ends, even if it crashed or was killed.
/// The thread sleeps in `WaitForSingleObject` and uses no CPU.
fn watch_host_process() {
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const INFINITE: u32 = u32::MAX;
    let Some(pid) = std::env::var("GRANDHONOR_HOST_PID").ok().and_then(|v| v.parse::<u32>().ok())
    else {
        return;
    };
    let handle = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
    if handle == 0 {
        std::process::exit(0);
    }
    let handle = handle as usize;
    let _ = std::thread::Builder::new()
        .name("host-watchdog".into())
        .stack_size(64 * 1024)
        .spawn(move || {
            unsafe { WaitForSingleObject(handle as isize, INFINITE) };
            std::process::exit(0);
        });
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
    fn WaitForSingleObject(handle: isize, milliseconds: u32) -> u32;
}

fn main() {
    watch_host_process();
    Application::new().run(|cx: &mut App| {
        // Inter (SIL OFL 1.1, Assets/fonts) is compiled in: no install, same type everywhere.
        let _ = cx.text_system().add_fonts(vec![
            Cow::Borrowed(include_bytes!("../../Assets/fonts/Inter-Regular.ttf").as_slice()),
            Cow::Borrowed(include_bytes!("../../Assets/fonts/Inter-Medium.ttf").as_slice()),
            Cow::Borrowed(include_bytes!("../../Assets/fonts/Inter-SemiBold.ttf").as_slice()),
            Cow::Borrowed(include_bytes!("../../Assets/fonts/InterDisplay-SemiBold.ttf").as_slice()),
        ]);
        let bounds = compute_taskbar_flyout_bounds(cx);
        cx.on_window_closed(|cx| cx.quit()).detach();

        let result = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(360.), px(440.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("GrandHonor".into()),
                    appears_transparent: true,
                    ..Default::default()
                }),
                kind: WindowKind::PopUp,
                is_resizable: false,
                is_minimizable: false,
                is_movable: true,
                window_background: WindowBackgroundAppearance::Blurred,
                app_id: Some("GrandHonor.Panel".into()),
                ..Default::default()
            },
            |window, cx| {
                setup_frosted_backdrop(window);
                window.activate_window();
                cx.new(|cx| {
                    let focus = cx.focus_handle();
                    focus.focus(window);
                    // Like Windows flyouts, close as soon as the user clicks elsewhere:
                    // the panel process then exits and frees its memory and GPU resources.
                    // GRANDHONOR_KEEP_OPEN=1 keeps it open (screenshots only).
                    let keep_open = std::env::var_os("GRANDHONOR_KEEP_OPEN").is_some();
                    cx.observe_window_activation(window, move |panel: &mut Panel, window, _| {
                        if window.is_window_active() {
                            panel.was_active = true;
                        } else if panel.was_active && !keep_open && !panel.busy {
                            // While an action runs the host may show a file dialog or a
                            // question: the panel stays and takes the focus back afterwards.
                            window.remove_window();
                        } else if panel.busy {
                            panel.refocus_after_busy = true;
                        }
                    })
                    .detach();
                    cx.observe_window_appearance(window, |_, _, cx| cx.notify()).detach();
                    let mut panel = Panel {
                        snapshot: None,
                        path: Vec::new(),
                        busy: false,
                        polling: false,
                        pending_command: None,
                        request_seq: 0,
                        host_slow: false,
                        refocus_after_busy: false,
                        busy_command: None,
                        disconnected: false,
                        was_active: false,
                        focus,
                        motion: Motion::default(),
                        generation: 0,
                        tab_from: None,
                        tab_generation: 0,
                        glass: None,
                        glass_tried: false,
                        framed_look: None,
                        glass_edge: None,
                        slider_drag: None,
                        slider_sent: None,
                        slider_active: false,
                        slider_bounds: Default::default(),
                        slider_commands: Default::default(),
                        scroll: HashMap::new(),
                        stable: Stabilizer::default(),
                    };
                    panel.request(0, cx);
                    panel.start_telemetry(cx);
                    panel
                })
            },
        );
        if result.is_err() {
            cx.quit();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slider_stops_round_to_the_nearest() {
        assert_eq!(slider_stop(0.0, 10), 0);
        assert_eq!(slider_stop(1.0, 10), 9);
        assert_eq!(slider_stop(0.5, 10), 5);
        assert_eq!(slider_stop(0.05, 10), 0);
        assert_eq!(slider_stop(0.06, 10), 1);
        let b = Bounds::new(point(px(10.), px(0.)), size(px(100.), px(20.)));
        assert_eq!(slider_fraction(px(60.), b), 0.5);
        assert_eq!(slider_fraction(px(0.), b), 0.0);
        assert_eq!(slider_fraction(px(500.), b), 1.0);
    }

    #[test]
    fn hold_ignores_jitter_but_follows_real_changes() {
        let mut h = Hold::default();
        assert_eq!(h.update(Some(45.0), 2.0), Some(45.0));
        // ±1 noise is not shown...
        assert_eq!(h.update(Some(46.0), 2.0), Some(45.0));
        assert_eq!(h.update(Some(45.0), 2.0), Some(45.0));
        assert_eq!(h.update(Some(44.0), 2.0), Some(45.0));
        // ...a step of 2 is shown at once...
        assert_eq!(h.update(Some(48.0), 2.0), Some(48.0));
        // ...and a small change that lasts 1.5 s is accepted.
        assert_eq!(h.update(Some(49.0), 2.0), Some(48.0));
        assert_eq!(h.update(Some(49.0), 2.0), Some(48.0));
        assert_eq!(h.update(Some(49.0), 2.0), Some(49.0));
        // A missing reading is never replaced by the old value.
        assert_eq!(h.update(None, 2.0), None);
    }

    #[test]
    fn stopped_fans_show_zero_at_once() {
        let mut s = Stabilizer::default();
        let mut t = TelemetrySummary { fan1_rpm: Some(2276), ..Default::default() };
        assert_eq!(s.apply(t.clone()).fan1_rpm, Some(2280));
        t.fan1_rpm = Some(2291);
        assert_eq!(s.apply(t.clone()).fan1_rpm, Some(2280));
        t.fan1_rpm = Some(0);
        assert_eq!(s.apply(t).fan1_rpm, Some(0));
    }
}
