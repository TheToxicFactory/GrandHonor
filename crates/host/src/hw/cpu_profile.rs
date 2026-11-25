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

/// A power scheme by GUID: the active one, or a saved one.
struct ActiveScheme(GUID);

/// Parses the `{:?}` form of a GUID ("381B4222-F694-41F0-9685-FF5BB260DF2E").
fn parse_guid(text: &str) -> Option<GUID> {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    (hex.len() == 32).then(|| u128::from_str_radix(&hex, 16).ok()).flatten().map(GUID::from_u128)
}

impl ActiveScheme {
    fn get() -> Option<Self> {
        let mut scheme: *mut GUID = std::ptr::null_mut();
        let ok = unsafe { PowerGetActiveScheme(None, &mut scheme) }.is_ok() && !scheme.is_null();
        if !ok {
            return None;
        }
        let guid = unsafe { *scheme };
        let _ = unsafe { LocalFree(Some(HLOCAL(scheme as *mut _))) };
        Some(Self(guid))
    }

    fn open(text: &str) -> Option<Self> {
        parse_guid(text).map(Self)
    }

    fn guid(&self) -> GUID {
        self.0
    }

    fn read(&self, setting: &GUID, ac: bool) -> Option<u32> {
        let mut value = 0u32;
        let status = unsafe {
            if ac {
                PowerReadACValueIndex(None, Some(&self.0), Some(&SUB_PROCESSOR), Some(setting), &mut value).0
            } else {
                PowerReadDCValueIndex(None, Some(&self.0), Some(&SUB_PROCESSOR), Some(setting), &mut value)
            }
        };
        (status == 0).then_some(value)
    }

    fn write(&self, setting: &GUID, ac: bool, value: u32) -> bool {
        unsafe {
            if ac {
                PowerWriteACValueIndex(None, &self.0, Some(&SUB_PROCESSOR), Some(setting), value).is_ok()
            } else {
                PowerWriteDCValueIndex(None, &self.0, Some(&SUB_PROCESSOR), Some(setting), value) == 0
            }
        }
    }

    /// Re-activating the active scheme makes Windows pick up the written values at once.
    /// Only ever called on the active scheme (it would switch to any other).
    fn commit(&self) {
        let _ = unsafe { PowerSetActiveScheme(None, Some(&self.0)) };
    }
}

/// Saved scheme values: "scheme-guid|setting-guid=ac,dc;…", keyed by setting so a later
/// version that adds or reorders `ROWS` still reads them. A setting the scheme does not have
/// (an older CPU without LP E-cores) is kept as "-,-"; one missing from the text is left alone.
/// The 2.5.4 form without setting GUIDs ("ac,dc;…" in `ROWS` order) is still read.
fn encode(scheme: &GUID, values: &[(Option<u32>, Option<u32>)]) -> String {
    let opt = |v: Option<u32>| v.map_or("-".to_string(), |n| n.to_string());
    let list: Vec<String> =
        ROWS.iter().zip(values).map(|(row, (a, d))| format!("{:?}={},{}", row.setting, opt(*a), opt(*d))).collect();
    format!("{:?}|{}", scheme, list.join(";"))
}

fn decode(text: &str) -> Option<(String, Vec<(Option<u32>, Option<u32>)>)> {
    let (scheme, list) = text.split_once('|')?;
    parse_guid(scheme)?;
    let pair = |p: &str| {
        let (a, d) = p.split_once(',').unwrap_or(("-", "-"));
        (a.parse().ok(), d.parse().ok())
    };
    let items: Vec<&str> = list.split(';').collect();
    let values = if items.iter().all(|i| i.contains('=')) {
        let keyed: Vec<(GUID, (Option<u32>, Option<u32>))> =
            items.iter().filter_map(|i| i.split_once('=')).filter_map(|(g, v)| Some((parse_guid(g)?, pair(v)))).collect();
        ROWS.iter().map(|row| keyed.iter().find(|(g, _)| *g == row.setting).map_or((None, None), |(_, v)| *v)).collect()
    } else if items.len() == 8 {
        // 2.5.4: the same eight rows in the same order as now.
        items.iter().map(|i| pair(i)).collect()
    } else {
        return None;
    };
    Some((scheme.to_string(), values))
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
}

/// Writes the chosen profiles into the active scheme, or puts the scheme's own values back
/// when neither source has one. Called after a choice in the panel and once at start (Windows
/// updates can reset schemes). Returns false when Windows refused a write.
/// Every scheme the profiles have written into keeps its own entry, so switching schemes
/// never loses the values of the one written before. Entries are joined with '/'.
type Saved = Vec<(String, Vec<(Option<u32>, Option<u32>)>)>;

fn load_saved() -> Saved {
    settings::cpu_profile_original().map_or_else(Vec::new, |t| t.split('/').filter_map(decode).collect())
}

fn store_saved(saved: &Saved) {
    if saved.is_empty() {
        settings::set_cpu_profile_original(None);
        return;
    }
    let text: Vec<String> = saved
        .iter()
        .filter_map(|(guid, values)| parse_guid(guid).map(|g| encode(&g, values)))
        .collect();
    settings::set_cpu_profile_original(Some(&text.join("/")));
}

/// Puts back every saved scheme's own values (each written by its GUID, active or not).
fn restore_all(saved: &Saved) {
    for (guid, values) in saved {
        if let Some(scheme) = ActiveScheme::open(guid) {
            restore_into(&scheme, values);
        }
    }
}

pub fn apply() -> bool {
    let Some(scheme) = ActiveScheme::get() else { return false };
    let mut saved = load_saved();
    let (ac_profile, dc_profile) = (chosen(true), chosen(false));
    if ac_profile.is_none() && dc_profile.is_none() {
        restore_all(&saved);
        // The active scheme last, so Windows re-reads it.
        scheme.commit();
        store_saved(&Vec::new());
        return true;
    }
    let key = format!("{:?}", scheme.guid());
    let originals = match saved.iter().find(|(g, _)| *g == key) {
        Some((_, values)) => values.clone(),
        None => {
            let values: Vec<_> = ROWS.iter().map(|r| (scheme.read(&r.setting, true), scheme.read(&r.setting, false))).collect();
            saved.push((key, values.clone()));
            store_saved(&saved);
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
        assert_eq!(parse_guid(&format!("{scheme:?}")), Some(scheme));
        // The 2.5.4 positional form still reads.
        let old = format!("{scheme:?}|50,50;50,50;-,-;50,50;50,50;50,50;50,50;50,50");
        assert_eq!(decode(&old).unwrap().1, back);
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
