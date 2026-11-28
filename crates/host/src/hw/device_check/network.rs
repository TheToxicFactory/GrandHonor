//! Local Windows adapter and network configuration checks.

use super::{
    Group, INTERNET_KEY, Outcome, Status, cancelled, device_driver, device_status, push, services,
};
use crate::hw::com::{
    connect_wmi, get_prop, get_prop_string, get_prop_u64, set_proxy_blanket_enum,
};
use crate::hw::devices::PresentDevice;
use grandhonor_core::locale::t;
use std::io::Read;
use std::net::IpAddr;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, ERROR_TIMEOUT, S_FALSE,
};
use windows::Win32::Networking::WinSock::{WSAENOBUFS, WSAPROTOCOL_INFOW, WSCEnumProtocols};
use windows::Win32::System::Ole::{SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::Win32::System::Variant::{VT_ARRAY, VT_BSTR};
use windows::Win32::System::Wmi::{
    IWbemClassObject, IWbemServices, WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY,
    WBEM_S_TIMEDOUT,
};
use windows_core::{BSTR, PCWSTR, w};

struct Wifi {
    id: Option<String>,
    index: Option<u64>,
    enabled: Option<bool>,
    connected: Option<bool>,
    address_changed: Option<bool>,
}

struct IpConfig {
    dhcp: Option<bool>,
    dhcp_server: Option<String>,
    addresses: Option<Vec<String>>,
    gateways: Option<Vec<String>>,
    dns: Option<Vec<String>>,
}

pub(super) fn scan(inventory: Option<&[PresentDevice]>) {
    let wifi = read_wifi();
    let matching_devices = match (&wifi, inventory) {
        (Ok(None), _) => Some(Vec::new()),
        (Ok(Some(wifi)), Some(inventory)) => wifi.id.as_ref().and_then(|id| {
            let matched: Vec<_> = inventory
                .iter()
                .filter(|node| node.device.instance_id.eq_ignore_ascii_case(id))
                .collect();
            (!matched.is_empty()).then_some(matched)
        }),
        _ => None,
    };
    push_wifi(
        "wifi.driver",
        t("Драйвер", "Driver", "驱动"),
        device_driver(matching_devices.as_deref()),
    );
    push_wifi(
        "wifi.service",
        t("Служба WLAN", "WLAN service", "WLAN 服务"),
        services(&[w!("WlanSvc")]),
    );
    push_wifi(
        "wifi.status",
        t("Состояние устройства", "Device status", "设备状态"),
        device_status(matching_devices.as_deref()),
    );

    let missing = if matches!(wifi, Ok(None)) {
        Status::Unsupported
    } else {
        Status::Unavailable
    };
    let wifi = wifi.ok().flatten();
    let enabled = wifi.as_ref().and_then(|wifi| wifi.enabled);
    let connected = wifi.as_ref().and_then(|wifi| wifi.connected);
    let index = wifi.as_ref().and_then(|wifi| wifi.index);
    push_wifi(
        "wifi.network",
        t("Состояние адаптера", "Adapter state", "适配器状态"),
        observed(
            enabled,
            missing,
            t(
                "Адаптер отключён",
                "The adapter is disabled",
                "适配器已禁用",
            ),
        ),
    );

    if cancelled() {
        return;
    }
    let ip = (connected == Some(true))
        .then_some(index)
        .flatten()
        .and_then(|index| read_ip_config(index).ok().flatten());
    let dhcp = match ip.as_ref().and_then(|ip| ip.dhcp) {
        Some(false) => Outcome::detail(
            Status::Passed,
            t(
                "Используется статическая настройка",
                "Static configuration in use",
                "使用静态配置",
            ),
        ),
        Some(true) => address_check(
            ip.as_ref()
                .and_then(|ip| ip.dhcp_server.as_ref())
                .map(|server| vec![server.clone()])
                .as_deref(),
            Status::Unavailable,
        ),
        None => Outcome::new(missing),
    };
    push_wifi("wifi.dhcp", "DHCP", dhcp);
    let assigned = ip.as_ref().and_then(|ip| ip.addresses.as_deref());
    let gateways = ip.as_ref().and_then(|ip| ip.gateways.as_deref());
    let connection = if connected == Some(true) {
        match (
            address_check(assigned, missing).status,
            address_check(gateways, missing).status,
        ) {
            (Status::Passed, Status::Passed) => Outcome::new(Status::Passed),
            (Status::Problem, _) => Outcome::detail(
                Status::Problem,
                t(
                    "Нет назначенного IP-адреса",
                    "No assigned IP address",
                    "未分配 IP 地址",
                ),
            ),
            (_, Status::Problem) => Outcome::detail(
                Status::Problem,
                t(
                    "Подключено без шлюза по умолчанию",
                    "Connected without a default gateway",
                    "已连接，但未配置默认网关",
                ),
            ),
            _ => Outcome::new(missing),
        }
    } else {
        observed(
            connected,
            missing,
            t(
                "Адаптер не подключён к сети",
                "The adapter is not connected",
                "适配器未连接网络",
            ),
        )
    };
    push_wifi(
        "wifi.connection",
        t("Подключение к Wi-Fi", "Wi-Fi connection", "Wi-Fi 连接"),
        connection,
    );
    push_wifi(
        "wifi.dns",
        t("Настройка DNS", "DNS configuration", "DNS 配置"),
        address_check(ip.as_ref().and_then(|ip| ip.dns.as_deref()), missing),
    );
    push_wifi(
        "wifi.proxy",
        t("Прокси Windows", "Windows proxy", "Windows 代理"),
        proxy(),
    );
    push_wifi(
        "wifi.lsp",
        t(
            "Каталог Winsock (LSP)",
            "Winsock catalog (LSP)",
            "Winsock 目录 (LSP)",
        ),
        winsock(),
    );
    push_wifi("wifi.hosts", "HOSTS", hosts());
    let mac = match wifi.and_then(|wifi| wifi.address_changed) {
        Some(true) => Outcome::detail(
            Status::Passed,
            t(
                "Текущий MAC отличается от заводского",
                "The current MAC differs from the permanent address",
                "当前 MAC 地址不同于出厂地址",
            ),
        ),
        Some(false) => Outcome::detail(
            Status::Passed,
            t(
                "Сейчас используется заводской MAC",
                "The permanent MAC address is currently in use",
                "当前使用出厂 MAC 地址",
            ),
        ),
        None => Outcome::new(missing),
    };
    push_wifi(
        "wifi.mac",
        t(
            "Текущий и заводской MAC",
            "Current and permanent MAC",
            "当前和出厂 MAC 地址",
        ),
        mac,
    );
    push_wifi(
        INTERNET_KEY,
        t("Доступ в интернет", "Internet access", "互联网连接"),
        Outcome::detail(
            Status::Interactive,
            t(
                "Запустите отдельную проверку HTTPS",
                "Run the separate HTTPS check",
                "请单独运行 HTTPS 检查",
            ),
        ),
    );
}

fn push_wifi(key: &'static str, name: &'static str, outcome: Outcome) {
    push(Group::WiFi, key, name, outcome, None);
}

fn observed(value: Option<bool>, missing: Status, failure: &'static str) -> Outcome {
    match value {
        Some(true) => Outcome::new(Status::Passed),
        Some(false) => Outcome::detail(Status::Problem, failure),
        None => Outcome::new(missing),
    }
}

fn valid_address(text: &str) -> bool {
    // Windows includes "%<index>" for some IPv6 addresses.
    text.split('%')
        .next()
        .and_then(|address| address.parse::<IpAddr>().ok())
        .is_some_and(|ip| !ip.is_unspecified())
}

fn address_check(addresses: Option<&[String]>, missing: Status) -> Outcome {
    match addresses {
        Some(addresses) if addresses.iter().any(|address| valid_address(address)) => {
            Outcome::new(Status::Passed)
        }
        Some(_) => Outcome::new(Status::Problem),
        None => Outcome::new(missing),
    }
}

fn read_wifi() -> windows_core::Result<Option<Wifi>> {
    let services = connect_wmi("root\\StandardCimv2")?;
    let adapters = query(
        &services,
        "SELECT InterfaceIndex, PnPDeviceID, InterfaceAdminStatus, MediaConnectState, NetworkAddresses, PermanentAddress FROM MSFT_NetAdapter WHERE NdisPhysicalMedium = 9 OR NdisPhysicalMedium = 1",
    )?;
    let adapter = adapters
        .iter()
        .find(|adapter| get_prop_u64(adapter, "MediaConnectState") == Some(1))
        .or_else(|| adapters.first());
    Ok(adapter.map(|adapter| Wifi {
        id: get_prop_string(adapter, "PnPDeviceID"),
        index: get_prop_u64(adapter, "InterfaceIndex"),
        enabled: get_prop_u64(adapter, "InterfaceAdminStatus").and_then(|state| match state {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        }),
        connected: get_prop_u64(adapter, "MediaConnectState").and_then(|state| match state {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        }),
        address_changed: strings(adapter, "NetworkAddresses")
            .zip(get_prop_string(adapter, "PermanentAddress"))
            .and_then(|(current, permanent)| {
                let current = current.iter().find_map(|address| bare_mac(address))?;
                let permanent = bare_mac(&permanent)?;
                Some(!current.eq_ignore_ascii_case(&permanent))
            }),
    }))
}

fn bare_mac(text: &str) -> Option<String> {
    let digits: String = text
        .chars()
        .filter(|character| character.is_ascii_hexdigit())
        .collect();
    (digits.len() == 12 && digits != "000000000000").then_some(digits)
}

fn read_ip_config(index: u64) -> windows_core::Result<Option<IpConfig>> {
    let services = connect_wmi("root\\cimv2")?;
    let query_text = format!(
        "SELECT DHCPEnabled, DHCPServer, IPAddress, DefaultIPGateway, DNSServerSearchOrder FROM Win32_NetworkAdapterConfiguration WHERE InterfaceIndex = {index}"
    );
    let rows = query(&services, &query_text)?;
    Ok(rows.first().map(|row| IpConfig {
        dhcp: get_prop_u64(row, "DHCPEnabled").map(|flag| flag != 0),
        dhcp_server: get_prop_string(row, "DHCPServer"),
        addresses: strings(row, "IPAddress"),
        gateways: strings(row, "DefaultIPGateway"),
        dns: strings(row, "DNSServerSearchOrder"),
    }))
}

/// Semisynchronous queries with a finite enumeration deadline.
fn query(services: &IWbemServices, text: &str) -> windows_core::Result<Vec<IWbemClassObject>> {
    let enumerator = unsafe {
        services.ExecQuery(
            &BSTR::from("WQL"),
            &BSTR::from(text),
            WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
            None,
        )?
    };
    set_proxy_blanket_enum(&enumerator);
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut rows = Vec::new();
    loop {
        if cancelled() || Instant::now() >= deadline {
            return Err(windows_core::Error::from_hresult(
                ERROR_TIMEOUT.to_hresult(),
            ));
        }
        let mut batch: [Option<IWbemClassObject>; 8] = Default::default();
        let mut returned = 0;
        let result = unsafe { enumerator.Next(100, &mut batch, &mut returned) };
        result.ok()?;
        rows.extend(batch.into_iter().take(returned as usize).flatten());
        if result == S_FALSE {
            return Ok(rows);
        }
        if rows.len() > 256 || (returned == 0 && result.0 != WBEM_S_TIMEDOUT.0) {
            return Err(windows_core::Error::from_hresult(
                ERROR_TIMEOUT.to_hresult(),
            ));
        }
    }
}

fn strings(object: &IWbemClassObject, name: &str) -> Option<Vec<String>> {
    let value = get_prop(object, name).ok()?;
    if value.is_null_or_empty() {
        return Some(Vec::new());
    }
    unsafe {
        let inner = &*value.0.Anonymous.Anonymous;
        if inner.vt.0 != VT_ARRAY.0 | VT_BSTR.0 {
            return None;
        }
        let array = inner.Anonymous.parray;
        if array.is_null() {
            return None;
        }
        let lower = SafeArrayGetLBound(array, 1).ok()?;
        let upper = SafeArrayGetUBound(array, 1).ok()?;
        if upper.saturating_sub(lower) > 63 {
            return None;
        }
        (lower..=upper)
            .map(|index| {
                let mut text = BSTR::new();
                SafeArrayGetElement(array, &index, (&mut text as *mut BSTR).cast()).ok()?;
                Some(text.to_string())
            })
            .collect()
    }
}

pub(super) fn registry_dword(key: HKEY, subkey: PCWSTR, name: PCWSTR) -> Result<Option<u32>, ()> {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            key,
            subkey,
            name,
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    match status {
        ERROR_SUCCESS if size == 4 => Ok(Some(value)),
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(None),
        _ => Err(()),
    }
}

fn registry_has_string(key: HKEY, subkey: PCWSTR, name: PCWSTR) -> Result<bool, ()> {
    let mut size = 0u32;
    let status = unsafe {
        RegGetValueW(
            key,
            subkey,
            name,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    };
    match status {
        ERROR_SUCCESS => Ok(size > 2),
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(false),
        _ => Err(()),
    }
}

fn proxy() -> Outcome {
    let key = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings");
    match (
        registry_dword(HKEY_CURRENT_USER, key, w!("ProxyEnable")),
        registry_has_string(HKEY_CURRENT_USER, key, w!("AutoConfigURL")),
    ) {
        (Ok(Some(1)), Ok(_)) => {
            match registry_has_string(HKEY_CURRENT_USER, key, w!("ProxyServer")) {
                Ok(true) => Outcome::detail(
                    Status::Passed,
                    t(
                        "Ручной прокси настроен",
                        "Manual proxy is configured",
                        "已配置手动代理",
                    ),
                ),
                Ok(false) => Outcome::detail(
                    Status::Problem,
                    t(
                        "Прокси включён без адреса",
                        "Proxy is enabled without an address",
                        "代理已启用但缺少地址",
                    ),
                ),
                Err(()) => Outcome::new(Status::Unavailable),
            }
        }
        (Ok(_), Ok(true)) => Outcome::detail(
            Status::Passed,
            t(
                "Задан сценарий автоконфигурации",
                "A proxy configuration script is set",
                "已设置代理配置脚本",
            ),
        ),
        (Ok(_), Ok(false)) => Outcome::detail(
            Status::Passed,
            t(
                "Ручной прокси и сценарий не заданы",
                "No manual proxy or configuration script is set",
                "未设置手动代理或配置脚本",
            ),
        ),
        _ => Outcome::new(Status::Unavailable),
    }
}

fn winsock() -> Outcome {
    let mut bytes = 0u32;
    let mut error = 0;
    let first = unsafe { WSCEnumProtocols(None, None, &mut bytes, &mut error) };
    if first != -1 || error != WSAENOBUFS.0 || bytes == 0 || bytes > 256 * 1024 {
        return Outcome::new(Status::Unavailable);
    }
    let mut entries = vec![
        WSAPROTOCOL_INFOW::default();
        (bytes as usize).div_ceil(size_of::<WSAPROTOCOL_INFOW>())
    ];
    let result =
        unsafe { WSCEnumProtocols(None, Some(entries.as_mut_ptr()), &mut bytes, &mut error) };
    if result <= 0 || result as usize > entries.len() {
        return Outcome::new(Status::Unavailable);
    }
    let entries = &entries[..result as usize];
    for protocol in entries {
        let chain = &protocol.ProtocolChain;
        let Ok(length) = usize::try_from(chain.ChainLen) else {
            return Outcome::new(Status::Problem);
        };
        let Some(chain) = chain.ChainEntries.get(..length) else {
            return Outcome::new(Status::Problem);
        };
        if length > 1
            && chain
                .iter()
                .any(|id| !entries.iter().any(|entry| entry.dwCatalogEntryId == *id))
        {
            return Outcome::detail(
                Status::Problem,
                t(
                    "Недействительная цепочка провайдеров",
                    "Invalid provider chain",
                    "提供程序链无效",
                ),
            );
        }
    }
    Outcome::new(Status::Passed)
}

fn hosts() -> Outcome {
    let path = crate::known_dirs::system32()
        .join("drivers")
        .join("etc")
        .join("hosts");
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Outcome::detail(
                Status::Passed,
                t(
                    "Файл HOSTS отсутствует",
                    "There is no HOSTS file",
                    "没有 HOSTS 文件",
                ),
            );
        }
        Err(_) => return Outcome::new(Status::Unavailable),
    };
    let mut bytes = Vec::new();
    if file.take(1_048_577).read_to_end(&mut bytes).is_err() || bytes.len() > 1_048_576 {
        return Outcome::new(Status::Unavailable);
    }
    let hosts_text = match bytes.strip_prefix(&[0xff, 0xfe]) {
        Some(bytes) => String::from_utf16_lossy(
            &bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect::<Vec<_>>(),
        ),
        None => String::from_utf8_lossy(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes))
            .into_owned(),
    };
    if hosts_text.lines().all(valid_hosts_line) {
        Outcome::new(Status::Passed)
    } else {
        Outcome::detail(
            Status::Problem,
            t(
                "Есть строки без допустимого адреса или имени",
                "Some entries lack a valid address or hostname",
                "部分条目缺少有效地址或主机名",
            ),
        )
    }
}

fn valid_hosts_line(line: &str) -> bool {
    let mut fields = line
        .split('#')
        .next()
        .unwrap_or_default()
        .split_whitespace();
    match fields.next() {
        Some(address) => address.parse::<IpAddr>().is_ok() && fields.next().is_some(),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_and_hosts_checks_distinguish_missing_and_unspecified() {
        assert!(valid_address("fe80::1%12"));
        assert!(!valid_address("0.0.0.0"));
        assert!(valid_hosts_line("127.0.0.1 localhost # comment"));
        assert!(valid_hosts_line(" #  skipping comments "));
        assert!(!valid_hosts_line("127.0.0.1 # no hostname"));
    }
}
