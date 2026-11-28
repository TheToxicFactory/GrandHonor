//! Optional shortcut editing in an unelevated panel process started by Explorer.

use grandhonor_core::locale::t;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, SelectObject,
};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize, IPersistFile, STGM_READ,
};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::{
    Common::COMDLG_FILTERSPEC, FOLDERID_Desktop, FOLDERID_LocalAppData, FOS_FILEMUSTEXIST,
    FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_NODEREFERENCELINKS, FileOpenDialog, IFileOpenDialog,
    IShellItem, IShellLinkW, KF_FLAG_DEFAULT, SHCNE_UPDATEITEM, SHCNF_PATHW, SHChangeNotify,
    SHFILEINFOW, SHGFI_ICON, SHGetFileInfoW, SHGetKnownFolderPath, SHCreateItemFromParsingName,
    SIGDN_FILESYSPATH, ShellLink,
};
use windows::Win32::UI::WindowsAndMessaging::{DI_NORMAL, DestroyIcon, DrawIconEx, HICON, MB_ICONERROR, MessageBoxW};
use windows::core::{GUID, HSTRING, Interface, w};

const SIDE: i32 = 32;
const MAX_SHORTCUT: u64 = 1024 * 1024;

#[derive(Deserialize, Serialize)]
struct Backup {
    original: Vec<u8>,
    modified: String,
}

struct Temporary(PathBuf);

impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn unelevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut size = 0;
        let result = GetTokenInformation(token, TokenElevation, Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
            size_of::<TOKEN_ELEVATION>() as u32, &mut size);
        let _ = CloseHandle(token);
        result.is_ok() && elevation.TokenIsElevated == 0
    }
}

fn folder(id: &GUID) -> windows::core::Result<PathBuf> {
    unsafe {
        let value = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None)?;
        let path = value.to_string();
        CoTaskMemFree(Some(value.0.cast()));
        Ok(PathBuf::from(path?))
    }
}

fn choose(desktop: &Path) -> windows::core::Result<Option<PathBuf>> {
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(desktop.as_os_str()), None)?;
        dialog.SetFolder(&item)?;
        dialog.SetOptions(dialog.GetOptions()? | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR | FOS_NODEREFERENCELINKS)?;
        dialog.SetTitle(&HSTRING::from(t("Выберите ярлык рабочего стола", "Select a desktop shortcut", "选择桌面快捷方式")))?;
        let file_type = HSTRING::from(t("Ярлык Windows", "Windows shortcut", "Windows 快捷方式"));
        dialog.SetFileTypes(&[COMDLG_FILTERSPEC {
            pszName: windows::core::PCWSTR(file_type.as_ptr()),
            pszSpec: w!("*.lnk"),
        }])?;
        if let Err(error) = dialog.Show(None) {
            if error.code().0 as u32 == 0x800704c7 {
                return Ok(None);
            }
            return Err(error);
        }
        let name = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = name.to_string();
        CoTaskMemFree(Some(name.0.cast()));
        Ok(Some(PathBuf::from(path?)))
    }
}

fn short_read(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut contents = Vec::new();
    fs::File::open(path)?.take(limit + 1).read_to_end(&mut contents)?;
    if contents.len() as u64 > limit {
        return Err(std::io::Error::other(t("Файл слишком большой.", "The file is too large.", "文件过大。")));
    }
    Ok(contents)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn staged(parent: &Path, label: &str) -> std::io::Result<(Temporary, fs::File)> {
    let extension = if label == "shortcut" { "lnk" } else { "tmp" };
    let path = parent.join(format!(".grandhonor-{}-{label}.{extension}", std::process::id()));
    let file = fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
    Ok((Temporary(path), file))
}

fn store(path: &Path, bytes: &[u8], label: &str) -> std::io::Result<()> {
    let (temp, mut file) = staged(path.parent().unwrap(), label)?;
    file.write_all(bytes)?;
    drop(file);
    fs::rename(&temp.0, path)
}

fn icon_on(icon: HICON, background: u32) -> Result<Vec<u32>, String> {
    unsafe {
        let format = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: SIDE,
                biHeight: SIDE, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return Err(t("Не удалось подготовить значок.", "Could not prepare the icon.", "无法准备图标。").into());
        }
        let mut pixels = std::ptr::null_mut();
        let result = (|| {
            let bitmap = CreateDIBSection(Some(dc), &format, DIB_RGB_COLORS, &mut pixels, None, 0).map_err(|err| err.to_string())?;
            let previous = SelectObject(dc, bitmap.into());
            let image = std::slice::from_raw_parts_mut(pixels.cast::<u32>(), (SIDE * SIDE) as usize);
            image.fill(background);
            let result = DrawIconEx(dc, 0, 0, icon, SIDE, SIDE, 0, None, DI_NORMAL).map(|()| image.to_vec());
            let _ = SelectObject(dc, previous);
            let _ = DeleteObject(bitmap.into());
            result.map_err(|err| err.to_string())
        })();
        let _ = DeleteDC(dc);
        result
    }
}

