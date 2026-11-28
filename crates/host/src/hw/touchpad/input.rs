use super::gestures::{Event, Frame, Gestures, PAD_END};
use super::{Gesture, GestureAction, SUPPORTED_DEVICES, dispatch};
use crate::hw::media_seek;
use std::cell::RefCell;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;
use windows::Win32::Devices::HumanInterfaceDevice::{
    HIDP_CAPS, HIDP_STATUS_SUCCESS, HIDP_VALUE_CAPS, HidP_GetCaps, HidP_GetUsageValue,
    HidP_GetUsagesEx, HidP_GetValueCaps, HidP_Input, PHIDP_PREPARSED_DATA, USAGE_AND_PAGE,
};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::*;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const WM_APP_TOUCHPAD_INPUT: u32 = WM_APP + 10;
const DIGITIZER: u16 = 0x0d;
const TOUCHPAD: u16 = 0x05;
const GENERIC_DESKTOP: u16 = 0x01;
const CONTACT_ID: u16 = 0x51;
const CONTACT_COUNT: u16 = 0x54;
const TIP: u16 = 0x42;
const CONFIDENCE: u16 = 0x47;
const MAX_CONTACTS: usize = 5;
static MAX_SLOTS: AtomicU8 = AtomicU8::new(0);

#[derive(Default)]
struct Input {
    registered: bool,
    actions: u32,
    media: bool,
    devices: Vec<Pad>,
}

thread_local! { static INPUT: RefCell<Input> = RefCell::new(Input::default()); }

pub fn supports(fingers: u8) -> bool {
    MAX_SLOTS.load(Ordering::Acquire) >= fingers
}

pub fn can_seek() -> bool {
    supports(4)
}

