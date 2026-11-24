use super::http::{download_package_to_file, get_text, head_package, post_json};
use super::trust::{compute_sha256_hex, is_honor_signed, verify_authenticode};
use crate::hw::com::{ComGuard, connect_wmi, get_prop_string, get_prop_u64, wmi_query};
use crate::scheduler::create_protected_directory;
use crate::settings;
use base64::Engine;
use flate2::read::GzDecoder;
use grandhonor_core::archive::{ArchiveBudget, MAX_NESTED_DEPTH};
use grandhonor_core::locale::{AppLanguage, language, t};
use grandhonor_core::url_guard::parse_package_url;
use grandhonor_core::version::{
    compare_versions, is_comparable_version, is_newer_version, normalize_release_date,
    normalize_support_title, normalize_support_version, version_parts,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use windows::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;

const CHECK_URL: &str = "https://update.platform.hihonorcloud.com/hid_and_common/v2/CheckEx.action?latest=true&verType=true&defenceHijack=true";
const LAPTOP_PRODUCT_ID: &str = "CMCG10000120";
const PLATFORM_TIMEOUT_MS: i32 = 10_000;
const HEAD_TIMEOUT_MS: i32 = 10_000;
const SUPPORT_TIMEOUT_MS: i32 = 30_000;
const DOWNLOAD_TIMEOUT_MS: i32 = 600_000;
const SUPPORT_CONCURRENCY: usize = 6;
const CIMV2_WMI: &str = r"\\.\root\cimv2";

#[derive(Clone, Debug)]
pub struct DriverComponent {
    pub id: i32,
    pub name: String,
    pub display_name: String,
    pub current_version: String,
    pub package_type: String,
    pub device_name: Option<String>,
    pub app_name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DriverUpdate {
    pub component: DriverComponent,
    pub version: String,
    pub download_base_url: String,
    pub size: u64,
    pub sha256: Option<String>,
    pub release_date: Option<String>,
    pub is_update: bool,
    pub package_title: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DriverCheckResult {
    pub components: Vec<DriverComponent>,
    pub updates: Vec<DriverUpdate>,
    pub is_complete: bool,
}

#[derive(Clone, Debug)]
struct PnpDriver {
    device_name: String,
    device_id: String,
    version: String,
    provider: String,
    device_class: String,
}

#[derive(Clone, Debug)]
struct MachineIdentity {
    device_name: String,
    c_version: String,
    processor_name: String,
    memory_gb: i32,
    identifiers: Vec<String>,
}

struct SupportEndpoint {
    api_url: &'static str,
    country_code: &'static str,
    language: &'static str,
    site_code: &'static str,
    minimal_parameters: bool,
}

const SUPPORT_ENDPOINTS: [SupportEndpoint; 3] = [
    SupportEndpoint {
        api_url: "https://selfservice-ap.honor.com/ccpcmd/services/dispatch/secured/CCPC/EN",
        country_code: "UZ",
        language: "ru",
        site_code: "ru_UZ_X",
        minimal_parameters: false,
    },
    SupportEndpoint {
        api_url: "https://selfservice-eu.honor.com/ccpcmd/services/dispatch/secured/CCPC/EN",
        country_code: "GB",
        language: "en",
        site_code: "en_GB_X",
        minimal_parameters: false,
    },
    SupportEndpoint {
        api_url: "https://selfservice-cn.honor.com/ccpcmd/services/dispatch/secured/CCPC/EN",
        country_code: "CN",
        language: "zh_cn",
        site_code: "zh_CN_X",
        minimal_parameters: false,
    },
];

const CHINESE_PACKAGE_ENDPOINT: SupportEndpoint = SupportEndpoint {
    api_url: "https://selfservice-cn.honor.com/ccpcmd/services/dispatch/secured/CCPC/EN",
    country_code: "CN",
    language: "zh-cn",
    site_code: "",
    minimal_parameters: true,
};

struct CatalogEntry {
    id: i32,
    name: &'static str,
    display_name: &'static str,
    app_name: Option<&'static str>,
}

const COMPONENT_CATALOG: [CatalogEntry; 22] = [
    CatalogEntry { id: 1, name: "Chipset", display_name: "Chipset", app_name: None },
    CatalogEntry { id: 2, name: "ME", display_name: "Intel Management Engine", app_name: None },
    CatalogEntry { id: 3, name: "Graphics", display_name: "Graphics", app_name: None },
    CatalogEntry { id: 4, name: "SerialIO", display_name: "Serial IO", app_name: None },
    CatalogEntry { id: 6, name: "DPTF", display_name: "Platform framework", app_name: None },
    CatalogEntry { id: 12, name: "WDT", display_name: "Watchdog", app_name: None },
    CatalogEntry { id: 14, name: "Audio", display_name: "Audio", app_name: None },
    CatalogEntry { id: 15, name: "WiFi", display_name: "Wi-Fi", app_name: None },
    CatalogEntry { id: 16, name: "BT", display_name: "Bluetooth", app_name: None },
    CatalogEntry { id: 18, name: "EgisFingerPrint", display_name: "Fingerprint", app_name: None },
    CatalogEntry { id: 23, name: "BIOS", display_name: "BIOS", app_name: None },
    CatalogEntry { id: 41, name: "Monitor", display_name: "Monitor", app_name: None },
    CatalogEntry { id: 52, name: "NFC", display_name: "NFC", app_name: None },
    CatalogEntry { id: 55, name: "PPM", display_name: "Intel PPM", app_name: None },
    CatalogEntry { id: 56, name: "TXT", display_name: "Intel TXT", app_name: None },
    CatalogEntry { id: 65, name: "ISST", display_name: "Intel Smart Sound", app_name: None },
    CatalogEntry { id: 73, name: "iPMT", display_name: "Intel PMT", app_name: None },
    CatalogEntry { id: 74, name: "NPU", display_name: "Intel NPU", app_name: None },
    CatalogEntry { id: 76, name: "LuxvisionsCameraDrv", display_name: "Camera", app_name: Some("HonorCamera") },
    CatalogEntry { id: 78, name: "MEP", display_name: "Windows Studio Effects", app_name: None },
    CatalogEntry { id: 87, name: "VDisplay", display_name: "Virtual Display", app_name: None },
    CatalogEntry { id: 88, name: "VHID", display_name: "Virtual HID", app_name: None },
];

#[derive(Serialize)]
struct PlatformRequest<'a> {
    components: Vec<PlatformComponentReq<'a>>,
    rules: PlatformRulesReq<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct PlatformComponentReq<'a> {
    app_name: &'a str,
    package_name: &'a str,
    package_type: &'a str,
    package_version_code: &'a str,
    package_version_name: &'a str,
    #[serde(rename = "componentID")]
    component_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct PlatformRulesReq<'a> {
    #[serde(rename = "C_version")]
    c_version: &'a str,
    dashboard: &'a str,
    dashboard_flash: &'a str,
    device_name: &'a str,
    firm_ware: &'a str,
    language: &'a str,
    #[serde(rename = "OS")]
    os: &'a str,
    #[serde(rename = "deviceId")]
    device_id: &'a str,
    #[serde(rename = "udid")]
    udid: &'a str,
}

pub fn check_drivers() -> Result<DriverCheckResult, String> {
    let installed = read_installed_components()?;
    let request_components = add_missing_catalog_components(&installed);
    let server_components: Vec<DriverComponent> = request_components
        .iter()
        .filter(|c| c.id > 0)
        .cloned()
        .collect();

    let machine = read_machine_identity();
    let dashboard = server_components
        .iter()
        .map(|c| format!("{}:{}", c.name, c.current_version))
        .collect::<Vec<_>>()
        .join(";")
        + ";";
    let lang_tag = match language() {
        AppLanguage::Russian => "ru-RU",
        AppLanguage::Chinese => "zh-CN",
        AppLanguage::English => "en-US",
    };
    let os_str = os_version_string();
    let payload = PlatformRequest {
        components: server_components
            .iter()
            .map(|c| PlatformComponentReq {
                app_name: c.app_name.as_deref().unwrap_or(&c.name),
                package_name: &c.name,
                package_type: &c.package_type,
                package_version_code: &c.current_version,
                package_version_name: env!("CARGO_PKG_VERSION"),
                component_id: c.id.to_string(),
            })
            .collect(),
        rules: PlatformRulesReq {
            c_version: &machine.c_version,
            dashboard: &dashboard,
            dashboard_flash: "",
            device_name: &machine.device_name,
            firm_ware: "",
            language: lang_tag,
            os: &os_str,
            device_id: "",
            udid: "",
        },
    };

    crate::log::info(&format!(
        "Checking HONOR driver updates for {}/{}; {} detected, {} requested",
        machine.device_name,
        machine.c_version,
        installed.len(),
        server_components.len()
    ));
    crate::log::info(&format!("Installed component versions: {dashboard}"));

    let (api_updates, api_versions) = check_platform(&payload, &server_components);
    let available_api_updates = filter_available_downloads(api_updates);
    let platform_missing = available_api_updates.is_empty() && api_versions.is_empty();

    // The support catalog is always read too: it is the only source of BIOS and of packages
    // the update API does not list, so an API result must not hide them.
    let _ = platform_missing;
    let (site_components, site_updates, _site_versions, site_complete) =
        check_support_site(&machine, &server_components);
    let available_site_updates = filter_available_downloads(site_updates);

    let mut grouped_updates: HashMap<i32, Vec<DriverUpdate>> = HashMap::new();
    for upd in available_api_updates
        .into_iter()
        .chain(available_site_updates)
    {
        grouped_updates
            .entry(upd.component.id)
            .or_default()
            .push(upd);
    }

    let mut updates = Vec::with_capacity(grouped_updates.len());
    for (_, mut candidates) in grouped_updates {
        candidates.sort_by(|a, b| {
            let cmp_a = is_comparable_version(&a.version, &a.component.current_version);
            let cmp_b = is_comparable_version(&b.version, &b.component.current_version);
            cmp_b
                .cmp(&cmp_a)
                .then_with(|| compare_versions(&b.version, &a.version))
                // Same version from both sources: prefer the one HONOR gives a SHA-256 for.
                .then_with(|| b.sha256.is_some().cmp(&a.sha256.is_some()))
        });
        if let Some(best) = candidates.into_iter().next() {
            updates.push(best);
        }
    }

    let mut seen_ids = HashSet::new();
    let mut components = Vec::new();
    for comp in request_components.into_iter().chain(site_components) {
        if seen_ids.insert(comp.id) {
            components.push(comp);
        }
    }

    for upd in updates.iter().filter(|u| u.is_update) {
        crate::log::info(&format!(
            "Update available for {}: {} -> {}",
            upd.component.name, upd.component.current_version, upd.version
        ));
    }
    crate::log::info(&format!(
        "HONOR driver update check completed; {} update(s), {} package(s)",
        updates.iter().filter(|u| u.is_update).count(),
        updates.len()
    ));

    Ok(DriverCheckResult {
        components,
        updates,
        is_complete: site_complete,
    })
}

fn check_platform(
    payload: &PlatformRequest<'_>,
    components: &[DriverComponent],
) -> (Vec<DriverUpdate>, HashMap<i32, String>) {
    let body = match serde_json::to_vec(payload) {
        Ok(b) => b,
        Err(e) => {
            crate::log::error(&format!("Could not serialize HONOR platform request: {e}"));
            return (Vec::new(), HashMap::new());
        }
    };
    let resp = match post_json(CHECK_URL, &body, PLATFORM_TIMEOUT_MS) {
        Ok(r) if r.status < 400 => r,
        Ok(r) => {
            crate::log::error(&format!(
                "HONOR update platform request failed with HTTP {}; using support catalog",
                r.status
            ));
            return (Vec::new(), HashMap::new());
        }
        Err(err) => {
            crate::log::info(&format!(
                "HONOR update platform request failed ({err}); using support catalog"
            ));
            return (Vec::new(), HashMap::new());
        }
    };
    let Ok(doc) = serde_json::from_slice::<Value>(&resp.body) else {
        return (Vec::new(), HashMap::new());
    };
    parse_platform_updates(&doc, components)
}

fn parse_platform_updates(
    root: &Value,
    components: &[DriverComponent],
) -> (Vec<DriverUpdate>, HashMap<i32, String>) {
    let mut updates = Vec::new();
    let mut versions: HashMap<i32, String> = HashMap::new();
    if root.get("status").and_then(Value::as_str) != Some("0") {
        return (updates, versions);
    }
    let Some(items) = root.get("components").and_then(Value::as_array) else {
        return (updates, versions);
    };

    let by_id: HashMap<i32, &DriverComponent> = components.iter().map(|c| (c.id, c)).collect();
    for item in items {
        let Some(id) = val_i32(item, "componentID") else {
            continue;
        };
        let Some(&comp) = by_id.get(&id) else {
            continue;
        };
        let version = val_str(item, "version")
            .or_else(|| val_str(item, "name"))
            .unwrap_or_default();
        if !version.is_empty() {
            versions
                .entry(id)
                .and_modify(|cur| {
                    if compare_versions(&version, cur).is_gt() {
                        *cur = version.clone();
                    }
                })
                .or_insert_with(|| version.clone());
        }
        let url = val_str(item, "url").unwrap_or_default();
        if version.is_empty() || url.is_empty() {
            continue;
        }
        let size = val_u64(item, "size").unwrap_or(0);
        let sha256 = val_str(item, "sha256").or_else(|| val_str(item, "hash"));
        let release_date = normalize_release_date(
            val_str(item, "releaseDate")
                .or_else(|| val_str(item, "updateTime"))
                .or_else(|| val_str(item, "date"))
                .as_deref(),
        );
        let is_update = is_newer_version(&version, &comp.current_version);
        updates.push(DriverUpdate {
            component: comp.clone(),
            version,
            download_base_url: url,
            size,
            sha256,
            release_date,
            is_update,
            package_title: None,
        });
    }
    (updates, versions)
}

fn filter_available_downloads(updates: Vec<DriverUpdate>) -> Vec<DriverUpdate> {
    let mut by_url: Vec<(url::Url, Vec<DriverUpdate>)> = Vec::new();
    let mut url_index: HashMap<String, usize> = HashMap::new();

    for upd in updates {
        let Some(url) = parse_package_url(&upd.download_base_url) else {
            continue;
        };
        let key = url.as_str().to_string();
        if let Some(&idx) = url_index.get(&key) {
            by_url[idx].1.push(upd);
        } else {
            url_index.insert(key, by_url.len());
            by_url.push((url, vec![upd]));
        }
    }

    let results = run_bounded_pool(by_url, SUPPORT_CONCURRENCY, |(url, group)| {
        match head_package(&url, HEAD_TIMEOUT_MS) {
            Ok(404 | 410) => {
                if let Some(first) = group.first() {
                    crate::log::info(&format!(
                        "Ignoring missing driver package: {} {} returned 404/410",
                        first.component.name, first.version
                    ));
                }
                Vec::new()
            }
            Ok(_) => group,
            Err(err) => {
                if err.contains("redirected outside") {
                    crate::log::error(&format!(
                        "Rejected driver package URL for {}: {err}",
                        url.host_str().unwrap_or("")
                    ));
                    Vec::new()
                } else {
                    crate::log::info(&format!(
                        "Driver package availability check warning for {}: {err}",
                        url.host_str().unwrap_or("")
                    ));
                    group
                }
            }
        }
    });

    results.into_iter().flatten().collect()
}

fn check_support_site(
    machine: &MachineIdentity,
    components: &[DriverComponent],
) -> (
    Vec<DriverComponent>,
    Vec<DriverUpdate>,
    HashMap<i32, String>,
    bool,
) {
    let known = known_support_offerings(machine);
    let endpoints = if !known.is_empty() {
        &SUPPORT_ENDPOINTS[..1]
    } else {
        &SUPPORT_ENDPOINTS[..]
    };
    let attempts = if !known.is_empty() { 2 } else { 1 };
    let mut had_failure = false;

    for endpoint in endpoints {
        for attempt in 1..=attempts {
            let last = attempt == attempts;
            let offerings = if !known.is_empty() {
                Ok(known.clone())
            } else {
                resolve_support_offerings(endpoint, machine)
            };
            match offerings {
                Ok(codes) if codes.is_empty() => break,
                Ok(codes) => match read_support_packages(&codes, components, machine) {
                    Ok((disc, upds, vers)) if !upds.is_empty() => {
                        crate::log::info(&format!(
                            "Official HONOR support matched {}; {} package(s)",
                            codes.join(","),
                            upds.len()
                        ));
                        return (disc, upds, vers, true);
                    }
                    Ok(_) => break,
                    Err(err) => {
                        crate::log::info(&format!(
                            "HONOR support catalog {} failed{}: {err}",
                            endpoint.country_code,
                            if last { "" } else { "; retrying" }
                        ));
                        if last {
                            had_failure = true;
                        }
                    }
                },
                Err(err) => {
                    crate::log::info(&format!(
                        "HONOR support catalog {} failed{}: {err}",
                        endpoint.country_code,
                        if last { "" } else { "; retrying" }
                    ));
                    if last {
                        had_failure = true;
                    }
                }
            }
        }
    }

    crate::log::info(&format!(
        "Official HONOR support has no catalog match for {}/{}",
        machine.device_name, machine.c_version
    ));
    (Vec::new(), Vec::new(), HashMap::new(), !had_failure)
}

/// Support catalog category of HONOR PC Manager and its add-ons.
const PC_MANAGER_CATEGORY: &str = "630";

/// HONOR prefixes model-specific versions with code names and a board number, e.g.
/// `ZhuqueC-XuanwuC-5211-BIOS 1.12` or `FermatB-5611-Alldriver 1.0`. Returns the prefix
/// when it names other models only; plain versions (`24.10.0.4`) are never foreign.
fn foreign_model(raw_version: &str, machine: &MachineIdentity) -> Option<String> {
    let parts: Vec<&str> = raw_version.trim().split('-').collect();
    let board = parts.iter().position(|p| p.len() == 4 && p.bytes().all(|b| b.is_ascii_digit()))?;
    let names = &parts[..board];
    if names.is_empty() || !names.iter().all(|n| n.len() >= 3 && n.bytes().all(|b| b.is_ascii_alphabetic())) {
        return None;
    }
    let mut ours: HashSet<String> = machine.identifiers.iter().map(|s| normalize_identity(s)).collect();
    ours.insert(normalize_identity(&machine.device_name));
    let matches = names.iter().any(|n| ours.contains(&normalize_identity(n)));
    (!matches).then(|| parts[..=board].join("-"))
}

fn known_support_offerings(machine: &MachineIdentity) -> Vec<String> {
    let mut ids: HashSet<String> = machine
        .identifiers
        .iter()
        .map(|s| normalize_identity(s))
        .collect();
    ids.insert(normalize_identity(&machine.device_name));
    let model_ok = ids.contains("ZQCP") || ids.contains("ZHUQUEC");
    let cpu_ok = processor_identity_tokens(&machine.processor_name)
        .iter()
        .any(|t| t == "358H");
    if model_ok && cpu_ok {
        vec!["OFFE00461151".to_string()]
    } else {
        Vec::new()
    }
}

fn resolve_support_offerings(
    endpoint: &SupportEndpoint,
    machine: &MachineIdentity,
) -> Result<Vec<String>, String> {
    let series_doc = get_support_json(
        endpoint,
        "/ccpc/queryNewCommodityList/1000",
        &[("productLevel", "lv3"), ("productId", LAPTOP_PRODUCT_ID)],
    )?;
    let Some(series) = extract_product_list(&series_doc) else {
        return Ok(Vec::new());
    };
    let product_ids: Vec<String> = series
        .iter()
        .filter_map(|item| val_str(item, "productId"))
        .filter(|id| !id.trim().is_empty())
        .collect();

    let product_lists = run_bounded_pool(product_ids, SUPPORT_CONCURRENCY, |product_id| {
        let Ok(doc) = get_support_json(
            endpoint,
            "/ccpc/queryNewCommodityList/1000",
            &[("productLevel", "lv5"), ("productId", &product_id)],
        ) else {
            return Vec::new();
        };
        let Some(list) = extract_product_list(&doc) else {
            return Vec::new();
        };
        list.iter()
            .filter_map(|item| {
                let code = val_str(item, "offeringCode")
                    .or_else(|| val_str(item, "productId"))
                    .unwrap_or_default();
                if code.trim().is_empty() {
                    return None;
                }
                let desc = ["displayName", "productName", "backName", "keyword"]
                    .iter()
                    .filter_map(|k| val_str(item, k))
                    .filter(|s| !s.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                Some((code, desc))
            })
            .collect::<Vec<_>>()
    });

    let mut scored: Vec<(String, i32)> = product_lists
        .into_iter()
        .flatten()
        .filter_map(|(code, desc)| {
            let score = score_product(&desc, machine);
            (score >= 100).then_some((code, score))
        })
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1));
    let Some(&(_, best)) = scored.first() else {
        return Ok(Vec::new());
    };
    if best < 130
        && scored.iter().filter(|(_, s)| *s == best).count() > 1
        && !processor_identity_tokens(&machine.processor_name).is_empty()
    {
        return Ok(Vec::new());
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (code, score) in scored {
        if score == best && seen.insert(code.to_ascii_uppercase()) {
            out.push(code);
        }
    }
    Ok(out)
}

fn score_product(description: &str, machine: &MachineIdentity) -> i32 {
    let product_text = normalize_identity(description);
    let mut score = 0i32;
    for id in &machine.identifiers {
        let norm = normalize_identity(id);
        if norm.len() >= 4 && product_text.contains(&norm) {
            let hw_code = !id.chars().any(char::is_whitespace)
                && (id.contains('-') || id.chars().any(|c| c.is_ascii_digit()));
            let base = if hw_code { 100 } else { 70 } + norm.len().min(20) as i32;
            score = score.max(base);
        }
    }
    if score < 70 {
        return 0;
    }
    for token in processor_identity_tokens(&machine.processor_name) {
        if product_text.contains(&token) {
            score += 30;
        }
    }
    if machine.memory_gb > 0 && contains_memory_gb(description, machine.memory_gb) {
        score += 5;
    }
    score
}

fn read_support_packages(
    offering_codes: &[String],
    installed: &[DriverComponent],
    machine: &MachineIdentity,
) -> Result<(Vec<DriverComponent>, Vec<DriverUpdate>, HashMap<i32, String>), String> {
    let catalog_endpoint = &SUPPORT_ENDPOINTS[0];
    let package_endpoint = &CHINESE_PACKAGE_ENDPOINT;
    let by_id: HashMap<i32, &DriverComponent> = installed.iter().map(|c| (c.id, c)).collect();
    let mut discovered: HashMap<i32, DriverComponent> = HashMap::new();
    let mut updates = Vec::new();
    let mut versions: HashMap<i32, String> = HashMap::new();

    for offering_code in offering_codes {
        let catalog_doc = get_support_json(
            catalog_endpoint,
            "/knowledgeBase/getKnowCatalog/1000",
            &[
                ("knowTypeId", "63"),
                ("orderType", "1"),
                ("page", "1"),
                ("pageSize", "100"),
                ("offeringCode", offering_code),
                ("cflag", "1"),
            ],
        )?;
        let Some(cat_list) = catalog_doc
            .get("responseData")
            .and_then(|d| d.get("catalogList"))
            .and_then(Value::as_array)
        else {
            continue;
        };

        let categories: Vec<(String, String)> = cat_list
            .iter()
            .filter_map(|cat| {
                let id = val_str(cat, "knowTypeId")?;
                let name = clean_catalog_label(&val_str(cat, "knowTypeName").unwrap_or_default());
                (!id.trim().is_empty()).then_some((id, name))
            })
            .collect();

        let pkg_lists = run_bounded_pool(categories, SUPPORT_CONCURRENCY, |(cat_id, cat_name)| {
            let Ok(doc) = get_support_json(
                package_endpoint,
                "/knowledgeBase/getServicePolicy/1000",
                &[
                    ("knowTypeId", &cat_id),
                    ("orderType", "1"),
                    ("page", "1"),
                    ("pageSize", "999"),
                    ("offeringCode", offering_code),
                    ("driverDown", "true"),
                    ("cflag", "1"),
                ],
            ) else {
                return Vec::new();
            };
            let Some(details) = doc
                .get("responseData")
                .and_then(|d| d.get("detailList"))
                .and_then(Value::as_array)
            else {
                return Vec::new();
            };
            details
                .iter()
                .filter_map(|item| {
                    let title = clean_catalog_label(
                        &val_str(item, "resourceTitle").unwrap_or_else(|| cat_name.clone()),
                    );
                    let raw_version = val_str(item, "versionNumber").unwrap_or_default();
                    if cat_id == PC_MANAGER_CATEGORY {
                        // HONOR PC Manager is what GrandHonor replaces; never offer it.
                        return None;
                    }
                    if let Some(model) = foreign_model(&raw_version, machine) {
                        crate::log::info(&format!("Skipping support package for another model ({model}): {raw_version}"));
                        return None;
                    }
                    let ver = normalize_support_version(&raw_version);
                    let url = val_str(item, "downloadUrl").unwrap_or_default();
                    let update_time = val_str(item, "updateTime");
                    if ver.trim().is_empty() || url.trim().is_empty() {
                        return None;
                    }
                    Some((title, ver, url, update_time, cat_id.clone(), cat_name.clone()))
                })
                .collect::<Vec<_>>()
        });

        for (title, version, url, update_time, cat_id, cat_name) in pkg_lists.into_iter().flatten()
        {
            let mut id = support_component_id(&title, &cat_id, &cat_name);
            let component = if id > 0
                && let Some(&inst) = by_id.get(&id)
            {
                inst.clone()
            } else {
                if id <= 0 {
                    id = stable_support_component_id(&format!("{cat_id}|{title}"));
                }
                let cat_entry = COMPONENT_CATALOG.iter().find(|c| c.id == id);
                let comp = DriverComponent {
                    id,
                    name: cat_entry
                        .map(|c| c.name.to_string())
                        .unwrap_or_else(|| title.clone()),
                    display_name: cat_entry
                        .map(|c| c.display_name.to_string())
                        .unwrap_or_else(|| cat_name.clone()),
                    current_version: "0".to_string(),
                    package_type: String::new(),
                    device_name: Some(normalize_support_title(&title, &version)),
                    app_name: cat_entry.and_then(|c| c.app_name.map(str::to_string)),
                };
                discovered.entry(id).or_insert_with(|| comp.clone());
                comp
            };

            versions
                .entry(id)
                .and_modify(|cur| {
                    if compare_versions(&version, cur).is_gt() {
                        *cur = version.clone();
                    }
                })
                .or_insert_with(|| version.clone());

            let is_update = is_newer_version(&version, &component.current_version);
            updates.push(DriverUpdate {
                component,
                version: version.clone(),
                download_base_url: url,
                size: 0,
                sha256: None,
                release_date: normalize_release_date(update_time.as_deref()),
                is_update,
                package_title: Some(normalize_support_title(&title, &version)),
            });
        }
    }

    Ok((discovered.into_values().collect(), updates, versions))
}

fn get_support_json(
    endpoint: &SupportEndpoint,
    path: &str,
    params: &[(&str, &str)],
) -> Result<Value, String> {
    let mut query_parts = Vec::with_capacity(params.len() + 8);
    for (k, v) in params {
        query_parts.push(format!("{}={}", percent_encode(k), percent_encode(v)));
    }
    query_parts.push(format!("areaCode={}", percent_encode(endpoint.country_code)));
    query_parts.push(format!("langCode={}", percent_encode(endpoint.language)));
    if !endpoint.minimal_parameters {
        query_parts.push("channelCode=HONOR".to_string());
        query_parts.push(format!("countryCode={}", percent_encode(endpoint.country_code)));
        query_parts.push(format!("country={}", percent_encode(endpoint.country_code)));
        query_parts.push(format!("language={}", percent_encode(endpoint.language)));
        query_parts.push(format!("siteCode={}", percent_encode(endpoint.site_code)));
    }
    query_parts.push("jsonp=callback".to_string());
    let full_url = format!("{}{}?{}", endpoint.api_url, path, query_parts.join("&"));
    let resp = get_text(&full_url, SUPPORT_TIMEOUT_MS)?;
    if resp.status >= 400 {
        return Err(format!("HTTP {}", resp.status));
    }
    let text = String::from_utf8_lossy(&resp.body);
    let start = text.find('(').ok_or("HONOR support returned invalid JSONP.")?;
    let end = text.rfind(')').ok_or("HONOR support returned invalid JSONP.")?;
    if end <= start {
        return Err("HONOR support returned invalid JSONP.".to_string());
    }
    serde_json::from_str(&text[start + 1..end]).map_err(|e| e.to_string())
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            use std::fmt::Write;
            let _ = write!(&mut out, "%{b:02X}");
        }
    }
    out
}

