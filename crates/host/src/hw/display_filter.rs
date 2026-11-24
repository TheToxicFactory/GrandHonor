//! Display → Eye comfort, the HONOR PC Manager "Display & Eye Comfort" modes without HONOR's
//! LCD service. PC Manager applies them with its own engine; the numbers are HONOR's own for
//! the ZQC-P panel (`BasicDisplay.xml` of model ZhuqueC). GrandHonor applies them as one
//! full-screen colour matrix through the Windows Magnification API (`MagSetFullscreenColorEffect`):
//! DWM does the work, the host creates no GPU device, and the effect ends with the process.
//!
//! - Eye comfort: less blue (RGB gains 1 / 1 / 0.95, 0.91 or 0.86 × 0.95 red for Strong).
//! - E-book: warm monochrome for reading, its tint following the time of day like HONOR's
//!   four bands (00–06, 06–12, 12–18, 18–24).
//! - Comfort: slightly calmer colour (saturation 0.88, gains 1 / 0.99 / 0.985).
//!
//! One thread owns the effect. It sleeps on a channel; only E-book wakes it at the next band.

use crate::settings::{self, ColorProfile, DisplayFilter};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;
use windows::Win32::UI::Magnification::{MAGCOLOREFFECT, MagInitialize, MagSetFullscreenColorEffect, MagUninitialize};

/// HONOR's E-book matrices for ZhuqueC, rows R', G', B' (each × 1/1000), by time band.
const EBOOK: [[f32; 9]; 4] = [
    [270.0, 631.0, 99.0, 235.0, 562.0, 86.0, 194.0, 482.0, 71.0],    // 00–06
    [333.0, 333.0, 189.0, 333.0, 333.0, 183.0, 333.0, 333.0, 161.0], // 06–12
    [333.0, 333.0, 130.0, 333.0, 333.0, 120.0, 333.0, 333.0, 100.0], // 12–18
    [310.0, 531.0, 99.0, 285.0, 462.0, 86.0, 244.0, 382.0, 71.0],    // 18–24
];

enum Msg {
    Apply,
}

fn sender() -> &'static Sender<Msg> {
    static TX: OnceLock<Sender<Msg>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("display-filter".into())
            .stack_size(128 * 1024)
            .spawn(move || run(rx));
        tx
    })
}

/// Applies the saved choice (at start, after a change in the panel, after waking).
pub fn apply_saved() {
    let _ = sender().send(Msg::Apply);
}

// ---- Colour profiles ---------------------------------------------------------------------------
//
// The ZQC-P panel covers more than sRGB (HONOR's BasicDisplay.xml for ZhuqueC gives its measured
// primaries below), so ordinary sRGB content looks oversaturated. A profile maps a standard colour
// space onto the panel: M = panel⁻¹ · standard, both RGB→XYZ for D65. The matrix works on the
// encoded values DWM blends, so it is exact for white, grey and the primaries and close between.

/// Chromaticities (x, y) of R, G, B and the white point.
type Gamut = [(f64, f64); 4];

const PANEL: Gamut = [(0.6853, 0.3143), (0.2399, 0.7288), (0.1389, 0.0427), (0.3127, 0.3290)];
const SRGB: Gamut = [(0.640, 0.330), (0.300, 0.600), (0.150, 0.060), (0.3127, 0.3290)];
const DISPLAY_P3: Gamut = [(0.680, 0.320), (0.265, 0.690), (0.150, 0.060), (0.3127, 0.3290)];

fn inverse(m: [[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < 1e-12 {
        return None;
    }
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            let (a, b) = ((j + 1) % 3, (j + 2) % 3);
            let (c, d) = ((i + 1) % 3, (i + 2) % 3);
            r[i][j] = (m[a][c] * m[b][d] - m[a][d] * m[b][c]) / det;
        }
    }
    Some(r)
}

fn mul(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}

