use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// DotNet DateTime epoch (0001-01-01) to Unix epoch (1970-01-01) in 100ns ticks.
const DOTNET_UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000;
const DOTNET_MAX_TICKS: i64 = 3_155_378_975_999_999_999;
const TICKS_PER_SECOND: i64 = 10_000_000;
const SNAPSHOT_FRESHNESS_SECS: i64 = 30;
pub const PANEL_TELEMETRY_COMMAND: u32 = u32::MAX;
/// The open Power tab asks for a live sample this often.
pub const PANEL_TELEMETRY_INTERVAL_MS: u64 = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BatteryMode {
    Disabled,
    Home,
    Office,
    Travel,
}

impl BatteryMode {
    pub fn parse(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("disabled") {
            Some(Self::Disabled)
        } else if value.eq_ignore_ascii_case("home") {
            Some(Self::Home)
        } else if value.eq_ignore_ascii_case("office") {
            Some(Self::Office)
        } else if value.eq_ignore_ascii_case("travel") {
            Some(Self::Travel)
        } else {
            None
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "Disabled",
            Self::Home => "Home",
            Self::Office => "Office",
            Self::Travel => "Travel",
        }
    }

    pub fn thresholds(self) -> (u8, u8) {
        match self {
            Self::Home => (40, 70),
            Self::Office => (70, 90),
            Self::Travel => (95, 100),
            Self::Disabled => (0, 100),
        }
    }

