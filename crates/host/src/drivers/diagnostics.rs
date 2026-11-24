use super::catalog::{
    DriverCheckResult, DriverComponent, DriverUpdate, check_drivers, download_and_run_verified_installer,
    download_and_save_dialog, verify_download_to_temp,
};
use super::transfer::{export_drivers, import_drivers, open_file_dialog, save_file_dialog};
use crate::menu_builder::SubMenu;
use grandhonor_core::locale::t;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use windows::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_DEFBUTTON2, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO,
    MessageBoxW,
};
use windows_core::HSTRING;

const BIOS_ID: i32 = 23;

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct CliDiagOutput<'a> {
    is_complete: bool,
    component_count: usize,
    packages: Vec<CliPackage<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct CliPackage<'a> {
    id: i32,
    display_name: &'a str,
    current_version: &'a str,
    version: &'a str,
    is_update: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct CliVerifyOutput<'a> {
    id: i32,
    version: &'a str,
    extension: String,
    size: u64,
}

pub fn run_cli(component: Option<i32>) -> i32 {
    let result = match check_drivers() {
        Ok(r) => r,
        Err(err) => {
            crate::log::error(&format!("Driver diagnostics failed: {err}"));
            eprintln!("{err}");
            return 1;
        }
    };

    if let Some(id) = component {
        let Some(pkg) = result.updates.iter().find(|u| u.component.id == id) else {
            return 2;
        };
        match verify_download_to_temp(pkg) {
            Ok((extension, size)) => {
                let out = CliVerifyOutput {
                    id: pkg.component.id,
                    version: &pkg.version,
                    extension,
                    size,
                };
                if let Ok(json) = serde_json::to_string(&out) {
                    println!("{json}");
                }
                0
            }
            Err(err) => {
                crate::log::error(&format!("Driver verification failed: {err}"));
                eprintln!("{err}");
                1
            }
        }
    } else {
        let out = CliDiagOutput {
            is_complete: result.is_complete,
            component_count: result.components.len(),
            packages: result
                .updates
                .iter()
                .map(|u| CliPackage {
                    id: u.component.id,
                    display_name: &u.component.display_name,
                    current_version: &u.component.current_version,
                    version: &u.version,
                    is_update: u.is_update,
                })
                .collect(),
        };
        if let Ok(json) = serde_json::to_string(&out) {
            println!("{json}");
        }
        if result.is_complete { 0 } else { 1 }
    }
}

#[derive(Default)]
struct PanelData {
    result: Option<DriverCheckResult>,
    message: Option<String>,
}

#[derive(Clone, Default)]
pub struct DriverPanel {
    data: Arc<Mutex<PanelData>>,
}

impl DriverPanel {
    pub fn pending_update_count(&self) -> usize {
        self.data
            .lock()
            .ok()
            .and_then(|d| d.result.as_ref().map(|r| r.updates.iter().filter(|u| u.is_update).count()))
            .unwrap_or(0)
    }

    pub fn build(&self, page: &mut SubMenu<'_>) {
        let this = self.clone();
        page.add_command_ex(
            t("Проверить обновления", "Check for updates", "检查更新"),
            Some(Box::new(move || this.scan())),
            Some(t(
                "Сверить версии драйверов и BIOS с сервером HONOR",
                "Compare installed drivers and BIOS against official HONOR servers",
                "将已安装驱动和 BIOS 与 HONOR 官方服务器比较",
            )),
            Some("HONOR"),
            Some("cta"),
            true,
            false,
        );

        let snapshot = self.data.lock().ok().map(|d| (d.message.clone(), d.result.clone()));
        if let Some((msg, res)) = snapshot {
            if let Some(m) = msg {
                page.add_command_ex(&m, None, None, None, Some("banner"), true, false);
            }
            if let Some(r) = res {
                let available: HashMap<i32, &DriverUpdate> =
                    r.updates.iter().map(|u| (u.component.id, u)).collect();
                let updates: Vec<&DriverComponent> = r
                    .components
                    .iter()
                    .filter(|c| available.get(&c.id).is_some_and(|u| u.is_update))
                    .collect();
                let others: Vec<&DriverComponent> = r
                    .components
                    .iter()
                    .filter(|c| available.get(&c.id).is_none_or(|u| !u.is_update))
                    .collect();

                if !updates.is_empty() {
                    page.add_item(
                        t("Доступные обновления", "Available updates", "可用更新"),
                        None,
                        false,
                        false,
                        None,
                    );
                    self.add_driver_cards(page, updates, &available, true);
                }

                if !others.is_empty() {
                    let badge = others.len().to_string();
                    let mut section = page.add_submenu_ex(
                        t("Все остальные компоненты", "Other components", "其他组件"),
                        Some(t(
                            "Установленные драйверы и пакеты каталога",
                            "Installed drivers and catalogue packages",
                            "已安装的驱动程序和目录软件包",
                        )),
                        Some(&badge),
                        Some("collection"),
                    );
                    self.add_driver_cards(&mut section, others, &available, false);
                }
            }
        }

        page.add_separator();
        page.add_item(
            t("Резервная копия хранилища", "Driver store backup", "驱动备份与还原"),
            None,
            false,
            false,
            None,
        );
        let mut tools = page.add_container("", "row-2");
        let this_exp = self.clone();
        tools.add_command_ex(
            t("Экспорт", "Export", "导出驱动"),
            Some(Box::new(move || this_exp.export())),
            Some(t("Сохранить архив .zip", "Save driver .zip", "保存 .zip 备份")),
            Some(".zip"),
            Some("card"),
            true,
            false,
        );
        let this_imp = self.clone();
        tools.add_command_ex(
            t("Импорт", "Import", "导入驱动"),
            Some(Box::new(move || this_imp.import())),
            Some(t("Восстановить через pnputil", "Install from .zip/.inf", "从 .zip/.inf 导入")),
            Some("pnputil"),
            Some("card"),
            true,
            false,
        );
    }