fn encode_icon(black: &[u32], white: &[u32]) -> Vec<u8> {
    let color_bytes = (SIDE * SIDE * 4) as u32;
    let mask_bytes = (SIDE * 4) as usize;
    let mut ico = vec![0, 0, 1, 0, 1, 0, SIDE as u8, SIDE as u8, 0, 0, 1, 0, 32, 0];
    ico.extend((40 + color_bytes + mask_bytes as u32).to_le_bytes());
    ico.extend(22u32.to_le_bytes());
    ico.extend(40u32.to_le_bytes());
    ico.extend(SIDE.to_le_bytes());
    ico.extend((2 * SIDE).to_le_bytes());
    ico.extend([1, 0, 32, 0]);
    ico.extend([0; 24]);
    let mut mask = vec![0; mask_bytes];
    for (index, (&dark, &light)) in black.iter().zip(white).enumerate() {
        let channel = |value: u32, shift| ((value >> shift) & 255u32) as i32;
        let difference = (channel(light, 0) - channel(dark, 0) + channel(light, 8) - channel(dark, 8)
            + channel(light, 16) - channel(dark, 16)) / 3;
        let alpha = (255 - difference).clamp(0, 255);
        if alpha == 0 {
            mask[index / 8] |= 0x80 >> (index % 8);
            ico.extend([0; 4]);
        } else {
            let luma = (54 * channel(dark, 16) + 183 * channel(dark, 8) + 19 * channel(dark, 0)) / 256;
            let gray = (34 + 109 * luma / alpha).clamp(34, 143) as u8;
            ico.extend([gray, gray, gray, alpha as u8]);
        }
    }
    ico.extend(mask);
    ico
}

fn monochrome_icon(path: &Path) -> Result<Vec<u8>, String> {
    let mut info = SHFILEINFOW::default();
    let result = unsafe {
        SHGetFileInfoW(&HSTRING::from(path.as_os_str()), FILE_FLAGS_AND_ATTRIBUTES(0), Some(&mut info),
            size_of::<SHFILEINFOW>() as u32, SHGFI_ICON)
    };
    if result == 0 || info.hIcon.is_invalid() {
        return Err(t("Не удалось прочитать значок ярлыка.", "Could not read the shortcut icon.", "无法读取快捷方式图标。").into());
    }
    let images = icon_on(info.hIcon, 0).and_then(|black| icon_on(info.hIcon, 0x00ffffff).map(|white| encode_icon(&black, &white)));
    unsafe { let _ = DestroyIcon(info.hIcon); }
    images
}

