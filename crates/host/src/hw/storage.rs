//! Drivers → Device check → SSD: the NVMe health log (log page 2) of the system disk through
//! `IOCTL_STORAGE_QUERY_PROPERTY` (`StorageDeviceProtocolSpecificProperty`), the same data
//! CrystalDiskInfo shows: wear, spare, temperature, data written, hours and media errors.
//! Read only on demand.

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::IOCTL_STORAGE_QUERY_PROPERTY;
use windows_core::HSTRING;

const PROPERTY_DEVICE: u32 = 0; // StorageDeviceProperty
const PROPERTY_PROTOCOL: u32 = 50; // StorageDeviceProtocolSpecificProperty
const PROTOCOL_NVME: u32 = 3;
const NVME_LOG_PAGE: u32 = 2;
const HEALTH_LOG: u32 = 2;
const SPECIFIC_DATA_SIZE: usize = 40;
const LOG_SIZE: usize = 512;

#[derive(Clone, Debug, PartialEq)]
pub struct SsdHealth {
    pub model: String,
    /// Rated life used, percent (can exceed 100).
    pub used_percent: u8,
    pub spare_percent: u8,
    pub temperature_c: Option<i32>,
    pub written_tb: f64,
    pub power_on_hours: u64,
    pub media_errors: u64,
    /// Any critical warning bit set by the drive.
    pub warning: bool,
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn open(index: u32) -> Option<Handle> {
    let path = HSTRING::from(format!(r"\\.\PhysicalDrive{index}"));
    unsafe {
        // Read only: health queries never need write access to the disk.
        CreateFileW(&path, 0x8000_0000, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None)
            .or_else(|_| CreateFileW(&path, 0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None))
            .ok()
            .map(Handle)
    }
}

fn query(handle: &Handle, input: &[u8], output: &mut [u8]) -> Option<usize> {
    let mut returned = 0u32;
    unsafe {
        DeviceIoControl(
            handle.0,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(input.as_ptr().cast()),
            input.len() as u32,
            Some(output.as_mut_ptr().cast()),
            output.len() as u32,
            Some(&mut returned),
            None,
        )
        .ok()?;
    }
    Some(returned as usize)
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).unwrap_or(0)
}

/// Low 64 bits of a 128-bit little-endian counter (enough for any real drive).
fn u64_at(b: &[u8], at: usize) -> u64 {
    b.get(at..at + 8).map(|s| u64::from_le_bytes(s.try_into().unwrap())).unwrap_or(0)
}

fn c_string(b: &[u8], at: usize) -> String {
    if at == 0 || at >= b.len() {
        return String::new();
    }
    let end = b[at..].iter().position(|&c| c == 0).map_or(b.len(), |e| at + e);
    String::from_utf8_lossy(&b[at..end]).trim().to_string()
}

fn model(handle: &Handle) -> String {
    let mut input = [0u8; 12];
    input[..4].copy_from_slice(&PROPERTY_DEVICE.to_le_bytes());
    let mut out = vec![0u8; 1024];
    let Some(n) = query(handle, &input, &mut out) else { return String::new() };
    out.truncate(n);
    // STORAGE_DEVICE_DESCRIPTOR: VendorIdOffset at 12, ProductIdOffset at 16.
    let vendor = c_string(&out, u32_at(&out, 12) as usize);
    let product = c_string(&out, u32_at(&out, 16) as usize);
    if vendor.is_empty() || product.starts_with(&vendor) { product } else { format!("{vendor} {product}") }
}

/// Parses the 512-byte NVMe SMART / health information log.
pub fn parse_health_log(log: &[u8], model: String) -> Option<SsdHealth> {
    if log.len() < LOG_SIZE {
        return None;
    }
    let kelvin = i32::from(u16::from_le_bytes([log[1], log[2]]));
    // A data unit is 1000 sectors of 512 bytes.
    let written_tb = u64_at(log, 48) as f64 * 512_000.0 / 1e12;
    Some(SsdHealth {
        model,
        used_percent: log[5],
        spare_percent: log[3],
        temperature_c: (kelvin > 0).then_some(kelvin - 273),
        written_tb,
        power_on_hours: u64_at(log, 128),
        media_errors: u64_at(log, 160),
        warning: log[0] != 0,
    })
}

/// Health of the first NVMe disk (normally the system SSD); None for SATA or when not readable.
/// Cached for a minute (it opens the physical disks).
pub fn read() -> Option<SsdHealth> {
    static CACHE: crate::cache::Ttl<Option<SsdHealth>> = crate::cache::Ttl::new(std::time::Duration::from_secs(60));
    CACHE.get(read_for_check)
}

pub(super) fn read_for_check() -> Option<SsdHealth> {
    for index in 0..4 {
        let Some(handle) = open(index) else { continue };
        let mut input = vec![0u8; 8 + SPECIFIC_DATA_SIZE + LOG_SIZE];
        input[..4].copy_from_slice(&PROPERTY_PROTOCOL.to_le_bytes());
        let spec = 8;
        for (i, v) in [PROTOCOL_NVME, NVME_LOG_PAGE, HEALTH_LOG, 0, SPECIFIC_DATA_SIZE as u32, LOG_SIZE as u32].into_iter().enumerate() {
            input[spec + i * 4..spec + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        let mut out = vec![0u8; input.len()];
        let Some(n) = query(&handle, &input, &mut out) else { continue };
        // STORAGE_PROTOCOL_DATA_DESCRIPTOR: Version, Size, then the specific data (offset 8).
        let offset = 8 + u32_at(&out, 8 + 16) as usize;
        let length = u32_at(&out, 8 + 20) as usize;
        if offset < 8 + SPECIFIC_DATA_SIZE || length < LOG_SIZE || offset + LOG_SIZE > n {
            continue;
        }
        return parse_health_log(&out[offset..offset + LOG_SIZE], model(&handle));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_log_fields_are_read_from_their_offsets() {
        let mut log = [0u8; LOG_SIZE];
        log[1..3].copy_from_slice(&313u16.to_le_bytes()); // 40 °C
        log[3] = 100;
        log[5] = 3;
        log[48..56].copy_from_slice(&20_000_000u64.to_le_bytes()); // 10.24 TB
        log[128..136].copy_from_slice(&1234u64.to_le_bytes());
        let h = parse_health_log(&log, "Disk".into()).unwrap();
        assert_eq!((h.temperature_c, h.spare_percent, h.used_percent, h.power_on_hours), (Some(40), 100, 3, 1234));
        assert!((h.written_tb - 10.24).abs() < 0.01);
        assert!(!h.warning && h.media_errors == 0);
        assert_eq!(parse_health_log(&[0; LOG_SIZE], "Disk".into()).unwrap().temperature_c, None);
        assert!(parse_health_log(&log[..100], String::new()).is_none());
    }
}
