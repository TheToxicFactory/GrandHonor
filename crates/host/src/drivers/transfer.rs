use crate::hw::com::ComGuard;
use crate::scheduler::create_protected_directory;
use grandhonor_core::archive::{ArchiveBudget, create_zip_from_directory};
use grandhonor_core::locale::t;
use grandhonor_core::protocol::DriverTransferResult;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, FileOpenDialog, FileSaveDialog,
    IFileDialog, IFileOpenDialog, IFileSaveDialog, SIGDN_FILESYSPATH,
};
use windows_core::{HSTRING, Interface, PCWSTR};

const PIPE_PREFIX: &str = r"\\.\pipe\GrandHonor.DriverTransfer.";
const REBOOT_REQUIRED_EXIT_CODE: i32 = 3010;

pub fn save_file_dialog(
    title: &str,
    default_file: &str,
    filter_label: &str,
    filter_spec: &str,
    default_ext: &str,
) -> Result<Option<PathBuf>, String> {
    let title = title.to_string();
    let default_file = default_file.to_string();
    let filter_label = filter_label.to_string();
    let filter_spec = filter_spec.to_string();
    let default_ext = default_ext.to_string();

    std::thread::spawn(move || {
        let _com = ComGuard::sta();
        let dialog: IFileSaveDialog =
            unsafe { CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| format!("FileSaveDialog failed: {e}"))?;
        let base: IFileDialog = dialog.cast().map_err(|e| e.to_string())?;
        let title_h = HSTRING::from(title);
        let file_h = HSTRING::from(default_file);
        let ext_h = HSTRING::from(default_ext);
        let flabel_h = HSTRING::from(filter_label);
        let fspec_h = HSTRING::from(filter_spec);
        let filters = [COMDLG_FILTERSPEC {
            pszName: PCWSTR(flabel_h.as_ptr()),
            pszSpec: PCWSTR(fspec_h.as_ptr()),
        }];

        unsafe {
            base.SetTitle(&title_h).map_err(|e| e.to_string())?;
            base.SetFileName(&file_h).map_err(|e| e.to_string())?;
            base.SetDefaultExtension(&ext_h).map_err(|e| e.to_string())?;
            base.SetFileTypes(&filters).map_err(|e| e.to_string())?;
            let opts = base.GetOptions().map_err(|e| e.to_string())?;
            base.SetOptions(opts | FOS_OVERWRITEPROMPT | FOS_FORCEFILESYSTEM)
                .map_err(|e| e.to_string())?;
            match base.Show(None) {
                Ok(()) => {
                    let item = base.GetResult().map_err(|e| e.to_string())?;
                    let pwstr = item.GetDisplayName(SIGDN_FILESYSPATH).map_err(|e| e.to_string())?;
                    let path = pwstr.to_string().map_err(|e| e.to_string())?;
                    CoTaskMemFree(Some(pwstr.as_ptr() as *const _));
                    Ok(Some(PathBuf::from(path)))
                }
                Err(err) if err.code().0 as u32 == 0x8007_04C7 => Ok(None), // ERROR_CANCELLED
                Err(err) => Err(err.to_string()),
            }
        }
    })
    .join()
    .map_err(|_| "Dialog thread panicked".to_string())?
}

