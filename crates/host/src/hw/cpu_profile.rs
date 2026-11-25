//! Power → Processor profile: Quiet, Balanced or Performance, chosen separately for the charger
//! and for battery. A profile is a set of documented processor settings of the active Windows
//! power scheme (energy preference per core class, boost, core parking, ramp thresholds),
//! written once to the scheme's plugged-in and on-battery values; Windows itself switches
//! between them when the charger changes, so nothing runs in the background.
//!
//! This is what HONOR PC Manager's "Performance" does on the Windows side (its own scheme
//! "Honor Performance"), minus the 100 % minimum processor state that keeps the CPU hot at
//! idle. Before the first change the scheme's own values are saved (`CpuProfileOriginal`) and
//! they come back when both sources are set to "Keep", or on uninstall.

use crate::settings;
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::System::Power::{
    PowerGetActiveScheme, PowerReadACValueIndex, PowerReadDCValueIndex, PowerSetActiveScheme,
    PowerWriteACValueIndex, PowerWriteDCValueIndex,
};
use windows_core::GUID;

const SUB_PROCESSOR: GUID = GUID::from_u128(0x54533251_82be_4824_96c1_47b60b740d00);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Quiet = 1,
    Balanced = 2,
    Performance = 3,
}

impl Profile {
    pub const ALL: [Profile; 3] = [Profile::Quiet, Profile::Balanced, Profile::Performance];

    pub fn from_code(code: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|p| *p as u32 == code)
    }
}

/// One processor setting and its value per profile: (plugged in, on battery).
struct Row {
    setting: GUID,
    quiet: (u32, u32),
    balanced: (u32, u32),
    performance: (u32, u32),
}

/// Balanced is Windows' own Balanced scheme as read on a Core Ultra (Panther Lake) laptop;
/// Performance follows HONOR's "Honor Performance" scheme except the minimum processor state.
const ROWS: [Row; 8] = [
    // Energy performance preference, 0 = fastest … 100 = most efficient: P-cores, E-cores, LP E-cores.
    Row { setting: GUID::from_u128(0x36687f9e_e3a5_4dbf_b1dc_15eb381c6863), quiet: (80, 85), balanced: (50, 50), performance: (20, 30) },
    Row { setting: GUID::from_u128(0x36687f9e_e3a5_4dbf_b1dc_15eb381c6864), quiet: (80, 85), balanced: (50, 50), performance: (20, 30) },
    Row { setting: GUID::from_u128(0x36687f9e_e3a5_4dbf_b1dc_15eb381c6865), quiet: (70, 80), balanced: (33, 50), performance: (10, 30) },
    // Boost mode: 0 off, 2 aggressive.
    Row { setting: GUID::from_u128(0xbe337238_0d82_4146_a960_4f3749d470c7), quiet: (0, 0), balanced: (2, 2), performance: (2, 2) },
    // Boost policy, % of the boost headroom used.
    Row { setting: GUID::from_u128(0x45bcc044_d885_43e2_8605_ee0ec6e96b59), quiet: (40, 20), balanced: (85, 40), performance: (100, 85) },
    // Core parking: minimum unparked cores, %.
    Row { setting: GUID::from_u128(0x0cc5b647_c1df_4637_891a_dec35c318583), quiet: (25, 10), balanced: (75, 25), performance: (100, 75) },
    // Load thresholds to raise and to lower performance, %.
    Row { setting: GUID::from_u128(0x06cadf0e_64ed_448a_8927_ce7bf90eb35d), quiet: (90, 90), balanced: (60, 90), performance: (30, 45) },
    Row { setting: GUID::from_u128(0x12a0ab44_fe28_4fa9_b3bd_4b64f44960a6), quiet: (40, 40), balanced: (20, 30), performance: (10, 15) },
];

impl Row {
    fn value(&self, profile: Profile, ac: bool) -> u32 {
        let pair = match profile {
            Profile::Quiet => self.quiet,
            Profile::Balanced => self.balanced,
            Profile::Performance => self.performance,
        };
        if ac { pair.0 } else { pair.1 }
    }
}

fn source_key(ac: bool) -> &'static str {
    if ac { "Ac" } else { "Dc" }
}

pub fn chosen(ac: bool) -> Option<Profile> {
    settings::cpu_profile(source_key(ac)).and_then(Profile::from_code)
}

/// The active scheme, freed when dropped.
struct ActiveScheme(*mut GUID);

impl ActiveScheme {
    fn get() -> Option<Self> {
        let mut scheme: *mut GUID = std::ptr::null_mut();
        let ok = unsafe { PowerGetActiveScheme(None, &mut scheme) }.is_ok() && !scheme.is_null();
        ok.then_some(Self(scheme))
    }

    fn guid(&self) -> GUID {
        unsafe { *self.0 }
    }

    fn read(&self, setting: &GUID, ac: bool) -> Option<u32> {
        let mut value = 0u32;
        let status = unsafe {
            if ac {
                PowerReadACValueIndex(None, Some(self.0), Some(&SUB_PROCESSOR), Some(setting), &mut value).0
            } else {
                PowerReadDCValueIndex(None, Some(self.0), Some(&SUB_PROCESSOR), Some(setting), &mut value)
            }
        };
        (status == 0).then_some(value)
    }

