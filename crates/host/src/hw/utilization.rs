use std::cell::RefCell;
use std::ffi::c_void;
use std::time::{Duration, Instant};
use windows::Wdk::Graphics::Direct3D::{
    D3DKMT_ADAPTERINFO, D3DKMT_CLOSEADAPTER, D3DKMT_DRIVER_DESCRIPTION, D3DKMT_ENUMADAPTERS_FILTER,
    D3DKMT_ENUMADAPTERS3, D3DKMT_QUERYADAPTERINFO, D3DKMTCloseAdapter, D3DKMTQueryAdapterInfo,
    KMTQAITYPE_ADAPTERTYPE, KMTQAITYPE_DRIVER_DESCRIPTION, KMTQUERYADAPTERINFOTYPE,
};
use windows::Win32::Foundation::{FILETIME, NTSTATUS};
use windows::Win32::Graphics::DXCore::{
    DXCORE_ADAPTER_ATTRIBUTE_D3D11_GRAPHICS, DXCoreAdapterProperty, DXCoreAdapterState,
    DXCoreCreateAdapterFactory, IDXCoreAdapter, IDXCoreAdapter_Impl, IDXCoreAdapter_Vtbl,
    IDXCoreAdapterFactory, IDXCoreAdapterFactory_Impl, IDXCoreAdapterFactory_Vtbl,
    IDXCoreAdapterList, IsHardware,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;
use windows_core::{GUID, HRESULT, Interface, interface};

// These extensions and argument values are from the current Windows SDK dxcore_interface.h.
#[interface("a0783366-cfa3-43be-9d79-55b2da97c63c")]
unsafe trait IDXCoreAdapter1: IDXCoreAdapter {
    fn get_property_with_input(
        &self,
        property: DXCoreAdapterProperty,
        input_size: usize,
        input: *const c_void,
        output_size: usize,
        output: *mut c_void,
    ) -> HRESULT;
}

#[interface("d5682e19-6d21-401c-827a-9a51a4ea35d7")]
unsafe trait IDXCoreAdapterFactory1: IDXCoreAdapterFactory {
    fn create_adapter_list_by_workload(
        &self,
        workload: u32,
        runtime: u32,
        hardware: u32,
        iid: *const GUID,
        result: *mut *mut c_void,
    ) -> HRESULT;
}

const ENGINE_COUNT: DXCoreAdapterProperty = DXCoreAdapterProperty(16);
const PHYSICAL_COUNT: DXCoreAdapterProperty = DXCoreAdapterProperty(15);
const RUNNING_TIME: DXCoreAdapterState = DXCoreAdapterState(4);

#[repr(C)]
#[derive(Clone, Copy)]
struct EngineIndex {
    physical: u32,
    engine: u32,
}

#[repr(C)]
struct EngineQueryInput {
    index: EngineIndex,
    process_id: u32,
}

#[repr(C)]
#[derive(Default)]
struct EngineQueryOutput {
    running_time: u64,
    process_query_succeeded: bool,
}

struct Engine {
    index: EngineIndex,
    previous: Option<(Instant, u64)>,
}

struct Accelerator {
    adapter: IDXCoreAdapter,
    engines: Vec<Engine>,
}

impl Accelerator {
    fn new(adapter: IDXCoreAdapter) -> Option<Self> {
        unsafe {
            let mut hardware = false;
            adapter
                .GetProperty(
                    IsHardware,
                    size_of::<bool>(),
                    (&mut hardware as *mut bool).cast(),
                )
                .ok()?;
            if !hardware || !adapter.IsQueryStateSupported(RUNNING_TIME) {
                return None;
            }
            let extended: IDXCoreAdapter1 = adapter.cast().ok()?;
            let mut count = 0u32;
            adapter
                .GetProperty(
                    PHYSICAL_COUNT,
                    size_of::<u32>(),
                    (&mut count as *mut u32).cast(),
                )
                .ok()?;
            let mut engines = Vec::new();
            for physical in 0..count.min(16) {
                let mut engine_count = 0;
                if extended
                    .get_property_with_input(
                        ENGINE_COUNT,
                        size_of::<u32>(),
                        (&physical as *const u32).cast(),
                        size_of::<u32>(),
                        (&mut engine_count as *mut u32).cast(),
                    )
                    .is_ok()
                {
                    engines.extend((0..engine_count.min(128)).map(|engine| Engine {
                        index: EngineIndex { physical, engine },
                        previous: None,
                    }));
                }
            }
            (!engines.is_empty()).then_some(Self { adapter, engines })
        }
    }

    fn busy_engine(&mut self) -> Option<u8> {
        self.engines
            .iter_mut()
            .filter_map(|engine| {
                let input = EngineQueryInput {
                    index: engine.index,
                    process_id: 0,
                };
                let mut output = EngineQueryOutput::default();
                let time = unsafe {
                    self.adapter.QueryState(
                        RUNNING_TIME,
                        size_of::<EngineQueryInput>(),
                        Some((&input as *const EngineQueryInput).cast()),
                        size_of::<EngineQueryOutput>(),
                        (&mut output as *mut EngineQueryOutput).cast(),
                    )
                };
                let ticks = output.running_time;
                let now = Instant::now();
                let previous =
                    std::mem::replace(&mut engine.previous, time.is_ok().then_some((now, ticks)));
                time.ok()?;
                let (before, previous) = previous?;
                let elapsed = now.duration_since(before);
                if elapsed > Duration::from_secs(3) {
                    return None;
                }
                percent(ticks.checked_sub(previous)? as u128, elapsed.as_micros())
            })
            .max()
    }
}

fn enumerate(list: IDXCoreAdapterList) -> Vec<Accelerator> {
    unsafe {
        (0..list.GetAdapterCount().min(16))
            .filter_map(|i| list.GetAdapter::<IDXCoreAdapter>(i).ok())
            .filter_map(Accelerator::new)
            .collect()
    }
}

// Some compute-only drivers support DXCore state queries by LUID but are omitted
// from the DXCore workload list. Confirm the driver's NPU name before using one.
fn compute_only_npus(factory: &IDXCoreAdapterFactory) -> Option<Vec<Accelerator>> {
    const INCLUDE_COMPUTE_ONLY: u64 = 1;
    const COMPUTE_ONLY: u32 = 1 << 11;
    let mut items = [D3DKMT_ADAPTERINFO::default(); 32];
    let mut request = D3DKMT_ENUMADAPTERS3 {
        Filter: D3DKMT_ENUMADAPTERS_FILTER {
            Value: INCLUDE_COMPUTE_ONLY,
        },
        NumAdapters: items.len() as u32,
        pAdapters: items.as_mut_ptr(),
    };
    unsafe {
        let gdi = GetModuleHandleW(windows_core::w!("gdi32.dll")).ok()?;
        let export = GetProcAddress(gdi, windows_core::s!("D3DKMTEnumAdapters3"))?;
        let enumerate: unsafe extern "system" fn(*mut D3DKMT_ENUMADAPTERS3) -> NTSTATUS =
            std::mem::transmute(export);
        let result = enumerate(&mut request);
        let mut devices = Vec::new();
        let count = (request.NumAdapters as usize).min(items.len());
        for entry in items.iter().take(count).filter(|entry| entry.hAdapter != 0) {
            if result.is_ok()
                && let Some(flags) = adapter_info::<u32>(entry.hAdapter, KMTQAITYPE_ADAPTERTYPE)
                && flags & COMPUTE_ONLY != 0
                && let Some(desc) = adapter_info::<D3DKMT_DRIVER_DESCRIPTION>(
                    entry.hAdapter,
                    KMTQAITYPE_DRIVER_DESCRIPTION,
                )
                && has_npu_name(&desc.DriverDescription)
                && let Ok(adapter) = factory.GetAdapterByLuid::<IDXCoreAdapter>(&entry.AdapterLuid)
                && let Some(device) = Accelerator::new(adapter)
            {
                devices.push(device);
            }
            let _ = D3DKMTCloseAdapter(&D3DKMT_CLOSEADAPTER {
                hAdapter: entry.hAdapter,
            });
        }
        Some(devices)
    }
}

fn adapter_info<T: Default>(adapter: u32, kind: KMTQUERYADAPTERINFOTYPE) -> Option<T> {
    let mut data = T::default();
    let mut query = D3DKMT_QUERYADAPTERINFO {
        hAdapter: adapter,
        Type: kind,
        pPrivateDriverData: (&mut data as *mut T).cast(),
        PrivateDriverDataSize: size_of::<T>() as u32,
    };
    unsafe { D3DKMTQueryAdapterInfo(&mut query) }
        .is_ok()
        .then_some(data)
}

fn has_npu_name(text: &[u16]) -> bool {
    let name = String::from_utf16_lossy(text.split(|ch| *ch == 0).next().unwrap_or_default());
    name.split(|ch: char| !ch.is_ascii_alphanumeric())
        .any(|word| word.eq_ignore_ascii_case("NPU"))
        || name.eq_ignore_ascii_case("Intel(R) AI Boost")
}

fn adapters() -> [Vec<Accelerator>; 2] {
    unsafe {
        let Ok(factory) = DXCoreCreateAdapterFactory::<IDXCoreAdapterFactory>() else {
            return Default::default();
        };
        let modern = factory.cast::<IDXCoreAdapterFactory1>().ok();
        let workload = |workload, hardware| {
            let mut raw = std::ptr::null_mut();
            modern
                .as_ref()?
                .create_adapter_list_by_workload(
                    workload,
                    0,
                    hardware,
                    &IDXCoreAdapterList::IID,
                    &mut raw,
                )
                .ok()
                .ok()?;
            (!raw.is_null()).then(|| IDXCoreAdapterList::from_raw(raw))
        };
        let mut npus = workload(3, 4).map(enumerate).unwrap_or_default();
        if npus.is_empty() {
            npus = compute_only_npus(&factory).unwrap_or_default();
        }
        [
            workload(0, 1)
                .or_else(|| {
                    factory
                        .CreateAdapterList(&[DXCORE_ADAPTER_ATTRIBUTE_D3D11_GRAPHICS])
                        .ok()
                })
                .map(enumerate)
                .unwrap_or_default(),
            npus,
        ]
    }
}

#[derive(Default)]
struct Sampler {
    cpu: Option<(Instant, [u64; 3])>,
    devices: Option<[Vec<Accelerator>; 2]>,
    sampled: Option<(Instant, [Option<u8>; 4])>,
}

thread_local! { static SAMPLER: RefCell<Sampler> = RefCell::new(Sampler::default()); }

pub fn reset() {
    SAMPLER.with(|sampler| *sampler.borrow_mut() = Sampler::default());
}

fn percent(used: u128, total: u128) -> Option<u8> {
    (total > 0).then(|| ((used.min(total) * 100 + total / 2) / total) as u8)
}

fn cpu_time() -> Option<[u64; 3]> {
    let (mut idle, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
    Some(
        [idle, kernel, user]
            .map(|time| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)),
    )
}

fn ram() -> Option<u8> {
    let mut mem = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    unsafe { GlobalMemoryStatusEx(&mut mem) }.ok()?;
    (mem.dwMemoryLoad <= 100).then_some(mem.dwMemoryLoad as u8)
}

pub fn sample() -> [Option<u8>; 4] {
    SAMPLER.with(|slot| {
        let mut sampler = slot.borrow_mut();
        let now = Instant::now();
        if let Some((at, values)) = sampler.sampled
            && now.duration_since(at) < Duration::from_millis(850)
        {
            return values;
        }
        if sampler
            .sampled
            .is_some_and(|(at, _)| now.duration_since(at) >= Duration::from_secs(3))
        {
            *sampler = Sampler::default();
        }
        let times = cpu_time();
        let previous = std::mem::replace(&mut sampler.cpu, times.map(|times| (now, times)));
        let cpu = previous.zip(times).and_then(|((_, previous), current)| {
            let idle = current[0].checked_sub(previous[0])?;
            let total = current[1]
                .checked_sub(previous[1])?
                .checked_add(current[2].checked_sub(previous[2])?)?;
            percent(u128::from(total.checked_sub(idle)?), u128::from(total))
        });
        let devices = sampler.devices.get_or_insert_with(adapters);
        if devices
            .iter()
            .flatten()
            .any(|device| !unsafe { device.adapter.IsValid() })
        {
            *devices = adapters();
        }
        let readings = [
            cpu,
            devices[0]
                .iter_mut()
                .filter_map(Accelerator::busy_engine)
                .max(),
            devices[1]
                .iter_mut()
                .filter_map(Accelerator::busy_engine)
                .max(),
            ram(),
        ];
        sampler.sampled = Some((now, readings));
        readings
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn usage_handles_idle_unknown_and_rounding() {
        assert_eq!(super::percent(0, 100), Some(0));
        assert_eq!(super::percent(1, 3), Some(33));
        assert_eq!(super::percent(1, 0), None);
        assert_eq!(super::percent(101, 100), Some(100));
    }
}