    pub fn from_thresholds(start: Option<i32>, end: Option<i32>) -> Option<Self> {
        match (start, end) {
            (Some(40), Some(70)) => Some(Self::Home),
            (Some(70), Some(90)) => Some(Self::Office),
            (Some(95), Some(100)) => Some(Self::Travel),
            (Some(0), Some(100)) => Some(Self::Disabled),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BacklightLevel {
    Off,
    Low,
    High,
}

impl BacklightLevel {
    pub fn parse(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("off") {
            Some(Self::Off)
        } else if value.eq_ignore_ascii_case("low") {
            Some(Self::Low)
        } else if value.eq_ignore_ascii_case("high") {
            Some(Self::High)
        } else {
            None
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Low => "Low",
            Self::High => "High",
        }
    }

    pub fn from_wmi_byte(value: i32) -> Option<Self> {
        match value {
            0x02 => Some(Self::Off),
            0x03 => Some(Self::Low),
            0x04 => Some(Self::High),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TouchpadHaptics {
    Low = 0,
    Medium = 1,
    High = 2,
}

impl TouchpadHaptics {
    pub fn parse(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("low") {
            Some(Self::Low)
        } else if value.eq_ignore_ascii_case("medium") {
            Some(Self::Medium)
        } else if value.eq_ignore_ascii_case("high") {
            Some(Self::High)
        } else {
            None
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchpadEdgeGesture {
    Brightness = 0x07,
    Volume = 0x08,
}

impl TouchpadEdgeGesture {
    pub fn key_suffix(self) -> &'static str {
        match self {
            Self::Brightness => "Brightness",
            Self::Volume => "Volume",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensorSnapshot {
    pub sampled_ticks: i64,
    pub fan1_rpm: Option<i32>,
    pub fan2_rpm: Option<i32>,
    pub cpu_temp: Option<i32>,
    pub battery_temp: Option<i32>,
    pub keyboard_backlight_mode: Option<i32>,
    pub charge_start: Option<i32>,
    pub charge_end: Option<i32>,
    pub fan1_target_rpm: Option<i32>,
    pub fan2_target_rpm: Option<i32>,
    pub performance_mode: Option<i32>,
}

/// Current time in 100 ns ticks since 0001-01-01 (full precision: samples are 0.5 s apart).
pub fn now_dotnet_ticks() -> i64 {
    let ticks = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| (d.as_nanos() / 100).min(i64::MAX as u128) as i64)
        .unwrap_or(0);
    DOTNET_UNIX_EPOCH_TICKS.saturating_add(ticks)
}

impl SensorSnapshot {
    pub fn is_fresh(&self) -> bool {
        let now = now_dotnet_ticks();
        let age = now.saturating_sub(self.sampled_ticks);
        (0..SNAPSHOT_FRESHNESS_SECS * TICKS_PER_SECOND).contains(&age)
    }

    pub fn serialize(&self, request_id: &str) -> String {
        fn opt(v: Option<i32>) -> String {
            v.map(|n| n.to_string()).unwrap_or_default()
        }
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            request_id,
            self.sampled_ticks,
            opt(self.fan1_rpm),
            opt(self.fan2_rpm),
            opt(self.cpu_temp),
            opt(self.battery_temp),
            opt(self.keyboard_backlight_mode),
            opt(self.charge_start),
            opt(self.charge_end),
            opt(self.fan1_target_rpm),
            opt(self.fan2_target_rpm),
            opt(self.performance_mode),
        )
    }

    pub fn try_parse(value: &str) -> Option<Self> {
        let parts: Vec<&str> = value.split('|').collect();
        if !matches!(parts.len(), 9 | 12) {
            return None;
        }
        let ticks: i64 = parts[1].parse().ok()?;
        if !(0..=DOTNET_MAX_TICKS).contains(&ticks) {
            return None;
        }
        let parse_opt = |s: &str| -> Option<i32> { s.parse::<i32>().ok() };
        let extended = parts.len() == 12;
        Some(Self {
            sampled_ticks: ticks,
            fan1_rpm: parse_opt(parts[2]),
            fan2_rpm: parse_opt(parts[3]),
            cpu_temp: parse_opt(parts[4]),
            battery_temp: parse_opt(parts[5]),
            keyboard_backlight_mode: parse_opt(parts[6]),
            charge_start: parse_opt(parts[7]),
            charge_end: parse_opt(parts[8]),
            fan1_target_rpm: if extended { parse_opt(parts[9]) } else { None },
            fan2_target_rpm: if extended { parse_opt(parts[10]) } else { None },
            performance_mode: if extended { parse_opt(parts[11]) } else { None },
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetrySummary {
    #[serde(default)]
    pub sampled_ticks: Option<i64>,
    pub mode_label: String,
    pub performance_active: bool,
    pub hunter_active: bool,
    pub charge_label: String,
    pub backlight_label: String,
    pub cpu_temp: Option<i32>,
    pub battery_temp: Option<i32>,
    pub fan1_rpm: Option<i32>,
    pub fan1_target: Option<i32>,
    pub fan2_rpm: Option<i32>,
    pub fan2_target: Option<i32>,
    pub power_label: Option<String>,
    /// Battery power in watts: positive while charging, negative while discharging.
    #[serde(default)]
    pub power_watts: Option<f64>,
    /// Battery charge from `GetSystemPowerStatus`, 0–100.
    #[serde(default)]
    pub battery_percent: Option<u8>,
    /// Whether the charger is connected.
    #[serde(default)]
    pub ac_online: Option<bool>,
    /// CPU package power in watts (Windows Energy Meter / RAPL).
    #[serde(default)]
    pub cpu_watts: Option<f64>,
    /// Charger power agreed over USB-C PD (volts × amps of the contract), when one is connected.
    #[serde(default)]
    pub charger_watts: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlItem {
    pub text: String,
    pub help: Option<String>,
    #[serde(default)]
    pub badge: Option<String>,
    pub command: Option<u32>,
    pub selected: bool,
    pub children: Option<Vec<ControlItem>>,
    pub kind: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelState {
    pub status: String,
    pub title: String,
    pub subtitle: String,
    pub refresh: String,
    pub back: String,
    pub telemetry: TelemetrySummary,
    pub items: Vec<ControlItem>,
    /// Interface language code (`ru`, `en`, `zh`) the panel must use for its own strings.
    #[serde(default)]
    pub language: Option<String>,
    /// Look chosen in System → Style: `graphite` (opaque OLED black) or `glass` (translucent).
    #[serde(default)]
    pub appearance: Option<String>,
    /// The result of the last action when it needs words (an error or an explanation): the
    /// panel shows it as a banner instead of a message box that would close the panel.
    #[serde(default)]
    pub notice: Option<String>,
    #[serde(default)]
    pub notice_is_error: bool,
    /// A long job (update check) runs in the background: the panel asks again shortly.
    #[serde(default)]
    pub job_running: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct DriverTransferResult {
    pub succeeded: bool,
    pub package_count: i32,
    pub reboot_required: bool,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_accepts_older_json_without_new_fields() {
        let json = r#"{"modeLabel":"Smart","performanceActive":false,"hunterActive":false,
            "chargeLabel":"40–70%","backlightLabel":"off","cpuTemp":50,"batteryTemp":null,
            "fan1Rpm":null,"fan1Target":null,"fan2Rpm":null,"fan2Target":null,"powerLabel":null}"#;
        let telemetry: TelemetrySummary = serde_json::from_str(json).expect("older payload");
        assert_eq!(telemetry.sampled_ticks, None);
        assert_eq!(telemetry.power_watts, None);
    }

    #[test]
    fn snapshot_round_trip_and_bounds() {
        let snap = SensorSnapshot {
            sampled_ticks: now_dotnet_ticks(),
            fan1_rpm: Some(2200),
            fan2_rpm: Some(1900),
            cpu_temp: Some(54),
            battery_temp: Some(31),
            keyboard_backlight_mode: Some(0x04),
            charge_start: Some(40),
            charge_end: Some(70),
            fan1_target_rpm: Some(2300),
            fan2_target_rpm: None,
            performance_mode: Some(1),
        };
        let s = snap.serialize("req1");
        let parsed = SensorSnapshot::try_parse(&s).expect("valid snapshot");
        assert_eq!(parsed, snap);
        assert!(parsed.is_fresh());

        assert!(SensorSnapshot::try_parse("id|-1|0|0|0|0|0|0|0").is_none());
        assert!(SensorSnapshot::try_parse("id|9999999999999999999|0|0|0|0|0|0|0").is_none());
        assert!(SensorSnapshot::try_parse("id|100|1|2").is_none());
    }
}