pub fn open_file_dialog(
    title: &str,
    filter_label: &str,
    filter_spec: &str,
) -> Result<Option<PathBuf>, String> {
    let title = title.to_string();
    let filter_label = filter_label.to_string();
    let filter_spec = filter_spec.to_string();

    std::thread::spawn(move || {
        let _com = ComGuard::sta();
        let dialog: IFileOpenDialog =
            unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| format!("FileOpenDialog failed: {e}"))?;
        let base: IFileDialog = dialog.cast().map_err(|e| e.to_string())?;
        let title_h = HSTRING::from(title);
        let flabel_h = HSTRING::from(filter_label);
        let fspec_h = HSTRING::from(filter_spec);
        let filters = [COMDLG_FILTERSPEC {
            pszName: PCWSTR(flabel_h.as_ptr()),
            pszSpec: PCWSTR(fspec_h.as_ptr()),
        }];

        unsafe {
            base.SetTitle(&title_h).map_err(|e| e.to_string())?;
            base.SetFileTypes(&filters).map_err(|e| e.to_string())?;
            let opts = base.GetOptions().map_err(|e| e.to_string())?;
            base.SetOptions(opts | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM)
                .map_err(|e| e.to_string())?;
            match base.Show(None) {
                Ok(()) => {
                    let item = base.GetResult().map_err(|e| e.to_string())?;
                    let pwstr = item.GetDisplayName(SIGDN_FILESYSPATH).map_err(|e| e.to_string())?;
                    let path = pwstr.to_string().map_err(|e| e.to_string())?;
                    CoTaskMemFree(Some(pwstr.as_ptr() as *const _));
                    Ok(Some(PathBuf::from(path)))
                }
                Err(err) if err.code().0 as u32 == 0x8007_04C7 => Ok(None),
                Err(err) => Err(err.to_string()),
            }
        }
    })
    .join()
    .map_err(|_| "Dialog thread panicked".to_string())?
}

pub fn export_cli(archive_path: Option<&str>, result_id: Option<&str>) -> i32 {
    let Some(path) = archive_path.filter(|s| !s.trim().is_empty()) else {
        return 2;
    };
    let result = export_drivers(Path::new(path));
    complete_transfer(result, result_id)
}

pub fn import_cli(source_path: Option<&str>, result_id: Option<&str>) -> i32 {
    let Some(path) = source_path.filter(|s| !s.trim().is_empty()) else {
        return 2;
    };
    let result = import_drivers(Path::new(path));
    complete_transfer(result, result_id)
}

pub fn export_drivers(archive_path: &Path) -> DriverTransferResult {
    let staging = make_staging_dir();
    let res = (|| {
        create_protected_directory(&staging, false)?;
        let staging_str = staging.to_string_lossy().into_owned();
        let (exit_code, output) = run_pnputil(&["/export-driver", "*", &staging_str])?;
        let packages = count_inf_files(&staging);
        if exit_code != 0 || packages == 0 {
            return Err(format!(
                "{}{}",
                t(
                    "Windows не вернула ни одного стороннего драйвера для выгрузки.",
                    "Windows returned no third-party drivers to export.",
                    "Windows 未返回可导出的第三方驱动程序。"
                ),
                detail(exit_code, &output)
            ));
        }

        let target = std::path::absolute(archive_path).map_err(|e| e.to_string())?;
        let dir = target.parent().ok_or("Invalid archive destination")?;
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let temp_zip = dir.join(format!(".GrandHonor-{}.tmp", unique_id()));
        create_zip_from_directory(&staging, &temp_zip).map_err(|e| e.to_string())?;
        if let Err(err) = fs::rename(&temp_zip, &target) {
            let _ = fs::remove_file(&temp_zip);
            return Err(err.to_string());
        }
        crate::log::info(&format!(
            "Exported {packages} driver package(s) to {}",
            archive_path.to_string_lossy()
        ));
        Ok(DriverTransferResult {
            succeeded: true,
            package_count: packages as i32,
            reboot_required: false,
            error: None,
        })
    })();
    let _ = fs::remove_dir_all(&staging);
    match res {
        Ok(r) => r,
        Err(err) => fail(err),
    }
}