fn extract_product_list(root: &Value) -> Option<&Vec<Value>> {
    if val_str(root, "responseCode").as_deref() != Some("200") {
        return None;
    }
    root.get("responseData")
        .and_then(|d| d.get("productList"))
        .and_then(Value::as_array)
}

pub fn verify_download_to_temp(update: &DriverUpdate) -> Result<(String, u64), String> {
    let url = parse_package_url(&update.download_base_url)
        .ok_or("The driver package address is not supported.")?;
    with_package_staging(|staging| {
        let prepared = prepare_package(update, &url, staging)?;
        let size = fs::metadata(&prepared).map_err(|e| e.to_string())?.len();
        let file_ext = prepared
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_default();
        Ok((file_ext, size))
    })
}

pub fn download_and_save_dialog(update: &DriverUpdate) -> Result<Option<PathBuf>, String> {
    let url = parse_package_url(&update.download_base_url)
        .ok_or("The driver package address is not supported.")?;
    let orig_name = get_original_package_name(&url).unwrap_or_else(|| {
        safe_file_name(
            update
                .package_title
                .as_deref()
                .unwrap_or(&update.component.display_name),
        )
    });
    let stem = Path::new(&orig_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("HONOR-driver");
    with_package_staging(|staging| {
        let prepared = prepare_package(update, &url, staging)?;
        let ext = prepared
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("download");
        let Some(dest) = super::transfer::save_file_dialog(
            t("Сохранить драйвер", "Save driver", "保存驱动程序"),
            &format!("{stem}.{ext}"),
            t("Файл драйвера", "Driver file", "驱动程序文件"),
            &format!("*.{ext}"),
            ext,
        )?
        else {
            return Ok(None);
        };
        fs::copy(&prepared, &dest).map_err(|e| e.to_string())?;
        crate::log::info(&format!(
            "Saved verified driver package to {}",
            dest.to_string_lossy()
        ));
        Ok(Some(dest))
    })
}

fn prepare_package(
    update: &DriverUpdate,
    download_url: &url::Url,
    pkg_dir: &Path,
) -> Result<PathBuf, String> {
    let ext = Path::new(download_url.path())
        .extension()
        .and_then(|e| e.to_str())
        .filter(|ext| {
            !ext.is_empty() && ext.len() <= 8 && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        })
        .map(|ext| format!(".{ext}"))
        .unwrap_or_else(|| ".download".to_string());
    let pkg_path = pkg_dir.join(format!("package{ext}"));

    crate::log::info(&format!(
        "Downloading driver {} {} from {}",
        update.component.name,
        update.version,
        download_url.host_str().unwrap_or("")
    ));
    let status =
        download_package_to_file(download_url, &pkg_path, update.size, DOWNLOAD_TIMEOUT_MS)?;
    if matches!(status, 404 | 410) {
        return Err(t(
            "Файл драйвера больше недоступен на сервере HONOR. Обновите список драйверов.",
            "The driver file is no longer available on the HONOR server. Refresh the driver list.",
            "该驱动程序文件已无法从 HONOR 服务器获取。请刷新驱动程序列表。",
        )
        .to_string());
    }
    if status >= 400 {
        return Err(format!("HONOR server returned HTTP {status}."));
    }

    let expected_hash = update
        .sha256
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(expected_hash) = expected_hash {
        let actual = compute_sha256_hex(&pkg_path)?;
        if !actual.eq_ignore_ascii_case(expected_hash) {
            return Err(t(
                "Контрольная сумма пакета драйвера не совпадает.",
                "The driver package checksum does not match.",
                "驱动程序包校验和不匹配。",
            )
            .to_string());
        }
    }

    let is_zip = is_zip_magic(&pkg_path);
    // HONOR's support catalog lists packages without a SHA-256. Such a ZIP is accepted only
    // when it holds exactly one installer and that installer carries a valid signature from
    // HONOR itself (Honor Device Co., Ltd.): the signed program is what runs, and Windows
    // checks that it was not modified. Anything else without a hash is refused.
    if expected_hash.is_none() && is_zip {
        let check_dir = pkg_dir.join("check");
        let mut budget = ArchiveBudget::default();
        fs::create_dir(&check_dir).map_err(|e| e.to_string())?;
        budget.extract(&pkg_path, &check_dir).map_err(|e| e.to_string())?;
        extract_nested_zips(&check_dir, &mut budget)?;
        let signed = find_unique_exe(&check_dir).filter(|exe| is_honor_signed(exe));
        let _ = fs::remove_dir_all(&check_dir);
        if signed.is_none() {
            return Err(t(
                "Пакет без контрольной суммы HONOR не содержит установщика с подписью HONOR.",
                "The package has no HONOR checksum and no installer signed by HONOR.",
                "该软件包没有 HONOR 校验和，也没有 HONOR 签名的安装程序。",
            )
            .to_string());
        }
        crate::log::info(&format!("Driver {}: no SHA-256 in the HONOR catalog; installer signed by HONOR", update.component.name));
    } else if expected_hash.is_none() && !ext.eq_ignore_ascii_case(".exe") {
        return Err(t(
            "Не удалось проверить целостность пакета драйвера.",
            "The driver package integrity could not be verified.",
            "无法验证驱动程序包的完整性。",
        )
        .to_string());
    }

    if is_zip && !ext.eq_ignore_ascii_case(".zip") {
        let archive = pkg_path.with_extension("zip");
        fs::rename(&pkg_path, &archive).map_err(|e| e.to_string())?;
        return Ok(archive);
    }
    if ext.eq_ignore_ascii_case(".exe") {
        check_installer_signature(&pkg_path)?;
        // Without a HONOR checksum only HONOR's own signature is trusted, not any signer.
        if expected_hash.is_none() && !crate::drivers::trust::is_honor_signed(&pkg_path) {
            return Err(t(
                "Пакет без контрольной суммы HONOR подписан не HONOR.",
                "The package has no HONOR checksum and is not signed by HONOR.",
                "该软件包没有 HONOR 校验和，且不是 HONOR 签名。",
            )
            .to_string());
        }
    }
    Ok(pkg_path)
}

fn check_installer_signature(path: &Path) -> Result<(), String> {
    if verify_authenticode(path) {
        return Ok(());
    }
    Err(t(
        "Цифровая подпись установщика недействительна.",
        "The installer digital signature is invalid.",
        "安装程序数字签名无效。",
    )
    .to_string())
}

pub fn download_and_run_verified_installer<F>(
    update: &DriverUpdate,
    confirm: F,
) -> Result<Option<i32>, String>
where
    F: FnOnce(&str) -> bool,
{
    let url = parse_package_url(&update.download_base_url)
        .ok_or("The driver package address is not supported.")?;
    with_package_staging(|staging| {
        let package = prepare_package(update, &url, staging)?;
        let installer = if is_zip_magic(&package) {
            let extract_dir = staging.join("package");
            let mut budget = ArchiveBudget::default();
            fs::create_dir(&extract_dir).map_err(|e| e.to_string())?;
            budget
                .extract(&package, &extract_dir)
                .map_err(|e| e.to_string())?;
            extract_nested_zips(&extract_dir, &mut budget)?;
            let candidate = find_unique_exe(&extract_dir);
            if let Some(path) = &candidate {
                check_installer_signature(path)?;
            }
            candidate
        } else if has_exe_extension(&package) {
            Some(package)
        } else {
            None
        };
        let prepared = installer.ok_or_else(|| t(
            "В пакете нет единственного установщика .exe. Используйте «Сохранить проверенный пакет».",
            "The package does not contain a single .exe installer. Use 'Save verified package'.",
            "软件包中没有唯一的 .exe 安装程序，请使用“保存已验证的软件包”。",
        ).to_string())?;
        if !confirm(&update.component.display_name) {
            return Ok(None);
        }

        let work_dir = prepared.parent().unwrap_or(staging);
        let status = std::process::Command::new(&prepared)
            .current_dir(work_dir)
            .status()
            .map_err(|e| e.to_string())?;
        Ok(Some(status.code().unwrap_or(-1)))
    })
}

fn with_package_staging<T>(run: impl FnOnce(&Path) -> Result<T, String>) -> Result<T, String> {
    let mut buffer = [0u16; 260];
    let len = unsafe { GetSystemWindowsDirectoryW(Some(&mut buffer)) } as usize;
    if len == 0 || len >= buffer.len() {
        return Err("Could not locate the Windows directory.".into());
    }
    let staging = PathBuf::from(String::from_utf16_lossy(&buffer[..len]))
        .join("Temp")
        .join(format!("GrandHonor-{}", unique_token()));
    create_protected_directory(&staging, true)?;

    let res = run(&staging);
    let _ = fs::remove_dir_all(&staging);
    res
}

fn clean_catalog_label(value: &str) -> String {
    value
        .replace('\u{00A0}', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_zip_magic(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).is_ok() && magic[0] == b'P' && magic[1] == b'K'
}

fn extract_nested_zips(root: &Path, budget: &mut ArchiveBudget) -> Result<(), String> {
    let mut seen = HashSet::new();
    for _ in 0..MAX_NESTED_DEPTH {
        let mut zips = Vec::new();
        collect_matching(
            root,
            &|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
            },
            &mut zips,
        );
        let pending: Vec<PathBuf> = zips
            .into_iter()
            .filter(|p| seen.insert(p.clone()))
            .collect();
        if pending.is_empty() {
            return Ok(());
        }
        for z in pending {
            let dest = z
                .parent()
                .unwrap_or(root)
                .join(z.file_stem().and_then(|s| s.to_str()).unwrap_or("nested"));
            fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
            budget.extract(&z, &dest).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn has_exe_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
}

fn find_unique_exe(root: &Path) -> Option<PathBuf> {
    let mut exes = Vec::new();
    collect_matching(root, &has_exe_extension, &mut exes);
    if exes.len() == 1 { exes.pop() } else { None }
}

fn collect_matching(dir: &Path, pred: &dyn Fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_matching(&path, pred, out);
        } else if pred(&path) {
            out.push(path);
        }
    }
}

fn get_original_package_name(url: &url::Url) -> Option<String> {
    let seg = url.path().rsplit('/').next().unwrap_or("");
    let stem = Path::new(seg).file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if !stem.is_empty() && stem.len() <= 8192 {
        let mut b64 = stem.replace('-', "+").replace('_', "/");
        while b64.len() % 4 != 0 {
            b64.push('=');
        }
        if let Ok(compressed) = base64::engine::general_purpose::STANDARD.decode(&b64) {
            let mut gz = GzDecoder::new(&compressed[..]);
            let mut json_str = String::new();
            if gz.read_to_string(&mut json_str).is_ok()
                && let Ok(doc) = serde_json::from_str::<Value>(&json_str)
                && let Some(file_name) = val_str(&doc, "fileName")
                && !file_name.trim().is_empty()
            {
                let base = Path::new(&file_name)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&file_name);
                return Some(safe_file_name(base));
            }
        }
    }
    let has_ext = Path::new(seg).extension().is_some();
    has_ext.then(|| safe_file_name(seg))
}

