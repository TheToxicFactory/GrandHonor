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
    fn commit(&self) -> bool {
        Self::get().is_some_and(|active| active.0 == self.0)
            && unsafe { PowerSetActiveScheme(None, Some(&self.0)) }.is_ok()
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

/// The settings of 2.5.4, in the order it saved them.
const ORDER_254: [GUID; 8] = [
    GUID::from_u128(0x36687f9e_e3a5_4dbf_b1dc_15eb381c6863),
    GUID::from_u128(0x36687f9e_e3a5_4dbf_b1dc_15eb381c6864),
    GUID::from_u128(0x36687f9e_e3a5_4dbf_b1dc_15eb381c6865),
    GUID::from_u128(0xbe337238_0d82_4146_a960_4f3749d470c7),
    GUID::from_u128(0x45bcc044_d885_43e2_8605_ee0ec6e96b59),
    GUID::from_u128(0x0cc5b647_c1df_4637_891a_dec35c318583),
    GUID::from_u128(0x06cadf0e_64ed_448a_8927_ce7bf90eb35d),
    GUID::from_u128(0x12a0ab44_fe28_4fa9_b3bd_4b64f44960a6),
];

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
    } else if items.len() == ORDER_254.len() {
        // 2.5.4 wrote the values by position in its own row order, fixed here so a later
        // change of `ROWS` cannot shift them.
        let old: Vec<(GUID, (Option<u32>, Option<u32>))> = ORDER_254.iter().zip(&items).map(|(g, i)| (*g, pair(i))).collect();
        ROWS.iter().map(|row| old.iter().find(|(g, _)| *g == row.setting).map_or((None, None), |(_, v)| *v)).collect()
    } else {
        return None;
    };
    Some((scheme.to_string(), values))
}

/// Every scheme the profiles have written into keeps its own entry, so switching schemes
/// never loses the values of the one written before. Entries are joined with '/'.
type Saved = Vec<(String, Vec<(Option<u32>, Option<u32>)>)>;

fn load_saved() -> Saved {
    settings::cpu_profile_original().map_or_else(Vec::new, |t| {
        t.split('/')
            .filter_map(|entry| {
                let parsed = decode(entry);
                if parsed.is_none() {
                    crate::log::error(&format!("Processor profile: saved scheme values not readable, dropped: {entry}"));
                }
                parsed
            })
            .collect()
    })
}

fn store_saved(saved: &Saved) -> bool {
    if saved.is_empty() {
        return settings::set_cpu_profile_original(None);
    }
    let text: Vec<String> = saved
        .iter()
        .filter_map(|(guid, values)| parse_guid(guid).map(|g| encode(&g, values)))
        .collect();
    settings::set_cpu_profile_original(Some(&text.join("/")))
}

/// Puts back every saved scheme's own values (each written by its GUID, active or not).
fn restore_all(mut saved: Saved) -> bool {
    let active = ActiveScheme::get();
    saved.retain(|(guid, values)| {
        let Some(scheme) = ActiveScheme::open(guid) else { return true };
        let mut restored = true;
        for (row, (ac, dc)) in ROWS.iter().zip(values) {
            for (on_ac, original) in [(true, ac), (false, dc)] {
                if let Some(value) = original {
                    restored &= write_and_verify(&scheme, &row.setting, on_ac, *value);
                }
            }
        }
        if active.as_ref().is_some_and(|a| a.0 == scheme.0) {
            restored &= scheme.commit();
        }
        !restored
    });
    let stored = store_saved(&saved);
    stored && saved.is_empty()
}

fn write_and_verify(scheme: &ActiveScheme, setting: &GUID, ac: bool, value: u32) -> bool {
    if scheme.read(setting, ac) == Some(value) {
        return true;
    }
    scheme.write(setting, ac, value) && scheme.read(setting, ac) == Some(value)
}

/// Writes the chosen profiles into the active scheme, or puts the scheme's own values back
/// when neither source has one. Used until the user first chooses a linked profile.
pub fn apply() -> bool {
    let (ac, dc) = (chosen(true), chosen(false));
    if ac.is_none() && dc.is_none() {
        return restore_all(load_saved());
    }
    apply_to_scheme([Some(ac), Some(dc)])
}

pub fn apply_for_source(ac: bool, profile: Profile) -> bool {
    if ac {
        apply_to_scheme([Some(Some(profile)), None])
    } else {
        apply_to_scheme([None, Some(Some(profile))])
    }
}

pub fn active_scheme_id() -> Option<GUID> {
    ActiveScheme::get().map(|scheme| scheme.0)
}

fn apply_to_scheme(profiles: [Option<Option<Profile>>; 2]) -> bool {
    let Some(scheme) = ActiveScheme::get() else { return false };
    let mut saved = load_saved();
    let key = format!("{:?}", scheme.guid());
    let originals = match saved.iter().find(|(g, _)| *g == key) {
        Some((_, values)) => values.clone(),
        None => {
            let values: Vec<_> = ROWS.iter().map(|r| (scheme.read(&r.setting, true), scheme.read(&r.setting, false))).collect();
            if values.iter().all(|(a, d)| a.is_none() && d.is_none()) {
                return false;
            }
            saved.push((key, values.clone()));
            values
        }
    };
    // Persist the originals before any writes, including when converting positional backups.
    if !store_saved(&saved) {
        return false;
    }
    let mut changed = Vec::new();
    let mut ok = [false; 2];
    let mut failed = false;
    for (row, (orig_ac, orig_dc)) in ROWS.iter().zip(&originals) {
        for (index, original) in [orig_ac, orig_dc].into_iter().enumerate() {
            let (Some(profile), Some(original)) = (profiles[index], original) else { continue };
            let ac = index == 0;
            let value = profile.map_or(*original, |p| row.value(p, ac));
            let Some(before) = scheme.read(&row.setting, ac) else {
                failed = true;
                break;
            };
            ok[index] = true;
            if before != value {
                changed.push((&row.setting, ac, before));
                if !write_and_verify(&scheme, &row.setting, ac, value) {
                    failed = true;
                    break;
                }
            }
        }
        if failed {
            break;
        }
    }
    let supported = profiles.into_iter().enumerate().all(|(i, p)| p.is_none() || ok[i]);
    if !failed && supported && (changed.is_empty() || scheme.commit()) {
        return true;
    }
    for (setting, ac, before) in &changed {
        if !write_and_verify(&scheme, setting, *ac, *before) {
            crate::log::error(&format!("Could not roll back processor setting {setting:?}"));
        }
    }
    if !changed.is_empty() && !scheme.commit() {
        crate::log::error("Could not reactivate the power scheme after processor profile rollback");
    }
    false
}

/// Uninstall: put each scheme's own values back. Keep failed entries so it can be retried.
pub fn restore_original() {
    if !restore_all(load_saved()) {
        crate::log::error("Could not restore all original processor settings");
    }
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