pub fn import_drivers(source_path: &Path) -> DriverTransferResult {
    let staging = make_staging_dir();
    let res = (|| {
        let is_inf = source_path.is_file()
            && source_path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("inf"));
        let (args, packages) = if is_inf {
            let full = std::path::absolute(source_path).map_err(|e| e.to_string())?;
            (
                vec![
                    "/add-driver".to_string(),
                    full.to_string_lossy().into_owned(),
                    "/install".to_string(),
                ],
                1usize,
            )
        } else {
            let directory = if source_path.is_file() {
                create_protected_directory(&staging, false)?;
                let mut budget = ArchiveBudget::default();
                budget
                    .extract(source_path, &staging)
                    .map_err(|e| e.to_string())?;
                staging.clone()
            } else if source_path.is_dir() {
                source_path.to_path_buf()
            } else {
                return Err(t(
                    "Файл или папка с драйверами не найдены.",
                    "The driver file or folder was not found.",
                    "未找到驱动程序文件或文件夹。",
                )
                .to_string());
            };

            let count = count_inf_files(&directory);
            if count == 0 {
                return Err(t(
                    "В выбранном источнике нет файлов драйверов (*.inf).",
                    "The selected source contains no driver files (*.inf).",
                    "所选来源中没有驱动程序文件 (*.inf)。",
                )
                .to_string());
            }
            let pattern = std::path::absolute(&directory)
                .map_err(|e| e.to_string())?
                .join("*.inf");
            (
                vec![
                    "/add-driver".to_string(),
                    pattern.to_string_lossy().into_owned(),
                    "/subdirs".to_string(),
                    "/install".to_string(),
                ],
                count,
            )
        };

        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let (exit_code, output) = run_pnputil(&arg_refs)?;
        let reboot = exit_code == REBOOT_REQUIRED_EXIT_CODE;
        if exit_code != 0 && !reboot {
            return Err(format!(
                "{}{}",
                t(
                    "Windows отклонила установку драйверов.",
                    "Windows rejected the driver installation.",
                    "Windows 拒绝了驱动程序安装。"
                ),
                detail(exit_code, &output)
            ));
        }

        crate::log::info(&format!(
            "Imported {packages} driver package(s) from {}",
            source_path.to_string_lossy()
        ));
        Ok(DriverTransferResult {
            succeeded: true,
            package_count: packages as i32,
            reboot_required: reboot,
            error: None,
        })
    })();
    let _ = fs::remove_dir_all(&staging);
    match res {
        Ok(r) => r,
        Err(err) => fail(err),
    }
}

fn run_pnputil(args: &[&str]) -> Result<(i32, String), String> {
    let sys_dir = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join("pnputil.exe");

    crate::log::info(&format!("Running pnputil {}", args.join(" ")));
    let output = Command::new(&sys_dir)
        .args(args)
        .output()
        .map_err(|_| {
            t(
                "Не удалось запустить pnputil.",
                "Could not start pnputil.",
                "无法启动 pnputil。",
            )
            .to_string()
        })?;
    let code = output.status.code().unwrap_or(1);
    crate::log::info(&format!("pnputil finished with code {code}"));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok((code, combined))
}

fn detail(exit_code: i32, output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let tail = lines
        .iter()
        .rev()
        .take(3)
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    if tail.is_empty() {
        format!(" (pnputil: {exit_code})")
    } else {
        format!(" (pnputil: {exit_code}) {tail}")
    }
}

fn fail(err: String) -> DriverTransferResult {
    crate::log::error(&format!("Driver transfer failed: {err}"));
    DriverTransferResult {
        succeeded: false,
        package_count: 0,
        reboot_required: false,
        error: Some(err),
    }
}

fn complete_transfer(result: DriverTransferResult, result_id: Option<&str>) -> i32 {
    let code = if result.succeeded { 0 } else { 1 };
    if let Some(id) = result_id
        && id.len() == 32
        && id.chars().all(|c| c.is_ascii_hexdigit())
    {
        let pipe_path = format!("{PIPE_PREFIX}{id}");
        if let Ok(mut f) = OpenOptions::new().write(true).open(&pipe_path)
            && let Ok(json) = serde_json::to_vec(&result)
        {
            let _ = f.write_all(&json);
        }
    }
    code
}

fn count_inf_files(root: &Path) -> usize {
    let mut total = 0usize;
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            total += count_inf_files(&p);
        } else if p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("inf"))
        {
            total += 1;
        }
    }
    total
}

fn unique_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn make_staging_dir() -> PathBuf {
    std::env::temp_dir().join(format!("GrandHonor-{}", unique_id()))
}
