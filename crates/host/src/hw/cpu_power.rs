//! Real CPU package power from the Windows "Energy Meter" performance counters (Intel RAPL,
//! exposed by Windows itself; no driver, no admin rights). One PDH query is opened on first use
//! and reused; a read costs microseconds.

use std::sync::Mutex;
use windows::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE, PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
    PdhGetFormattedCounterValue, PdhOpenQueryW,
};
use windows_core::w;

struct Query {
    query: isize,
    counter: isize,
}

// PDH handles may be used from any thread; access is serialized by the mutex.
unsafe impl Send for Query {}

static QUERY: Mutex<Option<Result<Query, ()>>> = Mutex::new(None);

fn open() -> Result<Query, ()> {
    unsafe {
        let mut query = Default::default();
        if PdhOpenQueryW(windows_core::PCWSTR::null(), 0, &mut query) != 0 {
            return Err(());
        }
        let mut counter = Default::default();
        if PdhAddEnglishCounterW(query, w!(r"\Energy Meter(rapl_package0_pkg)\Power"), 0, &mut counter) != 0 {
            let _ = PdhCloseQuery(query);
            return Err(());
        }
        let _ = PdhCollectQueryData(query);
        Ok(Query { query: query.0 as isize, counter: counter.0 as isize })
    }
}

/// CPU package power in watts, or `None` when the platform has no energy meter.
pub fn package_watts() -> Option<f64> {
    let mut slot = QUERY.lock().ok()?;
    let query = slot.get_or_insert_with(open).as_ref().ok()?;
    unsafe {
        let q = windows::Win32::System::Performance::PDH_HQUERY(query.query as *mut _);
        let c = windows::Win32::System::Performance::PDH_HCOUNTER(query.counter as *mut _);
        if PdhCollectQueryData(q) != 0 {
            return None;
        }
        let mut value = PDH_FMT_COUNTERVALUE::default();
        if PdhGetFormattedCounterValue(c, PDH_FMT_DOUBLE, None, &mut value) != 0 {
            return None;
        }
        let milliwatts = value.Anonymous.doubleValue;
        (milliwatts.is_finite() && milliwatts >= 0.0).then_some(milliwatts / 1000.0)
    }
}