    fn add_driver_cards(
        &self,
        target: &mut SubMenu<'_>,
        mut rows: Vec<&DriverComponent>,
        available: &HashMap<i32, &DriverUpdate>,
        is_update_group: bool,
    ) {
        rows.sort_by(|a, b| {
            (a.id != BIOS_ID)
                .cmp(&(b.id != BIOS_ID))
                .then_with(|| a.display_name.to_ascii_lowercase().cmp(&b.display_name.to_ascii_lowercase()))
        });
        for comp in rows {
            let update = available.get(&comp.id).copied();
            let cur_ver = if comp.current_version == "0" {
                "—"
            } else {
                &comp.current_version
            };
            let badge = match update {
                Some(u) if u.is_update => format!("{cur_ver} → {}", u.version),
                Some(u) if cur_ver == "—" => u.version.clone(),
                Some(_) | None => cur_ver.to_string(),
            };
            let mut details = if is_update_group {
                update
                    .and_then(|u| u.release_date.as_ref().map(|d| format!("{} {d}", t("От", "Released", "发布于"))))
                    .unwrap_or_else(|| t("Проверенный пакет HONOR", "Verified HONOR package", "已验证的 HONOR 软件包").to_string())
            } else {
                let mut text = format!("{}{cur_ver}", t("Установлено: ", "Installed: ", "已安装："));
                if let Some(u) = update {
                    text.push_str("  ·  ");
                    text.push_str(t("Каталог: ", "Catalogue: ", "目录："));
                    text.push_str(&u.version);
                    if let Some(date) = &u.release_date {
                        text.push_str(&format!(" ({date})"));
                    }
                }
                text
            };
            if let Some(dev) = &comp.device_name
                && !dev.eq_ignore_ascii_case(&comp.display_name)
            {
                details.push_str("  ·  ");
                details.push_str(dev);
            }

            let kind = if is_update_group { "driver-update" } else { "driver" };
            let Some(upd) = update.cloned() else {
                target.add_command_ex(&comp.display_name, None, Some(&details), Some(&badge), Some("driver"), true, false);
                continue;
            };

            let mut item = target.add_submenu_ex(&comp.display_name, Some(&details), Some(&badge), Some(kind));
            if comp.id != BIOS_ID && upd.is_update {
                let u_inst = upd.clone();
                let this_inst = self.clone();
                item.add_command_ex(
                    t("Установить обновление", "Install verified update", "安装已验证更新"),
                    Some(Box::new(move || this_inst.install_direct(&u_inst, false))),
                    Some(t(
                        "Скачивание в защищённую папку Windows Temp и проверка SHA-256 / подписи",
                        "Downloads into protected Windows Temp, verifies SHA-256 and Authenticode",
                        "下载到受保护目录并验证 SHA-256 和数字签名",
                    )),
                    Some("WinVerifyTrust"),
                    Some("cta"),
                    true,
                    false,
                );
            }
            if comp.id == BIOS_ID {
                item.add_command_ex(
                    t("Перед обновлением BIOS", "Before updating the BIOS", "更新 BIOS 前"),
                    None,
                    Some(t(
                        "Подключите зарядку и сохраните работу. Не выключайте ноутбук, пока идёт обновление: он сам перезагрузится.",
                        "Plug in the charger and save your work. Do not turn the laptop off while it updates; it restarts by itself.",
                        "请接通电源并保存工作。更新期间请勿关机，笔记本会自动重启。",
                    )),
                    Some("BIOS"),
                    Some("banner"),
                    true,
                    false,
                );
                if upd.is_update {
                    let u_bios = upd.clone();
                    let this_bios = self.clone();
                    item.add_command_ex(
                        t("Установить BIOS", "Install BIOS update", "安装 BIOS 更新"),
                        Some(Box::new(move || this_bios.install_bios(&u_bios))),
                        Some(t(
                            "Только от сети и при заряде от 30%. Пакет проверяется по подписи и SHA-256",
                            "AC power and 30% charge required. Signature and SHA-256 are verified",
                            "需接通电源且电量不低于 30%。将验证签名和 SHA-256",
                        )),
                        None,
                        Some("cta"),
                        true,
                        false,
                    );
                }
            }

            let u_save = upd.clone();
            let this_save = self.clone();
            item.add_command_ex(
                t("Сохранить проверенный пакет", "Save verified package", "保存已验证的软件包"),
                Some(Box::new(move || this_save.save_package(&u_save))),
                Some(t(
                    "Скачать, проверить целостность и сохранить локально",
                    "Download, verify integrity and choose destination folder",
                    "下载并验证完整性后保存到本地",
                )),
                Some(".exe"),
                Some(if comp.id == BIOS_ID { "cta" } else { "card" }),
                true,
                false,
            );
        }
    }