fn run_inner() -> Result<(), String> {
    let desktop = folder(&FOLDERID_Desktop).map_err(|e| e.to_string())?;
    let Some(selected) = choose(&desktop).map_err(|e| e.to_string())? else { return Ok(()) };
    let path = selected.canonicalize().map_err(|e| e.to_string())?;
    if path != selected && fs::symlink_metadata(&selected).is_ok_and(|info| info.is_symlink()) {
        return Err(t("Выберите ярлык, а не ссылку на файл.", "Select a shortcut file directly.", "请直接选择快捷方式文件。").into());
    }
    if !path.starts_with(desktop.canonicalize().map_err(|e| e.to_string())?)
        || !path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
    {
        return Err(t("Нужен ярлык .lnk в папке рабочего стола.", "Choose a .lnk file from your desktop folder.", "请选择桌面文件夹中的 .lnk 文件。").into());
    }
    let cache = folder(&FOLDERID_LocalAppData).map_err(|e| e.to_string())?.join("GrandHonor").join("ShortcutIcons");
    fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    let key = digest(path.to_string_lossy().to_lowercase().as_bytes());
    let state = cache.join(format!("{key}.json"));
    let icon = cache.join(format!("{key}.ico"));
    let original = short_read(&path, MAX_SHORTCUT).map_err(|e| e.to_string())?;
    if state.exists() {
        let record = short_read(&state, 5 * MAX_SHORTCUT).map_err(|e| e.to_string())?;
        let record: Backup = serde_json::from_slice(&record).map_err(|e| e.to_string())?;
        if record.modified == digest(&original) {
            store(&path, &record.original, "restore").map_err(|e| e.to_string())?;
        } else if record.original != original {
            return Err(t("Ярлык изменился. Его текущий значок сохранён.", "The shortcut has changed. Its current icon was kept.", "快捷方式已更改，保留当前图标。").into());
        }
        let _ = fs::remove_file(&state);
        let _ = fs::remove_file(&icon);
    } else {
        store(&icon, &monochrome_icon(&selected)?, "icon").map_err(|e| e.to_string())?;
        let (temp, file) = staged(path.parent().unwrap(), "shortcut").map_err(|e| e.to_string())?;
        drop(file);
        let result = unsafe {
            (|| -> windows::core::Result<()> {
                let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
                let file: IPersistFile = link.cast()?;
                file.Load(&HSTRING::from(selected.as_os_str()), STGM_READ)?;
                link.SetIconLocation(&HSTRING::from(icon.as_os_str()), 0)?;
                file.Save(&HSTRING::from(temp.0.as_os_str()), false)
            })()
        };
        result.map_err(|e| e.to_string())?;
        let modified = short_read(&temp.0, MAX_SHORTCUT).map_err(|e| e.to_string())?;
        if short_read(&path, MAX_SHORTCUT).map_err(|e| e.to_string())? != original {
            return Err(t("Ярлык изменился во время обработки.", "The shortcut changed during editing.", "快捷方式在处理过程中已更改。").into());
        }
        let backup = serde_json::to_vec(&Backup { original, modified: digest(&modified) }).map_err(|e| e.to_string())?;
        store(&state, &backup, "backup").map_err(|e| e.to_string())?;
        if let Err(err) = fs::rename(&temp.0, &path) {
            let _ = fs::remove_file(&state);
            return Err(err.to_string());
        }
    }
    unsafe {
        let filename = HSTRING::from(selected.as_os_str());
        SHChangeNotify(SHCNE_UPDATEITEM, SHCNF_PATHW, Some(filename.as_ptr().cast()), None);
    }
    Ok(())
}

pub fn run() {
    if !unelevated() {
        return;
    }
    unsafe {
        let Ok(instance) = CreateMutexW(None, false, w!("Local\\GrandHonor.ShortcutEditor")) else { return };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(instance);
            return;
        }
        if CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_err() {
            let _ = CloseHandle(instance);
            return;
        }
        let result = run_inner();
        CoUninitialize();
        if let Err(err) = result {
            let message = HSTRING::from(err);
            let _ = MessageBoxW(None, &message, w!("GrandHonor"), MB_ICONERROR);
        }
        let _ = CloseHandle(instance);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_keeps_transparency_and_stores_a_dark_neutral_color() {
        let mut black = vec![0; (SIDE * SIDE) as usize];
        let mut white = vec![0x00ffffff; black.len()];
        black[0] = 0x00ff0000;
        white[0] = black[0];
        let rendered = encode_icon(&black, &white);
        let first = &rendered[62..66];
        assert_eq!(first, &[56, 56, 56, 255]);
        assert_eq!(&rendered[66..70], &[0, 0, 0, 0]);
        assert_eq!(rendered[62 + black.len() * 4], 0x7f);
    }
}