fn safe_file_name(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control()
            {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "HONOR-driver".to_string()
    } else {
        trimmed.to_string()
    }
}

fn read_installed_components() -> Result<Vec<DriverComponent>, String> {
    let _com = ComGuard::mta();
    let services = connect_wmi(CIMV2_WMI).map_err(|e| e.to_string())?;
    let pnp_objs = wmi_query(
        &services,
        "SELECT DeviceName, DeviceID, DeviceClass, DriverVersion, DriverProviderName FROM Win32_PnPSignedDriver",
    )
    .map_err(|e| e.to_string())?;

    let mut drivers = Vec::with_capacity(pnp_objs.len());
    for obj in pnp_objs {
        let version = get_prop_string(&obj, "DriverVersion").unwrap_or_default();
        if version.trim().is_empty() {
            continue;
        }
        drivers.push(PnpDriver {
            device_name: get_prop_string(&obj, "DeviceName").unwrap_or_default(),
            device_id: get_prop_string(&obj, "DeviceID").unwrap_or_default(),
            version,
            provider: get_prop_string(&obj, "DriverProviderName").unwrap_or_default(),
            device_class: get_prop_string(&obj, "DeviceClass").unwrap_or_default(),
        });
    }

    let mut comps = Vec::new();
    add_comp(&mut comps, &drivers, 87, "VDisplay", "Virtual Display", &[&|d| has(d, "virtual display")], None, None);
    add_comp(&mut comps, &drivers, 88, "VHID", "Virtual HID", &[&|d| has(d, "virtual hid")], None, None);
    add_comp(&mut comps, &drivers, 12, "WDT", "Watchdog", &[&|d| has(d, "wdtdevice") || id_has(d, "WDT0001")], None, None);
    add_comp(&mut comps, &drivers, 1, "Chipset", "Chipset", &[&|d| has(d, "lpc") || has(d, "espi") || has(d, "smbus") || has(d, "chipset")], None, None);
    add_comp(&mut comps, &drivers, 2, "ME", "Intel Management Engine", &[&|d| has(d, "management engine interface")], None, None);
    add_comp(
        &mut comps,
        &drivers,
        3,
        "Graphics",
        "Graphics",
        &[
            &|d| is_display_adapter(d, "VEN_8086"),
            &|d| is_display_adapter(d, "VEN_1002"),
            &|d| is_display_adapter(d, "VEN_10DE"),
        ],
        None,
        None,
    );
    add_comp(&mut comps, &drivers, 4, "SerialIO", "Serial IO", &[&|d| has(d, "serial io")], None, None);
    add_comp(&mut comps, &drivers, 6, "DPTF", "Platform framework", &[&|d| {
        has(d, "innovation platform framework manager") || has(d, "dynamic platform and thermal framework")
    }], None, None);
    add_comp(&mut comps, &drivers, 14, "Audio", "Audio", &[&|d| id_has(d, "FUNC_01") && has(d, "audio")], None, None);
    add_comp(&mut comps, &drivers, 15, "WiFi", "Wi-Fi", &[&|d| {
        class_is(d, "NET")
            && (starts_ci(&d.device_id, "PCI\\") || starts_ci(&d.device_id, "USB\\"))
            && (has(d, "wi-fi") || has(d, "wifi") || has(d, "wireless") || has(d, "wlan"))
            && !has(d, "virtual")
            && !is_generic_provider(d)
    }], None, None);
    add_comp(&mut comps, &drivers, 16, "BT", "Bluetooth", &[&|d| {
        (class_is(d, "BLUETOOTH") || has(d, "bluetooth")) && !is_generic_provider(d)
    }], None, None);
    add_comp(&mut comps, &drivers, 18, "EgisFingerPrint", "Fingerprint", &[&|d| {
        class_is(d, "BIOMETRIC") || has(d, "fingerprint")
    }], Some(&fingerprint_pkg_type), None);
    add_comp(&mut comps, &drivers, 41, "Monitor", "Monitor", &[&|d| {
        (class_is(d, "MONITOR")
            || starts_ci(&d.device_id, "DISPLAY\\")
            || starts_ci(&d.device_id, "MONITOR\\"))
            && !is_generic_provider(d)
    }], None, None);
    add_comp(&mut comps, &drivers, 52, "NFC", "NFC", &[&|d| has(d, "nfc") || id_has(d, "NTAG")], None, None);
    add_comp(&mut comps, &drivers, 55, "PPM", "Intel PPM", &[&|d| has(d, "ppm provisioning")], None, None);
    add_comp(&mut comps, &drivers, 56, "TXT", "Intel TXT", &[&|d| has(d, "txt authenticated")], None, None);
    add_comp(&mut comps, &drivers, 65, "ISST", "Intel Smart Sound", &[&|d| has(d, "smart sound")], None, None);
    add_comp(&mut comps, &drivers, 73, "iPMT", "Intel PMT", &[&|d| has(d, "platform monitoring technology")], None, None);
    add_comp(&mut comps, &drivers, 74, "NPU", "Intel NPU", &[&|d| {
        !has(d, "gaussian")
            && !has(d, "gna")
            && (class_is(d, "COMPUTEACCELERATOR")
                || class_is(d, "NEURALPROCESSOR")
                || has(d, "npu")
                || has(d, "neural processor")
                || has(d, "ai boost"))
    }], None, None);
    add_comp(&mut comps, &drivers, 76, "LuxvisionsCameraDrv", "Camera", &[&|d| {
        (class_is(d, "CAMERA") || class_is(d, "IMAGE") || has(d, "camera")) && !is_generic_provider(d)
    }], None, Some("HonorCamera"));
    add_comp(&mut comps, &drivers, 78, "MEP", "Windows Studio Effects", &[&|d| {
        id_has(d, "MEP_") && (has(d, "windows camera effects") || has(d, "windows studio effects"))
    }], None, None);

    if let Ok(bios_objs) = wmi_query(&services, "SELECT SMBIOSBIOSVersion FROM Win32_BIOS")
        && let Some(bios_ver) = bios_objs
            .into_iter()
            .filter_map(|o| get_prop_string(&o, "SMBIOSBIOSVersion"))
            .find(|v| !v.trim().is_empty())
    {
        let pkg_type = settings::read_hklm_string(
            r"HARDWARE\DESCRIPTION\System\BIOS",
            "SystemProductName",
        )
        .unwrap_or_default();
        comps.push(DriverComponent {
            id: 23,
            name: "BIOS".to_string(),
            display_name: "BIOS".to_string(),
            current_version: bios_ver,
            package_type: pkg_type,
            device_name: None,
            app_name: None,
        });
    }

    if let Ok(missing_objs) = wmi_query(
        &services,
        "SELECT Name, DeviceID, PNPClass FROM Win32_PnPEntity WHERE ConfigManagerErrorCode = 28",
    ) {
        let mut missing: Vec<(Option<String>, Option<String>, Option<String>)> = missing_objs
            .into_iter()
            .map(|o| {
                (
                    get_prop_string(&o, "Name"),
                    get_prop_string(&o, "DeviceID"),
                    get_prop_string(&o, "PNPClass"),
                )
            })
            .collect();
        missing.sort_by(|a, b| {
            a.1.as_deref()
                .unwrap_or("")
                .to_ascii_lowercase()
                .cmp(&b.1.as_deref().unwrap_or("").to_ascii_lowercase())
        });
        let mut neg_id = -1;
        for (name, dev_id, pnp_class) in missing {
            comps.push(DriverComponent {
                id: neg_id,
                name: "MissingDevice".to_string(),
                display_name: name
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| {
                        t("Неизвестное устройство", "Unknown device", "未知设备").to_string()
                    }),
                current_version: "0".to_string(),
                package_type: pnp_class.unwrap_or_default(),
                device_name: dev_id,
                app_name: None,
            });
            neg_id -= 1;
        }
    }

    let mut seen = HashSet::new();
    Ok(comps
        .into_iter()
        .filter(|c| seen.insert(c.id))
        .collect())
}