    fn set_message(&self, msg: String) {
        if let Ok(mut d) = self.data.lock() {
            d.message = Some(msg);
        }
    }

    fn scan(&self) {
        match check_drivers() {
            Ok(res) => {
                let count = res.updates.iter().filter(|u| u.is_update).count();
                let msg = if res.is_complete {
                    match grandhonor_core::locale::language() {
                        grandhonor_core::locale::AppLanguage::Russian => format!("Проверка завершена · обновлений: {count}"),
                        grandhonor_core::locale::AppLanguage::Chinese => format!("检查完成 · 可用更新：{count}"),
                        grandhonor_core::locale::AppLanguage::English => format!("Scan complete · {count} update(s)"),
                    }
                } else {
                    t(
                        "Каталог ответил не полностью. Часть пакетов может отсутствовать.",
                        "The catalogue returned incomplete results.",
                        "目录返回了不完整的结果。",
                    )
                    .to_string()
                };
                if let Ok(mut d) = self.data.lock() {
                    d.result = Some(res);
                    d.message = Some(msg);
                }
            }
            Err(err) => {
                crate::log::error(&format!("Driver check failed: {err}"));
                self.set_message(
                    t(
                        "Проверка не завершилась. Проверьте подключение к сети.",
                        "The check failed. Check your network connection.",
                        "检查失败，请检查网络连接。",
                    )
                    .to_string(),
                );
            }
        }
    }

    fn save_package(&self, update: &DriverUpdate) {
        match download_and_save_dialog(update) {
            Ok(Some(_)) => {
                self.set_message(
                    t(
                        "Проверенный пакет сохранён.",
                        "Verified package saved.",
                        "已保存验证过的软件包。",
                    )
                    .to_string(),
                );
            }
            Ok(None) => {}
            Err(err) => {
                crate::log::error(&format!("Driver download failed: {err}"));
                self.set_message(err);
            }
        }
    }

    /// BIOS flashing is refused unless the charger is connected and the battery has 30 %:
    /// a power loss during the flash can leave the laptop unable to start.
    fn install_bios(&self, update: &DriverUpdate) {
        if !bios_power_ok() {
            self.set_message(
                t(
                    "Для обновления BIOS подключите зарядку; заряд должен быть не ниже 30%.",
                    "Connect the charger; the battery must be at least 30% to update the BIOS.",
                    "请接通电源，且电量需不低于 30% 才能更新 BIOS。",
                )
                .to_string(),
            );
            return;
        }
        self.install_direct(update, true);
    }