/// RGB → XYZ for a gamut, white normalised to Y = 1.
fn rgb_to_xyz(g: Gamut) -> Option<[[f64; 3]; 3]> {
    let col = |(x, y): (f64, f64)| [x / y, 1.0, (1.0 - x - y) / y];
    let (r, gr, b, w) = (col(g[0]), col(g[1]), col(g[2]), col(g[3]));
    let m = [[r[0], gr[0], b[0]], [r[1], gr[1], b[1]], [r[2], gr[2], b[2]]];
    let inv = inverse(m)?;
    let s: Vec<f64> = (0..3).map(|i| (0..3).map(|k| inv[i][k] * w[k]).sum()).collect();
    Some([
        [m[0][0] * s[0], m[0][1] * s[1], m[0][2] * s[2]],
        [m[1][0] * s[0], m[1][1] * s[1], m[1][2] * s[2]],
        [m[2][0] * s[0], m[2][1] * s[1], m[2][2] * s[2]],
    ])
}

/// The panel-space matrix for a profile; None for the panel's own (native) colours.
pub fn profile_matrix(profile: ColorProfile) -> Option<[f32; 9]> {
    let target = match profile {
        ColorProfile::Native => return None,
        ColorProfile::Srgb => SRGB,
        ColorProfile::DisplayP3 => DISPLAY_P3,
    };
    let m = mul(inverse(rgb_to_xyz(PANEL)?)?, rgb_to_xyz(target)?);
    let mut out = [0.0f32; 9];
    for i in 0..3 {
        for j in 0..3 {
            out[i * 3 + j] = m[i][j] as f32;
        }
    }
    Some(out)
}

/// The filter applied after the profile: out = filter · profile · in.
pub fn combine(filter: Option<[f32; 9]>, profile: Option<[f32; 9]>) -> Option<[f32; 9]> {
    match (filter, profile) {
        (None, p) => p,
        (f, None) => f,
        (Some(f), Some(p)) => {
            let mut out = [0.0f32; 9];
            for i in 0..3 {
                for j in 0..3 {
                    out[i * 3 + j] = (0..3).map(|k| f[i * 3 + k] * p[k * 3 + j]).sum();
                }
            }
            Some(out)
        }
    }
}

/// Green and blue gains for warmth 1–10. Steps 3, 5 and 8 match HONOR's Eye protection
/// gains (1 1 0.95, 1 1 0.91, 1 0.95 0.86); above 5 green drops too, so white turns amber.
pub fn warmth_gains(warmth: u32) -> (f32, f32) {
    let w = warmth.clamp(1, settings::WARMTH_MAX) as f32;
    let blue = 1.0 - 0.018 * w;
    let green = if w <= 5.0 { 1.0 } else { 1.0 - 0.0167 * (w - 5.0) };
    (green, blue)
}

/// The 3×3 matrix (rows = output R, G, B; columns = input R, G, B) for a choice, or None for off.
pub fn matrix(filter: DisplayFilter, warmth: u32, hour: u32) -> Option<[f32; 9]> {
    match filter {
        DisplayFilter::Off => None,
        DisplayFilter::EyeComfort => {
            let (g, b) = warmth_gains(warmth);
            Some([1.0, 0.0, 0.0, 0.0, g, 0.0, 0.0, 0.0, b])
        }
        DisplayFilter::Ebook => {
            let band = (hour.min(23) / 6) as usize;
            let mut m = EBOOK[band];
            for v in &mut m {
                *v /= 1000.0;
            }
            Some(m)
        }
        DisplayFilter::Comfort => {
            // Saturation s around Rec. 709 luma, then HONOR's per-channel gains.
            let s = 0.88;
            let luma = [0.2126, 0.7152, 0.0722];
            let gains = [1.0, 0.99, 0.985];
            let mut m = [0.0f32; 9];
            for out in 0..3 {
                for inp in 0..3 {
                    let id = if out == inp { 1.0 } else { 0.0 };
                    m[out * 3 + inp] = (s * id + (1.0 - s) * luma[inp]) * gains[out];
                }
            }
            Some(m)
        }
    }
}