fn add_missing_catalog_components(installed: &[DriverComponent]) -> Vec<DriverComponent> {
    let mut out = installed.to_vec();
    let ids: HashSet<i32> = installed.iter().map(|c| c.id).collect();
    for entry in &COMPONENT_CATALOG {
        if !ids.contains(&entry.id) {
            out.push(DriverComponent {
                id: entry.id,
                name: entry.name.to_string(),
                display_name: entry.display_name.to_string(),
                current_version: "0".to_string(),
                package_type: String::new(),
                device_name: None,
                app_name: entry.app_name.map(str::to_string),
            });
        }
    }
    out
}

fn add_comp(
    out: &mut Vec<DriverComponent>,
    drivers: &[PnpDriver],
    id: i32,
    name: &str,
    display_name: &str,
    predicates: &[&dyn Fn(&PnpDriver) -> bool],
    pkg_type_fn: Option<&dyn Fn(&PnpDriver) -> String>,
    app_name: Option<&str>,
) {
    for pred in predicates {
        let matches: Vec<&PnpDriver> = drivers.iter().filter(|d| pred(d)).collect();
        if let Some(rep) = select_representative(&matches) {
            out.push(DriverComponent {
                id,
                name: name.to_string(),
                display_name: display_name.to_string(),
                current_version: rep.version.clone(),
                package_type: pkg_type_fn.map(|f| f(rep)).unwrap_or_default(),
                device_name: Some(rep.device_name.clone()),
                app_name: app_name.map(str::to_string),
            });
            return;
        }
    }
}