    fn write(&self, setting: &GUID, ac: bool, value: u32) -> bool {
        unsafe {
            if ac {
                PowerWriteACValueIndex(None, self.0, Some(&SUB_PROCESSOR), Some(setting), value).is_ok()
            } else {
                PowerWriteDCValueIndex(None, self.0, Some(&SUB_PROCESSOR), Some(setting), value) == 0
            }
        }
    }

    /// Re-activating the scheme makes Windows pick up the written values at once.
    fn commit(&self) {
        let _ = unsafe { PowerSetActiveScheme(None, Some(self.0)) };
    }
}

impl Drop for ActiveScheme {
    fn drop(&mut self) {
        let _ = unsafe { LocalFree(Some(HLOCAL(self.0 as *mut _))) };
    }
}

/// Saved scheme values: "scheme-guid|ac,dc;ac,dc;…" in `ROWS` order. A setting the scheme
/// does not have (an older CPU without LP E-cores) is kept as "-,-".
fn encode(scheme: &GUID, values: &[(Option<u32>, Option<u32>)]) -> String {
    let opt = |v: Option<u32>| v.map_or("-".to_string(), |n| n.to_string());
    let list: Vec<String> = values.iter().map(|(a, d)| format!("{},{}", opt(*a), opt(*d))).collect();
    format!("{:?}|{}", scheme, list.join(";"))
}

fn decode(text: &str) -> Option<(String, Vec<(Option<u32>, Option<u32>)>)> {
    let (scheme, list) = text.split_once('|')?;
    let values: Vec<_> = list
        .split(';')
        .map(|pair| {
            let (a, d) = pair.split_once(',').unwrap_or(("-", "-"));
            (a.parse().ok(), d.parse().ok())
        })
        .collect();
    (values.len() == ROWS.len()).then(|| (scheme.to_string(), values))
}

fn restore_into(scheme: &ActiveScheme, values: &[(Option<u32>, Option<u32>)]) {
    for (row, (ac, dc)) in ROWS.iter().zip(values) {
        if let Some(v) = ac {
            scheme.write(&row.setting, true, *v);
        }
        if let Some(v) = dc {
            scheme.write(&row.setting, false, *v);
        }
    }
    scheme.commit();
}

/// Writes the chosen profiles into the active scheme, or puts the scheme's own values back
/// when neither source has one. Called after a choice in the panel and once at start (Windows
/// updates can reset schemes). Returns false when Windows refused a write.
pub fn apply() -> bool {
    let Some(scheme) = ActiveScheme::get() else { return false };
    let saved = settings::cpu_profile_original().and_then(|t| decode(&t));
    let (ac_profile, dc_profile) = (chosen(true), chosen(false));
    if ac_profile.is_none() && dc_profile.is_none() {
        if let Some((guid, values)) = &saved
            && *guid == format!("{:?}", scheme.guid())
        {
            restore_into(&scheme, values);
        }
        settings::set_cpu_profile_original(None);
        return true;
    }
    // The user switched schemes since the values were saved: save this scheme's own values.
    let originals = match saved {
        Some((guid, values)) if guid == format!("{:?}", scheme.guid()) => values,
        _ => {
            let values: Vec<_> = ROWS.iter().map(|r| (scheme.read(&r.setting, true), scheme.read(&r.setting, false))).collect();
            settings::set_cpu_profile_original(Some(&encode(&scheme.guid(), &values)));
            values
        }
    };
    let mut ok = true;
    for (row, (orig_ac, orig_dc)) in ROWS.iter().zip(&originals) {
        for (ac, profile, original) in [(true, ac_profile, orig_ac), (false, dc_profile, orig_dc)] {
            // A setting the scheme lacks stays absent.
            if original.is_none() {
                continue;
            }
            let value = profile.map_or(original.unwrap_or_default(), |p| row.value(p, ac));
            ok &= scheme.write(&row.setting, ac, value);
        }
    }
    scheme.commit();
    ok
}

/// Uninstall: put the scheme's own values back and forget the choice.
pub fn restore_original() {
    settings::set_cpu_profile(source_key(true), None);
    settings::set_cpu_profile(source_key(false), None);
    let _ = apply();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_values_round_trip() {
        let scheme = GUID::from_u128(0x381b4222_f694_41f0_9685_ff5bb260df2e);
        let mut values = vec![(Some(50), Some(50)); ROWS.len()];
        values[2] = (None, None);
        let text = encode(&scheme, &values);
        let (guid, back) = decode(&text).unwrap();
        assert_eq!(guid, format!("{scheme:?}"));
        assert_eq!(back, values);
        assert!(decode("garbage").is_none());
    }

    #[test]
    fn balanced_matches_windows_and_performance_is_faster() {
        for row in &ROWS {
            for ac in [true, false] {
                assert!(row.value(Profile::Quiet, ac) <= 100 && row.value(Profile::Performance, ac) <= 100);
            }
        }
        // Energy preference: lower is faster.
        assert!(ROWS[0].value(Profile::Performance, true) < ROWS[0].value(Profile::Balanced, true));
        assert!(ROWS[0].value(Profile::Quiet, true) > ROWS[0].value(Profile::Balanced, true));
        // Quiet turns boost off.
        assert_eq!(ROWS[3].value(Profile::Quiet, false), 0);
    }
}