fn effect(m: Option<[f32; 9]>) -> MAGCOLOREFFECT {
    let mut e = MAGCOLOREFFECT::default();
    for i in 0..5 {
        e.transform[i * 5 + i] = 1.0;
    }
    if let Some(m) = m {
        // The Magnification API multiplies a row vector by the matrix: [input][output].
        for out in 0..3 {
            for inp in 0..3 {
                e.transform[inp * 5 + out] = m[out * 3 + inp];
            }
        }
    }
    e
}

fn local_hour_and_minutes_left() -> (u32, u32) {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let hour = u32::from(t.wHour);
    let minute = u32::from(t.wMinute);
    let left = ((hour / 6 + 1) * 6 - hour) * 60 - minute;
    (hour, left.max(1))
}

fn run(rx: Receiver<Msg>) {
    let mut initialized = false;
    let mut wait = None;
    loop {
        let msg = match wait {
            Some(d) => match rx.recv_timeout(d) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(m) => Some(m),
                Err(_) => break,
            },
        };
        let _ = msg.map(|Msg::Apply| ());
        let filter = settings::display_filter();
        let (hour, minutes_left) = local_hour_and_minutes_left();
        let m = combine(matrix(filter, settings::eye_warmth(), hour), profile_matrix(settings::color_profile()));
        if m.is_some() && !initialized {
            initialized = unsafe { MagInitialize() }.as_bool();
            if !initialized {
                crate::log::error("Eye comfort is unavailable: MagInitialize failed");
                wait = None;
                continue;
            }
        }
        if initialized {
            let e = effect(m);
            if !unsafe { MagSetFullscreenColorEffect(&e) }.as_bool() {
                crate::log::error("Could not apply the eye comfort colour filter");
            }
            if m.is_none() {
                unsafe {
                    let _ = MagUninitialize();
                }
                initialized = false;
            }
        }
        // E-book follows the time of day: wake at the next 6-hour band, otherwise sleep.
        wait = (filter == DisplayFilter::Ebook).then(|| Duration::from_secs(u64::from(minutes_left) * 60));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_is_identity_and_bands_follow_the_clock() {
        assert!(matrix(DisplayFilter::Off, 5, 12).is_none());
        let e = effect(None);
        assert_eq!(e.transform[0], 1.0);
        assert_eq!(e.transform[6], 1.0);
        let night = matrix(DisplayFilter::Ebook, 5, 3).unwrap();
        let noon = matrix(DisplayFilter::Ebook, 5, 13).unwrap();
        assert!((night[0] - 0.270).abs() < 1e-6 && (noon[2] - 0.130).abs() < 1e-6);
    }

    #[test]
    fn profiles_keep_white_and_narrow_the_gamut() {
        assert!(profile_matrix(ColorProfile::Native).is_none());
        for profile in [ColorProfile::Srgb, ColorProfile::DisplayP3] {
            let m = profile_matrix(profile).unwrap();
            for row in 0..3 {
                let white: f32 = m[row * 3..row * 3 + 3].iter().sum();
                assert!((white - 1.0).abs() < 1e-4, "{profile:?} row {row}: {white}");
            }
        }
        // sRGB red is less saturated than the panel's: its red output is < 1 and mixes green.
        let s = profile_matrix(ColorProfile::Srgb).unwrap();
        assert!(s[0] < 0.95 && s[3] > 0.02);
        // Composition with a filter keeps the filter's white.
        let c = combine(matrix(DisplayFilter::EyeComfort, 5, 12), Some(s)).unwrap();
        assert!((c[6] + c[7] + c[8] - 0.91).abs() < 1e-4);
    }

    #[test]
    fn comfort_keeps_grey_grey() {
        let m = matrix(DisplayFilter::Comfort, 5, 12).unwrap();
        // White in → each output equals its gain (rows of the saturation part sum to 1).
        let white: Vec<f32> = (0..3).map(|o| m[o * 3] + m[o * 3 + 1] + m[o * 3 + 2]).collect();
        assert!((white[0] - 1.0).abs() < 1e-5 && (white[2] - 0.985).abs() < 1e-5);
    }
}