fn select_representative<'a>(matches: &[&'a PnpDriver]) -> Option<&'a PnpDriver> {
    if matches.is_empty() {
        return None;
    }
    let vendor: Vec<&PnpDriver> = matches
        .iter()
        .copied()
        .filter(|d| !is_generic_provider(d))
        .collect();
    let pool = if vendor.is_empty() { matches } else { &vendor };
    let mut groups: HashMap<String, Vec<&PnpDriver>> = HashMap::new();
    for &d in pool {
        groups
            .entry(d.version.to_ascii_lowercase())
            .or_default()
            .push(d);
    }
    let mut best_group = groups
        .into_values()
        .max_by(|a, b| {
            a.len().cmp(&b.len()).then_with(|| {
                grandhonor_core::version::compare_version_parts(
                    &version_parts(&a[0].version),
                    &version_parts(&b[0].version),
                )
            })
        })?;
    best_group.sort_by(|a, b| {
        a.device_name
            .to_ascii_lowercase()
            .cmp(&b.device_name.to_ascii_lowercase())
    });
    best_group.into_iter().next()
}

fn read_machine_identity() -> MachineIdentity {
    let _com = ComGuard::mta();
    // The firmware's own SMBIOS table first (code name `ZhuqueC`, SKU `C233`); HONOR's
    // registry copy only when the firmware does not carry them (it vanishes with HONOR's apps).
    let firmware = crate::hw::firmware::identity();
    let mut device_name = firmware
        .code_name()
        .or_else(|| settings::read_hklm_string(r"SOFTWARE\HONOR\BIOS", "DeviceTypeEx"));
    let mut c_version = firmware
        .sku
        .clone()
        .or_else(|| settings::read_hklm_string(r"SOFTWARE\HONOR\BIOS", "CVersion"));
    let mut ids = Vec::new();
    let mut seen = HashSet::new();
    let mut push_id = |val: Option<String>| {
        if let Some(s) = val {
            let trimmed = s.trim();
            if !trimmed.is_empty()
                && !trimmed.eq_ignore_ascii_case("To be filled by O.E.M.")
                && !trimmed.eq_ignore_ascii_case("Default string")
                && seen.insert(trimmed.to_ascii_uppercase())
            {
                ids.push(trimmed.to_string());
            }
        }
    };
    push_id(device_name.clone());
    push_id(c_version.clone());

    let mut sys_model = None;
    let mut sys_sku = None;
    let mut mem_bytes = 0u64;
    let mut cpu_name = String::new();

    if let Ok(services) = connect_wmi(CIMV2_WMI) {
        if let Ok(objs) = wmi_query(
            &services,
            "SELECT Model, SystemFamily, SystemSKUNumber, TotalPhysicalMemory FROM Win32_ComputerSystem",
        ) && let Some(sys) = objs.into_iter().next()
        {
            sys_model = get_prop_string(&sys, "Model");
            sys_sku = get_prop_string(&sys, "SystemSKUNumber");
            mem_bytes = get_prop_u64(&sys, "TotalPhysicalMemory").unwrap_or(0);
            push_id(sys_model.clone());
            push_id(get_prop_string(&sys, "SystemFamily"));
            push_id(sys_sku.clone());
        }
        if let Ok(objs) = wmi_query(&services, "SELECT Name, Version, SKUNumber FROM Win32_ComputerSystemProduct") {
            for p in objs {
                push_id(get_prop_string(&p, "Name"));
                push_id(get_prop_string(&p, "Version"));
                push_id(get_prop_string(&p, "SKUNumber"));
            }
        }
        if let Ok(objs) = wmi_query(&services, "SELECT Product FROM Win32_BaseBoard") {
            for b in objs {
                push_id(get_prop_string(&b, "Product"));
            }
        }
        if let Ok(objs) = wmi_query(&services, "SELECT Name FROM Win32_Processor") {
            for c in objs {
                if let Some(n) = get_prop_string(&c, "Name")
                    && !n.trim().is_empty()
                {
                    cpu_name = n;
                    break;
                }
            }
        }
    }

    if device_name.as_deref().is_none_or(|s| s.trim().is_empty()) {
        device_name = sys_model.or_else(|| {
            settings::read_hklm_string(r"HARDWARE\DESCRIPTION\System\BIOS", "SystemProductName")
        });
    }
    if c_version.as_deref().is_none_or(|s| s.trim().is_empty()) {
        c_version = sys_sku.or_else(|| {
            settings::read_hklm_string(r"HARDWARE\DESCRIPTION\System\BIOS", "SystemSKU")
        });
    }
    let mem_gb = (mem_bytes as f64 / (1024.0 * 1024.0 * 1024.0)).round() as i32;

    MachineIdentity {
        device_name: device_name
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "HONOR".to_string()),
        c_version: c_version.unwrap_or_default(),
        processor_name: cpu_name,
        memory_gb: mem_gb,
        identifiers: ids,
    }
}

