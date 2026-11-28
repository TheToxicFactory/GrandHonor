//! Explicit offline device diagnostics and opt-in interactive checks.

mod network;

use super::{com::ComGuard, devices, storage};
use grandhonor_core::locale::t;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SERVICE_DOES_NOT_EXIST};
use windows::Win32::Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator, eCapture, eConsole};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_HANDLE,
    SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Accessibility::FILTERKEYS;
use windows::Win32::UI::WindowsAndMessaging::{
    FKF_FILTERKEYSON, MB_OK, SPI_GETFILTERKEYS, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    SystemParametersInfoW,
};
use windows_core::{GUID, PCWSTR, w};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Passed,
    Problem,
    Unavailable,
    Unsupported,
    Interactive,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Passed => t("Пройдено", "Passed", "通过"),
            Self::Problem => t("Найдена проблема", "Problem found", "发现问题"),
            Self::Unavailable => t("Не удалось проверить", "Could not check", "无法检查"),
            Self::Unsupported => t("Не поддерживается", "Unsupported", "不支持"),
            Self::Interactive => t("Требует участия", "Needs your input", "需要手动确认"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub status: Status,
    pub note: Option<String>,
}

impl Outcome {
    fn new(status: Status) -> Self {
        Self { status, note: None }
    }

    fn detail(status: Status, note: impl Into<String>) -> Self {
        Self {
            status,
            note: Some(note.into()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Group {
    Audio,
    Fingerprint,
    WiFi,
    Bluetooth,
    Processor,
    Memory,
    Graphics,
    Storage,
    Keyboard,
    Updates,
    Camera,
    Microphone,
    Touchpad,
}

impl Group {
    pub const ALL: [Self; 13] = [
        Self::Audio,
        Self::Fingerprint,
        Self::WiFi,
        Self::Bluetooth,
        Self::Processor,
        Self::Memory,
        Self::Graphics,
        Self::Storage,
        Self::Keyboard,
        Self::Updates,
        Self::Camera,
        Self::Microphone,
        Self::Touchpad,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Audio => t("Звук", "Audio", "音频"),
            Self::Fingerprint => t("Отпечаток пальца", "Fingerprint", "指纹"),
            Self::WiFi => t("Wi-Fi", "Wi-Fi", "Wi-Fi"),
            Self::Bluetooth => t("Bluetooth", "Bluetooth", "蓝牙"),
            Self::Processor => t("Процессор", "Processor", "处理器"),
            Self::Memory => t("Память", "Memory", "内存"),
            Self::Graphics => t("Графика", "Graphics", "显卡"),
            Self::Storage => t("Накопитель", "Storage", "存储"),
            Self::Keyboard => t("Клавиатура", "Keyboard", "键盘"),
            Self::Updates => t("Обновления Windows", "Windows Update", "Windows 更新"),
            Self::Camera => t("Камера", "Camera", "摄像头"),
            Self::Microphone => t("Микрофон", "Microphone", "麦克风"),
            Self::Touchpad => t("Тачпад", "Touchpad", "触控板"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Manual {
    Sound,
    Camera,
    Microphone,
    Touchpad,
}

impl Manual {
    pub fn key(self) -> &'static str {
        match self {
            Self::Sound => "audio.play",
            Self::Camera => "camera.picture",
            Self::Microphone => "microphone.signal",
            Self::Touchpad => "touchpad.gesture",
        }
    }

    pub fn prompt(self) -> &'static str {
        match self {
            Self::Sound => t(
                "Было слышно системный звук?",
                "Did you hear the system sound?",
                "听到了系统提示音吗？",
            ),
            Self::Camera => t(
                "В приложении «Камера» есть изображение?",
                "Does the Camera app show an image?",
                "相机应用中有画面吗？",
            ),
            Self::Microphone => t(
                "В параметрах звука виден сигнал микрофона?",
                "Does Sound settings show a microphone signal?",
                "声音设置中有麦克风信号吗？",
            ),
            Self::Touchpad => t(
                "Тачпад реагирует на движение и жесты?",
                "Does the touchpad respond to movement and gestures?",
                "触控板能响应移动和手势吗？",
            ),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Check {
    pub group: Group,
    pub key: &'static str,
    pub title: &'static str,
    pub outcome: Outcome,
    pub manual: Option<Manual>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Scan,
    Internet,
}

#[derive(Clone, Default)]
pub struct Report {
    pub active: Option<Operation>,
    pub cancelled: bool,
    pub finished_at: Option<(u16, u16, u16)>,
    pub pnp: Option<Status>,
    pub problems: Vec<devices::Device>,
    pub ssd: Option<storage::SsdHealth>,
    pub checks: Vec<Check>,
    pub manual: Option<Manual>,
    pub internet: Option<Outcome>,
}

pub const TOTAL_CHECKS: usize = 39;
const INTERNET_KEY: &str = "wifi.internet";
static REPORT: Mutex<Report> = Mutex::new(Report {
    active: None,
    cancelled: false,
    finished_at: None,
    pnp: None,
    problems: Vec::new(),
    ssd: None,
    checks: Vec::new(),
    manual: None,
    internet: None,
});
static CANCEL: AtomicBool = AtomicBool::new(false);

pub fn snapshot() -> Report {
    REPORT
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone()
}

pub fn busy() -> bool {
    REPORT
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .active
        .is_some()
}

pub fn cancel() {
    let mut report = REPORT.lock().unwrap_or_else(|poison| poison.into_inner());
    if report.active == Some(Operation::Scan) {
        CANCEL.store(true, Ordering::Release);
        report.cancelled = true;
    }
}

fn cancelled() -> bool {
    CANCEL.load(Ordering::Acquire)
}

pub fn begin(operation: Operation) -> bool {
    {
        let mut report = REPORT.lock().unwrap_or_else(|poison| poison.into_inner());
        if report.active.is_some() {
            return false;
        }
        if operation == Operation::Scan {
            CANCEL.store(false, Ordering::Release);
            *report = Report::default();
        } else {
            report.internet = None;
        }
        report.active = Some(operation);
    }

    if std::thread::Builder::new()
        .name("device-check".into())
        .spawn(move || {
            let _job = JobGuard(operation);
            match operation {
                Operation::Scan => scan(),
                Operation::Internet => check_internet(),
            }
        })
        .is_err()
    {
        REPORT
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .active = None;
        return false;
    }
    true
}

struct JobGuard(Operation);

impl Drop for JobGuard {
    fn drop(&mut self) {
        let mut report = REPORT.lock().unwrap_or_else(|poison| poison.into_inner());
        if self.0 == Operation::Scan {
            report.cancelled = cancelled();
            let time = unsafe { GetLocalTime() };
            report.finished_at = Some((time.wHour, time.wMinute, time.wSecond));
        }
        report.active = None;
    }
}

fn push(
    group: Group,
    key: &'static str,
    title: &'static str,
    outcome: Outcome,
    manual: Option<Manual>,
) {
    if !cancelled() {
        REPORT
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .checks
            .push(Check {
                group,
                key,
                title,
                outcome,
                manual,
            });
    }
}

fn scan() {
    let _com = ComGuard::mta();
    let inventory = devices::inventory(cancelled);
    let inventory = match inventory {
        Ok(devices) => Some(devices),
        Err(error) => {
            crate::log::error(&format!(
                "Device check: SetupAPI enumeration failed: {error}"
            ));
            None
        }
    };
    if cancelled() {
        return;
    }
    {
        let mut report = REPORT.lock().unwrap_or_else(|poison| poison.into_inner());
        report.pnp = Some(inventory.as_ref().map_or(Status::Unavailable, |devices| {
            report.problems = devices
                .iter()
                .filter(|device| device.has_problem())
                .map(|device| device.device.clone())
                .collect();
            if devices.is_empty() || devices.iter().any(|device| !device.status_known) {
                Status::Unavailable
            } else if report.problems.is_empty() {
                Status::Passed
            } else {
                Status::Problem
            }
        }));
    }
    let inventory = inventory.as_deref();
    let audio = class_devices(inventory, &[MEDIA_CLASS]);
    device_pair(Group::Audio, "audio", audio.as_deref());
    push(
        Group::Audio,
        "audio.service",
        t("Службы звука", "Audio services", "音频服务"),
        services(&[w!("Audiosrv"), w!("AudioEndpointBuilder")]),
        None,
    );
    let sound = if audio.as_ref().is_some_and(Vec::is_empty) {
        Status::Unsupported
    } else {
        Status::Interactive
    };
    push(
        Group::Audio,
        Manual::Sound.key(),
        t("Воспроизведение", "Playback", "播放"),
        Outcome::new(sound),
        Some(Manual::Sound),
    );
    device_pair(
        Group::Fingerprint,
        "fingerprint",
        class_devices(inventory, &[BIOMETRIC_CLASS]).as_deref(),
    );

    if cancelled() {
        return;
    }
    network::scan(inventory);
    if cancelled() {
        return;
    }
    device_pair(
        Group::Bluetooth,
        "bluetooth",
        class_devices(inventory, &[devices::GUID_DEVCLASS_BLUETOOTH]).as_deref(),
    );
    processor_checks();
    memory_checks();
    if cancelled() {
        return;
    }
    device_pair(
        Group::Graphics,
        "graphics",
        class_devices(inventory, &[DISPLAY_CLASS]).as_deref(),
    );

    let ssd = storage::read_for_check();
    let outcome = match ssd.as_ref() {
        Some(s) if s.warning || s.media_errors != 0 || s.used_percent >= 90 => Status::Problem,
        Some(_) => Status::Passed,
        None => Status::Unavailable,
    };
    REPORT
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .ssd = ssd;
    push(
        Group::Storage,
        "storage.smart",
        "S.M.A.R.T. (NVMe)",
        Outcome::new(outcome),
        None,
    );
    push(
        Group::Keyboard,
        "keyboard.filter",
        t("Фильтрация ввода", "Filter Keys", "筛选键"),
        filter_keys(),
        None,
    );
    push(
        Group::Updates,
        "updates.auto",
        t("Автоматические обновления", "Automatic updates", "自动更新"),
        automatic_updates(),
        None,
    );

    let camera = device_status(class_devices(inventory, &[CAMERA_CLASS, IMAGE_CLASS]).as_deref());
    push(
        Group::Camera,
        Manual::Camera.key(),
        t("Изображение", "Camera image", "相机画面"),
        manual_device(camera),
        Some(Manual::Camera),
    );
    push(
        Group::Microphone,
        Manual::Microphone.key(),
        t("Уровень сигнала", "Microphone signal", "麦克风信号"),
        manual_device(microphone_status()),
        Some(Manual::Microphone),
    );
    push(
        Group::Touchpad,
        Manual::Touchpad.key(),
        t("Движение и жесты", "Movement and gestures", "移动和手势"),
        Outcome::new(Status::Interactive),
        Some(Manual::Touchpad),
    );
}

const MEDIA_CLASS: GUID = GUID::from_u128(0x4d36e96c_e325_11ce_bfc1_08002be10318);
const BIOMETRIC_CLASS: GUID = GUID::from_u128(0x53d29ef7_377c_4d14_864b_eb3a85769359);
const DISPLAY_CLASS: GUID = GUID::from_u128(0x4d36e968_e325_11ce_bfc1_08002be10318);
const CAMERA_CLASS: GUID = GUID::from_u128(0xca3e7ab9_b4c3_4ae6_8251_579ef933890f);
const IMAGE_CLASS: GUID = GUID::from_u128(0x6bdd1fc6_810f_11d0_bec7_08002be2092f);

fn class_devices<'a>(
    inventory: Option<&'a [devices::PresentDevice]>,
    classes: &[GUID],
) -> Option<Vec<&'a devices::PresentDevice>> {
    inventory.map(|inventory| {
        inventory
            .iter()
            .filter(|device| classes.contains(&device.class))
            .collect()
    })
}

fn device_driver(devices: Option<&[&devices::PresentDevice]>) -> Outcome {
    let status = match devices {
        None => Status::Unavailable,
        Some([]) => Status::Unsupported,
        Some(devices) if devices.iter().any(|device| device.device.problem == 28) => {
            Status::Problem
        }
        Some(devices) if devices.iter().all(|device| device.has_driver) => Status::Passed,
        Some(_) => Status::Unavailable,
    };
    Outcome::new(status)
}

fn device_status(devices: Option<&[&devices::PresentDevice]>) -> Outcome {
    let status = match devices {
        None => Status::Unavailable,
        Some([]) => Status::Unsupported,
        Some(devices) if devices.iter().any(|device| device.has_problem()) => Status::Problem,
        Some(devices) if devices.iter().any(|device| !device.status_known) => Status::Unavailable,
        Some(_) => Status::Passed,
    };
    Outcome::new(status)
}

fn device_pair(group: Group, key: &str, devices: Option<&[&devices::PresentDevice]>) {
    let (driver_key, status_key) = match key {
        "audio" => ("audio.driver", "audio.status"),
        "fingerprint" => ("fingerprint.driver", "fingerprint.status"),
        "bluetooth" => ("bluetooth.driver", "bluetooth.status"),
        _ => ("graphics.driver", "graphics.status"),
    };
    push(
        group,
        driver_key,
        t("Драйвер", "Driver", "驱动"),
        device_driver(devices),
        None,
    );
    push(
        group,
        status_key,
        t("Состояние устройства", "Device status", "设备状态"),
        device_status(devices),
        None,
    );
}

fn manual_device(mut outcome: Outcome) -> Outcome {
    if outcome.status == Status::Passed {
        outcome.status = Status::Interactive;
    }
    outcome
}

struct Service(SC_HANDLE);

impl Drop for Service {
    fn drop(&mut self) {
        let _ = unsafe { CloseServiceHandle(self.0) };
    }
}

fn services(names: &[PCWSTR]) -> Outcome {
    let result = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) };
    let Ok(manager) = result.map(Service) else {
        return Outcome::new(Status::Unavailable);
    };
    for name in names {
        let service = match unsafe { OpenServiceW(manager.0, *name, SERVICE_QUERY_STATUS) } {
            Ok(handle) => Service(handle),
            Err(error) if error.code() == ERROR_SERVICE_DOES_NOT_EXIST.to_hresult() => {
                return Outcome::new(Status::Unsupported);
            }
            Err(_) => return Outcome::new(Status::Unavailable),
        };
        let mut state = SERVICE_STATUS::default();
        if unsafe { QueryServiceStatus(service.0, &mut state) }.is_err() {
            return Outcome::new(Status::Unavailable);
        }
        if state.dwCurrentState != SERVICE_RUNNING {
            return Outcome::detail(
                Status::Problem,
                t("Служба остановлена", "Service stopped", "服务已停止"),
            );
        }
    }
    Outcome::new(Status::Passed)
}

fn processor_checks() {
    use std::arch::is_x86_feature_detected;
    for (key, title, supported) in [
        ("cpu.sse", "SSE", is_x86_feature_detected!("sse")),
        ("cpu.sse2", "SSE2", is_x86_feature_detected!("sse2")),
        ("cpu.sse3", "SSE3", is_x86_feature_detected!("sse3")),
        ("cpu.ssse3", "SSSE3", is_x86_feature_detected!("ssse3")),
        ("cpu.sse41", "SSE4.1", is_x86_feature_detected!("sse4.1")),
        ("cpu.sse42", "SSE4.2", is_x86_feature_detected!("sse4.2")),
    ] {
        push(
            Group::Processor,
            key,
            title,
            Outcome::new(if supported {
                Status::Passed
            } else {
                Status::Unsupported
            }),
            None,
        );
    }
}

fn memory_checks() {
    const WORDS: usize = 1 << 20; // 8 MiB of process memory, never physical RAM or user data.
    let patterns: [(_, _, &[fn(u64) -> u64]); 5] = [
        (
            "memory.quick",
            t(
                "Быстрая запись и чтение",
                "Quick read and write",
                "快速读写",
            ),
            &[|_| 0xA5A5_A5A5_A5A5_A5A5],
        ),
        (
            "memory.complement",
            t("Дополняющие шаблоны", "Complementary patterns", "互补模式"),
            &[|i| i, |i| !i],
        ),
        (
            "memory.bit",
            t(
                "Побитовая запись и чтение",
                "Bit-by-bit read and write",
                "逐位读写",
            ),
            &[
                |i| 1u64.rotate_left(i as u32),
                |i| !1u64.rotate_left(i as u32),
            ],
        ),
        (
            "memory.random",
            t(
                "Псевдослучайный шаблон",
                "Pseudorandom pattern",
                "伪随机模式",
            ),
            &[mixed_word],
        ),
        (
            "memory.arithmetic",
            t("Арифметический шаблон", "Arithmetic pattern", "算术模式"),
            &[|i| i.wrapping_mul(0x0101_0101_0101_0101).wrapping_add(42)],
        ),
    ];
    let mut words = Vec::new();
    let allocated = words.try_reserve_exact(WORDS).is_ok();
    if allocated {
        words.resize(WORDS, 0);
    }
    for (key, title, patterns) in patterns {
        if cancelled() {
            return;
        }
        let outcome = if allocated {
            let status = if patterns
                .iter()
                .all(|pattern| memory_pattern(&mut words, *pattern))
            {
                Status::Passed
            } else {
                Status::Problem
            };
            Outcome::detail(
                status,
                t(
                    "Проверено 8 МиБ выделенной памяти",
                    "Checked 8 MiB of allocated memory",
                    "检查了 8 MiB 已分配内存",
                ),
            )
        } else {
            Outcome::new(Status::Unavailable)
        };
        push(Group::Memory, key, title, outcome, None);
    }
}

fn mixed_word(mut word: u64) -> u64 {
    word = (word ^ (word >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    word = (word ^ (word >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    word ^ (word >> 31)
}

fn memory_pattern(words: &mut [u64], pattern: fn(u64) -> u64) -> bool {
    const CHUNK: usize = 4096;
    for (n, chunk) in words.chunks_mut(CHUNK).enumerate() {
        if cancelled() {
            return false;
        }
        for (i, word) in chunk.iter_mut().enumerate() {
            *word = pattern((n * CHUNK + i) as u64);
        }
    }
    let words = std::hint::black_box(words);
    for (n, chunk) in words.chunks(CHUNK).enumerate() {
        if cancelled()
            || !chunk
                .iter()
                .enumerate()
                .all(|(i, &word)| word == pattern((n * CHUNK + i) as u64))
        {
            return false;
        }
    }
    true
}

fn filter_keys() -> Outcome {
    let mut keys = FILTERKEYS {
        cbSize: size_of::<FILTERKEYS>() as u32,
        ..Default::default()
    };
    let read = unsafe {
        SystemParametersInfoW(
            SPI_GETFILTERKEYS,
            keys.cbSize,
            Some((&mut keys as *mut FILTERKEYS).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    if read.is_err() {
        return Outcome::new(Status::Unavailable);
    }
    if keys.dwFlags & FKF_FILTERKEYSON != 0 {
        Outcome::detail(
            Status::Passed,
            t(
                "Windows игнорирует часть коротких или повторных нажатий",
                "Windows ignores some short or repeated presses",
                "Windows 会忽略部分短按或重复按键",
            ),
        )
    } else {
        Outcome::detail(
            Status::Passed,
            t("Фильтрация выключена", "Filter Keys is off", "筛选键已关闭"),
        )
    }
}

fn automatic_updates() -> Outcome {
    match network::registry_dword(
        windows::Win32::System::Registry::HKEY_LOCAL_MACHINE,
        w!("SOFTWARE\\Policies\\Microsoft\\Windows\\WindowsUpdate\\AU"),
        w!("NoAutoUpdate"),
    ) {
        Ok(Some(1)) => Outcome::detail(
            Status::Passed,
            t(
                "Автообновления отключены политикой",
                "Automatic updates are disabled by policy",
                "策略已关闭自动更新",
            ),
        ),
        Ok(Some(0)) => Outcome::detail(
            Status::Passed,
            t(
                "Политика разрешает автообновления",
                "Policy allows automatic updates",
                "策略允许自动更新",
            ),
        ),
        Ok(None) => Outcome::detail(
            Status::Passed,
            t(
                "Политика NoAutoUpdate не задана",
                "NoAutoUpdate policy is not configured",
                "未配置 NoAutoUpdate 策略",
            ),
        ),
        _ => Outcome::new(Status::Unavailable),
    }
}

fn microphone_status() -> Outcome {
    let enumerator: windows_core::Result<IMMDeviceEnumerator> =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER) };
    let Ok(enumerator) = enumerator else {
        return Outcome::new(Status::Unavailable);
    };
    match unsafe { enumerator.GetDefaultAudioEndpoint(eCapture, eConsole) } {
        Ok(_) => Outcome::new(Status::Passed),
        Err(error)
            if error.code() == ERROR_FILE_NOT_FOUND.to_hresult()
                || error.code().0 == 0x80070490u32 as i32 =>
        {
            Outcome::detail(
                Status::Unavailable,
                t(
                    "В Windows не выбран активный микрофон",
                    "Windows has no active default microphone",
                    "Windows 没有活动的默认麦克风",
                ),
            )
        }
        Err(_) => Outcome::new(Status::Unavailable),
    }
}

pub fn start_manual(check: Manual) {
    {
        let mut report = REPORT.lock().unwrap_or_else(|poison| poison.into_inner());
        if report.active.is_some()
            || !report
                .checks
                .iter()
                .any(|row| row.manual == Some(check) && row.outcome.status == Status::Interactive)
        {
            return;
        }
        report.manual = Some(check);
    }
    match check {
        Manual::Sound => {
            if unsafe { MessageBeep(MB_OK) }.is_err() {
                let mut report = REPORT.lock().unwrap_or_else(|poison| poison.into_inner());
                report.manual = None;
                if let Some(row) = report.checks.iter_mut().find(|row| row.key == check.key()) {
                    row.outcome = Outcome::new(Status::Unavailable);
                }
            }
        }
        Manual::Camera => crate::unelevated::open("microsoft.windows.camera:"),
        Manual::Microphone => crate::unelevated::open("ms-settings:sound"),
        Manual::Touchpad => crate::unelevated::open("ms-settings:devices-touchpad"),
    }
}

pub fn confirm(check: Manual, result: bool) {
    let mut report = REPORT.lock().unwrap_or_else(|poison| poison.into_inner());
    if report.manual != Some(check) {
        return;
    }
    report.manual = None;
    if let Some(row) = report.checks.iter_mut().find(|row| row.key == check.key()) {
        row.outcome = Outcome::detail(
            if result {
                Status::Passed
            } else {
                Status::Problem
            },
            t("Ваш ответ", "Your answer", "您的回答"),
        );
    }
}

fn check_internet() {
    let outcome = match crate::drivers::http::probe_internet() {
        Ok(()) => Outcome::detail(
            Status::Passed,
            t(
                "Microsoft ответил по HTTPS",
                "Microsoft responded over HTTPS",
                "Microsoft 通过 HTTPS 响应",
            ),
        ),
        Err(error) => {
            crate::log::info(&format!("Internet check: {error}"));
            Outcome::detail(
                Status::Unavailable,
                t(
                    "Не удалось связаться с Microsoft напрямую по HTTPS",
                    "Could not reach Microsoft directly over HTTPS",
                    "无法通过 HTTPS 直接连接 Microsoft",
                ),
            )
        }
    };
    let mut report = REPORT.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(row) = report.checks.iter_mut().find(|row| row.key == INTERNET_KEY) {
        row.outcome = outcome.clone();
    }
    report.internet = Some(outcome);
}