pub fn settings_changed(window: isize) {
    unsafe {
        let _ = PostMessageW(
            Some(HWND(window as *mut _)),
            WM_APP_TOUCHPAD_INPUT,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

pub fn synchronize(hwnd: HWND) {
    let actions = Gesture::ALL
        .into_iter()
        .filter(|gesture| {
            gesture.output_command().is_none()
                && GestureAction::current(*gesture) != GestureAction::None
        })
        .fold(0u32, |mask, gesture| mask | (1 << gesture as u8));
    let media = crate::settings::media_seek_enabled();
    INPUT.with(|slot| {
        let mut state = slot.borrow_mut();
        let devices = devices();
        let slots = devices
            .iter()
            .map(|device| device.slots.len())
            .max()
            .unwrap_or_default();
        MAX_SLOTS.store(slots as u8, Ordering::Release);
        let enabled = (actions != 0 && slots > 0) || (media && slots >= 4);
        if state.registered != enabled {
            let request = RAWINPUTDEVICE {
                usUsagePage: DIGITIZER,
                usUsage: TOUCHPAD,
                dwFlags: if enabled {
                    RIDEV_INPUTSINK | RIDEV_DEVNOTIFY
                } else {
                    RIDEV_REMOVE
                },
                hwndTarget: if enabled { hwnd } else { HWND::default() },
            };
            match unsafe { RegisterRawInputDevices(&[request], size_of::<RAWINPUTDEVICE>() as u32) }
            {
                Ok(()) => state.registered = enabled,
                Err(err) => {
                    crate::log::error(&format!(
                        "Could not change touchpad raw input registration: {err}"
                    ));
                    if enabled {
                        crate::menu_builder::post_panel_error(grandhonor_core::locale::t(
                            "Windows не разрешила получать данные тачпада для этих жестов.",
                            "Windows did not allow touchpad input for these gestures.",
                            "Windows 未允许读取这些手势的触控板数据。",
                        ));
                    }
                }
            }
        }
        media_seek::cancel();
        state.actions = if enabled { actions } else { 0 };
        state.media = enabled && media;
        state.devices = if state.registered && enabled {
            devices
        } else {
            Vec::new()
        };
    });
}

pub fn reset() {
    media_seek::cancel();
    INPUT.with(|state| {
        for device in &mut state.borrow_mut().devices {
            device.gestures.abort();
        }
    });
}

pub fn process(hwnd: HWND, lparam: LPARAM) {
    INPUT.with(|slot| {
        let mut state = slot.borrow_mut();
        if !state.registered || state.devices.is_empty() {
            return;
        }
        // The buffer is aligned for RAWINPUTHEADER. Reports from unrelated collections are ignored.
        let mut buffer = [0u64; 128];
        let mut size = size_of_val(&buffer) as u32;
        let bytes = unsafe {
            GetRawInputData(
                HRAWINPUT(lparam.0 as *mut _),
                RID_INPUT,
                Some(buffer.as_mut_ptr().cast()),
                &mut size,
                size_of::<RAWINPUTHEADER>() as u32,
            )
        } as usize;
        const DATA: usize = std::mem::offset_of!(RAWINPUT, data);
        const REPORT: usize = DATA + std::mem::offset_of!(RAWHID, bRawData);
        if !(REPORT..=size_of_val(&buffer)).contains(&bytes) {
            return;
        }
        let header = unsafe { &*buffer.as_ptr().cast::<RAWINPUTHEADER>() };
        if header.dwType != RIM_TYPEHID.0 {
            return;
        }
        let hid = unsafe { buffer.as_ptr().cast::<u8>().add(DATA).cast::<RAWHID>() };
        let (width, count) = unsafe { ((*hid).dwSizeHid as usize, (*hid).dwCount as usize) };
        if width == 0 || count > (bytes - REPORT) / width {
            return;
        }
        let mut actions = state.actions;
        if Gesture::Inward.firmware_enabled() {
            actions &= !(1 << Gesture::Inward as u8);
        }
        let media = state.media;
        let Some(pad) = state
            .devices
            .iter_mut()
            .find(|pad| pad.handle == header.hDevice)
        else {
            return;
        };
        let reports = unsafe {
            std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>().add(REPORT), width * count)
        };
        for report in reports.chunks_exact(width) {
            let Some(frame) = pad.decode(report) else {
                media_seek::cancel();
                pad.gestures.abort();
                continue;
            };
            match pad.gestures.frame(Instant::now(), frame, actions, media) {
                Some(Event::Action(gesture)) => dispatch(gesture, hwnd.0 as isize),
                Some(Event::Seek(delta)) => media_seek::update(delta),
                Some(Event::FinishSeek) => media_seek::finish(),
                None => {}
            }
        }
    });
}

struct Pad {
    handle: HANDLE,
    preparsed: Vec<u32>,
    report_id: u8,
    report_bytes: usize,
    slots: Vec<ContactSlot>,
    gestures: Gestures,
}

struct Axis {
    minimum: i32,
    maximum: i32,
}

impl Axis {
    fn from_caps(caps: &HIDP_VALUE_CAPS) -> Option<Self> {
        (caps.LogicalMin >= 0 && caps.LogicalMin < caps.LogicalMax).then_some(Self {
            minimum: caps.LogicalMin,
            maximum: caps.LogicalMax,
        })
    }

    fn scaled(&self, value: u32) -> Option<i32> {
        let value = i32::try_from(value).ok()?;
        (self.minimum..=self.maximum).contains(&value).then(|| {
            ((i64::from(value - self.minimum) * i64::from(PAD_END))
                / i64::from(self.maximum - self.minimum)) as i32
        })
    }
}

struct ContactSlot {
    link: u16,
    x: Axis,
    y: Axis,
}

impl Pad {
    fn decode(&self, report: &[u8]) -> Option<Frame> {
        if report.len() != self.report_bytes || report.first() != Some(&self.report_id) {
            return None;
        }
        let preparsed = PHIDP_PREPARSED_DATA(self.preparsed.as_ptr() as isize);
        let count = value(DIGITIZER, 0, CONTACT_COUNT, preparsed, report)? as usize;
        if count > self.slots.len() {
            return None;
        }
        if count == 0 {
            return Some(Frame::default());
        }
        let mut frame = Frame::default();
        for slot in &self.slots {
            let buttons = usages(slot.link, preparsed, report);
            if !buttons.has(DIGITIZER, TIP) {
                continue;
            }
            let id = value(DIGITIZER, slot.link, CONTACT_ID, preparsed, report)?;
            let bit = 1u32.checked_shl(id)?;
            if frame.ids & bit != 0 || !buttons.has(DIGITIZER, CONFIDENCE) {
                return None;
            }
            frame.ids |= bit;
            frame.fingers += 1;
            frame.center.x +=
                slot.x
                    .scaled(value(GENERIC_DESKTOP, slot.link, 0x30, preparsed, report)?)?;
            frame.center.y +=
                slot.y
                    .scaled(value(GENERIC_DESKTOP, slot.link, 0x31, preparsed, report)?)?;
            if usize::from(frame.fingers) == count {
                break;
            }
        }
        if frame.fingers > 0 {
            frame.center.x /= i32::from(frame.fingers);
            frame.center.y /= i32::from(frame.fingers);
            frame.button = usages(0, preparsed, report).has(0x09, 1);
        }
        Some(frame)
    }
}

struct Usages {
    list: [USAGE_AND_PAGE; 32],
    count: usize,
}

impl Usages {
    fn has(&self, page: u16, usage: u16) -> bool {
        self.list[..self.count]
            .iter()
            .any(|item| item.UsagePage == page && item.Usage == usage)
    }
}

fn usages(link: u16, preparsed: PHIDP_PREPARSED_DATA, report: &[u8]) -> Usages {
    let mut list = [USAGE_AND_PAGE::default(); 32];
    let mut count = list.len() as u32;
    let status = unsafe {
        HidP_GetUsagesEx(
            HidP_Input,
            Some(link),
            list.as_mut_ptr(),
            &mut count,
            preparsed,
            report,
        )
    };
    Usages {
        list,
        count: if status == HIDP_STATUS_SUCCESS {
            (count as usize).min(list.len())
        } else {
            0
        },
    }
}

fn value(
    page: u16,
    link: u16,
    usage: u16,
    preparsed: PHIDP_PREPARSED_DATA,
    report: &[u8],
) -> Option<u32> {
    let mut number = 0;
    let status = unsafe {
        HidP_GetUsageValue(
            HidP_Input,
            page,
            Some(link),
            usage,
            &mut number,
            preparsed,
            report,
        )
    };
    (status == HIDP_STATUS_SUCCESS).then_some(number)
}

fn devices() -> Vec<Pad> {
    let size = size_of::<RAWINPUTDEVICELIST>() as u32;
    let mut count = 0;
    if unsafe { GetRawInputDeviceList(None, &mut count, size) } == u32::MAX
        || !(1..=1024).contains(&count)
    {
        return Vec::new();
    }
    let mut list = vec![RAWINPUTDEVICELIST::default(); count as usize];
    let count = unsafe { GetRawInputDeviceList(Some(list.as_mut_ptr()), &mut count, size) };
    if count == u32::MAX {
        return Vec::new();
    }
    list.into_iter()
        .take(count as usize)
        .filter(|item| item.dwType == RIM_TYPEHID)
        .filter_map(|item| describe(item.hDevice))
        .collect()
}

fn describe(handle: HANDLE) -> Option<Pad> {
    let mut info = RID_DEVICE_INFO {
        cbSize: size_of::<RID_DEVICE_INFO>() as u32,
        ..Default::default()
    };
    let mut size = info.cbSize;
    let read = unsafe {
        GetRawInputDeviceInfoW(
            Some(handle),
            RIDI_DEVICEINFO,
            Some((&mut info as *mut RID_DEVICE_INFO).cast()),
            &mut size,
        )
    };
    if read == u32::MAX || info.dwType != RIM_TYPEHID {
        return None;
    }
    let hid = unsafe { info.Anonymous.hid };
    let identity = (
        u16::try_from(hid.dwVendorId).ok()?,
        u16::try_from(hid.dwProductId).ok()?,
    );
    if (hid.usUsagePage, hid.usUsage) != (DIGITIZER, TOUCHPAD)
        || !SUPPORTED_DEVICES.contains(&identity)
    {
        return None;
    }
    size = 0;
    let result =
        unsafe { GetRawInputDeviceInfoW(Some(handle), RIDI_PREPARSEDDATA, None, &mut size) };
    if result == u32::MAX || !(1..=16384).contains(&size) {
        return None;
    }
    let mut preparsed = vec![0u32; (size as usize).div_ceil(4)];
    if unsafe {
        GetRawInputDeviceInfoW(
            Some(handle),
            RIDI_PREPARSEDDATA,
            Some(preparsed.as_mut_ptr().cast()),
            &mut size,
        )
    } == u32::MAX
    {
        return None;
    }
    let data = PHIDP_PREPARSED_DATA(preparsed.as_ptr() as isize);
    let mut caps = HIDP_CAPS::default();
    if unsafe { HidP_GetCaps(data, &mut caps) } != HIDP_STATUS_SUCCESS
        || caps.NumberInputValueCaps > 256
    {
        return None;
    }
    let mut count = caps.NumberInputValueCaps;
    let mut values = vec![HIDP_VALUE_CAPS::default(); usize::from(count)];
    if unsafe { HidP_GetValueCaps(HidP_Input, values.as_mut_ptr(), &mut count, data) }
        != HIDP_STATUS_SUCCESS
    {
        return None;
    }
    values.truncate(usize::from(count));
    let matches_usage = |caps: &HIDP_VALUE_CAPS, page, usage| {
        !caps.IsRange && caps.UsagePage == page && unsafe { caps.Anonymous.NotRange.Usage == usage }
    };
    let report_id = values
        .iter()
        .find(|caps| caps.LinkCollection == 0 && matches_usage(caps, DIGITIZER, CONTACT_COUNT))?
        .ReportID;
    let slots: Vec<_> = values
        .iter()
        .filter(|caps| {
            caps.ReportID == report_id
                && caps.LinkCollection != 0
                && matches_usage(caps, DIGITIZER, CONTACT_ID)
        })
        .take(MAX_CONTACTS)
        .filter_map(|id| {
            let axis = |usage| {
                values
                    .iter()
                    .find(|caps| {
                        caps.ReportID == report_id
                            && caps.LinkCollection == id.LinkCollection
                            && matches_usage(caps, GENERIC_DESKTOP, usage)
                    })
                    .and_then(Axis::from_caps)
            };
            Some(ContactSlot {
                link: id.LinkCollection,
                x: axis(0x30)?,
                y: axis(0x31)?,
            })
        })
        .collect();
    (!slots.is_empty()).then_some(Pad {
        handle,
        preparsed,
        report_id,
        report_bytes: usize::from(caps.InputReportByteLength),
        slots,
        gestures: Gestures::default(),
    })
}