fn has(d: &PnpDriver, value: &str) -> bool {
    if value.len() <= 4 && value.chars().all(|c| c.is_ascii_alphanumeric()) {
        has_word(&d.device_name, value)
    } else {
        contains_ci(&d.device_name, value)
    }
}

fn has_word(text: &str, value: &str) -> bool {
    let lower_text = text.to_ascii_lowercase();
    let lower_val = value.to_ascii_lowercase();
    let bytes = lower_text.as_bytes();
    let mut start = 0;
    while let Some(pos) = lower_text[start..].find(&lower_val) {
        let abs = start + pos;
        let end = abs + lower_val.len();
        let left_ok = abs == 0 || !bytes[abs - 1].is_ascii_alphanumeric();
        let right_ok = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if left_ok && right_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

fn id_has(d: &PnpDriver, val: &str) -> bool {
    contains_ci(&d.device_id, val)
}

fn class_is(d: &PnpDriver, val: &str) -> bool {
    d.device_class.eq_ignore_ascii_case(val)
}

fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

fn starts_ci(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len() && text[..prefix.len()].eq_ignore_ascii_case(prefix)
}

fn is_generic_provider(d: &PnpDriver) -> bool {
    d.provider.trim().is_empty()
        || contains_ci(&d.provider, "microsoft")
        || contains_ci(&d.provider, "standard")
}

fn is_display_adapter(d: &PnpDriver, vendor_id: &str) -> bool {
    (class_is(d, "DISPLAY") || has(d, "graphics") || has(d, "arc(tm)"))
        && id_has(d, vendor_id)
        && !is_generic_provider(d)
}

fn contains_memory_gb(description: &str, gb: i32) -> bool {
    if gb <= 0 {
        return false;
    }
    let bytes = description.as_bytes();
    let num = gb.to_string();
    let num_b = num.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';

    let mut i = 0;
    while i + num_b.len() + 2 <= bytes.len() {
        if bytes[i..].starts_with(num_b) && (i == 0 || !is_word(bytes[i - 1])) {
            let mut j = i + num_b.len();
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j + 2 <= bytes.len()
                && bytes[j..j + 2].eq_ignore_ascii_case(b"GB")
                && (j + 2 == bytes.len() || !is_word(bytes[j + 2]))
            {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn fingerprint_pkg_type(d: &PnpDriver) -> String {
    for w in d.device_id.as_bytes().windows(17) {
        if w[0..4].eq_ignore_ascii_case(b"VID_")
            && w[4..8].iter().all(u8::is_ascii_hexdigit)
            && w[8..13].eq_ignore_ascii_case(b"&PID_")
            && w[13..17].iter().all(u8::is_ascii_hexdigit)
            && let Ok(s) = std::str::from_utf8(w)
        {
            return s.to_ascii_uppercase();
        }
    }
    String::new()
}

fn normalize_identity(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

fn processor_identity_tokens(value: &str) -> Vec<String> {
    let bytes = value.as_bytes();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_alphanumeric() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-') {
            i += 1;
        }
        let mut end = i;
        while end > start && bytes[end - 1] == b'-' {
            end -= 1;
        }
        let raw = &bytes[start..end];
        if raw.len() >= 4
            && raw[..raw.len() - 2].iter().any(u8::is_ascii_digit)
            && let Ok(s) = std::str::from_utf8(raw)
        {
            let tok = normalize_identity(s);
            if tok.len() >= 4
                && tok.chars().any(|c| c.is_ascii_alphabetic())
                && tok.chars().any(|c| c.is_ascii_digit())
                && seen.insert(tok.clone())
            {
                out.push(tok);
            }
        }
    }
    out
}

fn support_component_id(title: &str, cat_id: &str, cat_name: &str) -> i32 {
    let text = format!("{title} {cat_name}");
    for (needle, id) in [
        ("camera", 76),
        ("isst", 65),
        ("intelme", 2),
        ("monitor", 41),
        ("mep", 78),
        ("serialio", 4),
        ("vhid", 88),
        ("dptf", 6),
        ("ppm", 55),
        ("txt", 56),
        ("npu", 74),
        ("nfc", 52),
        ("ipmt", 73),
        ("vdisplay", 87),
        ("wdt", 12),
    ] {
        if contains_ci(&text, needle) {
            return id;
        }
    }
    match cat_id {
        "631" => 1,
        "633" => 3,
        "634" => 15,
        "635" => 16,
        "637" => 18,
        "639" => 14,
        "6310" => 23,
        _ => 0,
    }
}

fn stable_support_component_id(value: &str) -> i32 {
    let mut hash: u32 = 2_166_136_261;
    for ch in value.chars() {
        for upper in ch.to_uppercase() {
            hash = (hash ^ (upper as u32)).wrapping_mul(16_777_619);
        }
    }
    1000 + (hash % 1_000_000) as i32
}

fn val_str(v: &Value, key: &str) -> Option<String> {
    let prop = v.get(key)?;
    if prop.is_null() {
        return None;
    }
    match prop {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        other => Some(other.to_string()),
    }
}

fn val_i32(v: &Value, key: &str) -> Option<i32> {
    val_str(v, key)?.trim().parse::<i32>().ok()
}

fn val_u64(v: &Value, key: &str) -> Option<u64> {
    val_str(v, key)?.trim().parse::<u64>().ok()
}

fn os_version_string() -> String {
    let ver = settings::read_hklm_string(
        r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
        "CurrentBuildNumber",
    )
    .unwrap_or_else(|| "26100".to_string());
    format!("Microsoft Windows NT 10.0.{ver}.0")
}

fn unique_token() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    format!(
        "{}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed),
        grandhonor_core::protocol::now_dotnet_ticks()
    )
}

fn run_bounded_pool<T, R, F>(items: Vec<T>, workers: usize, func: F) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Sync,
{
    if items.is_empty() {
        return Vec::new();
    }
    let count = workers.min(items.len()).max(1);
    let queue = Mutex::new(items.into_iter());
    let results = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..count {
            scope.spawn(|| loop {
                let next = queue.lock().ok().and_then(|mut q| q.next());
                let Some(item) = next else { break };
                let res = func(item);
                if let Ok(mut out) = results.lock() {
                    out.push(res);
                }
            });
        }
    });
    results.into_inner().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn support_packages_for_other_models_are_skipped() {
        let machine = MachineIdentity {
            device_name: "ZhuqueC".into(),
            c_version: "C233".into(),
            processor_name: String::new(),
            memory_gb: 32,
            identifiers: vec!["ZQC-P".into()],
        };
        assert_eq!(foreign_model("FermatB-5611-Alldriver 1.0", &machine).as_deref(), Some("FermatB-5611"));
        assert_eq!(foreign_model("ZhuqueC-XuanwuC-5211-BIOS 1.12(Service)", &machine), None);
        assert_eq!(foreign_model("ZhuqueC-5211-IntelNPU 32.0.100.4841", &machine), None);
        assert_eq!(foreign_model("24.10.0.4", &machine), None);
        assert_eq!(foreign_model("20260619", &machine), None);
        assert_eq!(foreign_model("PCManager_4IN1 20.0.0.50(SP3C233)", &machine), None);
    }

    #[test]
    fn ascii_matchers_without_regex() {
        assert!(contains_memory_gb("HONOR MagicBook Pro 14 Ultra 7 32GB 1TB", 32));
        assert!(contains_memory_gb("16 GB RAM", 16));
        assert!(!contains_memory_gb("132GB SSD", 32));
        assert_eq!(clean_catalog_label("Горячая кнопка\u{00A0} "), "Горячая кнопка");
        assert_eq!(
            processor_identity_tokens("Intel(R) Core(TM) Ultra 7 255H / Ryzen AI 9 HX 358H"),
            vec!["255H", "358H"]
        );
        let d = PnpDriver {
            device_name: String::new(),
            device_id: r"USB\VID_27C6&pid_0f9a\6&1234".to_string(),
            version: String::new(),
            provider: String::new(),
            device_class: String::new(),
        };
        assert_eq!(fingerprint_pkg_type(&d), "VID_27C6&PID_0F9A");
    }
}