    fn install_direct(&self, update: &DriverUpdate, bios: bool) {
        let confirm = |display_name: &str| -> bool {
            // The download can take minutes: check the power again right before flashing.
            if bios && !bios_power_ok() {
                show_power_refusal();
                return false;
            }
            let prompt = match grandhonor_core::locale::language() {
                grandhonor_core::locale::AppLanguage::Russian => {
                    format!("Пакет проверен (подпись HONOR/поставщика действительна).\n\nЗапустить установщик «{display_name}»?")
                }
                grandhonor_core::locale::AppLanguage::Chinese => {
                    format!("软件包数字签名有效。\n\n是否运行 {display_name} 安装程序？")
                }
                grandhonor_core::locale::AppLanguage::English => {
                    format!("The downloaded package passed signature verification.\n\nRun the {display_name} installer?")
                }
            };
            let prompt_h = HSTRING::from(prompt);
            let title_h = HSTRING::from("GrandHonor");
            let answer = unsafe {
                MessageBoxW(
                    None,
                    &prompt_h,
                    &title_h,
                    MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
                )
            };
            answer == IDYES
        };

        match download_and_run_verified_installer(update, confirm) {
            Ok(None) => {}
            Ok(Some(code @ (0 | 3010 | 1641))) => {
                self.scan();
                self.set_message(if code == 3010 || code == 1641 {
                    t(
                        "Установщик завершён. Требуется перезагрузка.",
                        "Installer succeeded. Restart required.",
                        "安装完成，需要重启。",
                    )
                    .to_string()
                } else {
                    t(
                        "Драйвер установлен. Список версий обновлён.",
                        "Driver installed. Versions refreshed.",
                        "驱动已安装，版本已刷新。",
                    )
                    .to_string()
                });
            }
            Ok(Some(code)) => {
                self.set_message(match grandhonor_core::locale::language() {
                    grandhonor_core::locale::AppLanguage::Russian => {
                        format!("Установщик завершился с кодом {code}.")
                    }
                    grandhonor_core::locale::AppLanguage::Chinese => {
                        format!("安装程序返回代码 {code}。")
                    }
                    grandhonor_core::locale::AppLanguage::English => {
                        format!("The installer returned {code}.")
                    }
                });
            }
            Err(err) => {
                crate::log::error(&format!("Driver installation failed: {err}"));
                self.set_message(err);
            }
        }
    }

    fn export(&self) {
        let Ok(Some(path)) = save_file_dialog(
            t("Экспорт драйверов", "Export drivers", "导出驱动"),
            "GrandHonor-drivers.zip",
            "ZIP (*.zip)",
            "*.zip",
            "zip",
        ) else {
            return;
        };
        let res = export_drivers(&path);
        self.apply_transfer_result(res, false);
    }

    fn import(&self) {
        let Ok(Some(path)) = open_file_dialog(
            t("Импорт драйверов", "Import drivers", "导入驱动"),
            "Drivers (*.zip;*.inf)",
            "*.zip;*.inf",
        ) else {
            return;
        };
        let res = import_drivers(&path);
        self.apply_transfer_result(res, true);
    }

    fn apply_transfer_result(&self, res: grandhonor_core::protocol::DriverTransferResult, imported: bool) {
        if res.succeeded {
            if imported {
                self.scan();
            }
            let msg = if res.reboot_required {
                t(
                    "Операция завершена. Нужна перезагрузка.",
                    "Driver transfer completed. Restart required.",
                    "驱动传输已完成，需要重启。",
                )
                .to_string()
            } else {
                let count = res.package_count;
                match grandhonor_core::locale::language() {
                    grandhonor_core::locale::AppLanguage::Russian => format!("Обработано пакетов: {count}"),
                    grandhonor_core::locale::AppLanguage::Chinese => format!("已处理软件包：{count}"),
                    grandhonor_core::locale::AppLanguage::English => format!("Packages processed: {count}"),
                }
            };
            self.set_message(msg);
        } else if let Some(err) = res.error {
            self.set_message(err);
        }
    }
}

/// BIOS flashing needs the charger and a known charge of at least 30 % (an unknown charge,
/// 255, is refused): a power loss while flashing can leave the laptop unable to start.
fn bios_power_ok() -> bool {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut status = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut status) }.is_ok()
        && status.ACLineStatus == 1
        && status.BatteryLifePercent != 255
        && status.BatteryLifePercent >= 30
}

fn show_power_refusal() {
    let text = HSTRING::from(t(
        "Для обновления BIOS подключите зарядку; заряд должен быть не ниже 30%.",
        "Connect the charger; the battery must be at least 30% to update the BIOS.",
        "请接通电源，且电量需不低于 30% 才能更新 BIOS。",
    ));
    unsafe {
        MessageBoxW(None, &text, &HSTRING::from("GrandHonor"), MB_OK | MB_ICONINFORMATION | MB_TOPMOST | MB_SETFOREGROUND);
    }
}
