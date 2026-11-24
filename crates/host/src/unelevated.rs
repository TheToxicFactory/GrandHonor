//! Opens files, programs and protocol links as the signed-in user, never with GrandHonor's
//! administrator rights. The host runs elevated, so a plain `ShellExecute` would hand those
//! rights to whatever a user-writable setting or `HKCU\Software\Classes` handler names. Here
//! Explorer (which runs unelevated) is asked to do the opening, through the desktop's
//! `IShellDispatch2::ShellExecute` (the documented "launch unelevated" route). If Explorer is
//! not available nothing is started.

use std::mem::ManuallyDrop;
use std::path::Path;
use windows::Win32::System::Com::{
    CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IDispatch,
    IServiceProvider,
};
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_BSTR, VT_EMPTY, VT_I4};
use windows::Win32::UI::Shell::{
    IShellBrowser, IShellDispatch2, IShellFolderViewDual, IShellView, IShellWindows, SID_STopLevelBrowser,
    SVGIO_BACKGROUND, SWC_DESKTOP, SWFO_NEEDDISPATCH, ShellWindows,
};
use windows_core::{BSTR, Interface};

fn empty() -> VARIANT {
    VARIANT::default()
}

fn bstr(text: &str) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_BSTR,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { bstrVal: ManuallyDrop::new(BSTR::from(text)) },
            }),
        },
    }
}

fn int(value: i32) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_I4,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { lVal: value },
            }),
        },
    }
}

/// Frees a BSTR held in a hand-built VARIANT.
fn clear(mut v: VARIANT) {
    unsafe {
        let inner = &mut v.Anonymous.Anonymous;
        if inner.vt == VT_BSTR {
            ManuallyDrop::drop(&mut inner.Anonymous.bstrVal);
        }
        inner.vt = VT_EMPTY;
    }
}

fn desktop_shell() -> windows_core::Result<IShellDispatch2> {
    unsafe {
        let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER)?;
        let (loc, root) = (empty(), empty());
        let mut hwnd = 0i32;
        let disp: IDispatch = windows.FindWindowSW(&loc, &root, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH)?;
        let provider: IServiceProvider = disp.cast()?;
        let browser: IShellBrowser = provider.QueryService(&SID_STopLevelBrowser)?;
        let view: IShellView = browser.QueryActiveShellView()?;
        let background: IDispatch = view.GetItemObject(SVGIO_BACKGROUND)?;
        let folder: IShellFolderViewDual = background.cast()?;
        folder.Application()?.cast()
    }
}

fn run(file: String, args: String, dir: String) -> Result<(), String> {
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let result = (|| {
            let shell = desktop_shell().map_err(|e| format!("Explorer is not available: {e}"))?;
            let (a, d, op, show) = (bstr(&args), bstr(&dir), bstr("open"), int(1));
            let r = shell.ShellExecute(&BSTR::from(file.as_str()), &a, &d, &op, &show);
            for v in [a, d, op, show] {
                clear(v);
            }
            r.map_err(|e| e.to_string())
        })();
        if init.is_ok() {
            CoUninitialize();
        }
        result
    }
}

/// Opens `target` (a file, program or `scheme:` link) as the signed-in user, on a short-lived
/// thread of its own. Errors are logged.
pub fn open(target: &str) {
    open_with(target, "")
}

pub fn open_with(target: &str, args: &str) {
    let file = target.to_string();
    let args = args.to_string();
    let dir = Path::new(target).parent().filter(|p| p.is_absolute()).map(|p| p.display().to_string()).unwrap_or_default();
    let _ = std::thread::Builder::new().name("open-unelevated".into()).stack_size(256 * 1024).spawn(move || {
        if let Err(err) = run(file.clone(), args, dir) {
            crate::log::error(&format!("Could not open {file} as the user: {err}"));
        }
    });
}

/// True when Explorer's desktop can open things for the user (used by checks).
#[allow(dead_code)]
pub fn desktop_shell_ok() -> bool {
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let ok = desktop_shell().is_ok();
        if init.is_ok() {
            CoUninitialize();
        }
        ok
    }
}

/// A program a key may start: an absolute path to an existing .exe, .lnk, .bat or .cmd file.
pub fn is_launchable_program(path: &str) -> bool {
    let p = Path::new(path);
    p.is_absolute()
        && p.is_file()
        && p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| ["exe", "lnk", "bat", "cmd"].iter().any(|x| e.eq_ignore_ascii_case(x)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_absolute_program_files_are_launchable() {
        assert!(!super::is_launchable_program("notepad.exe"));
        assert!(!super::is_launchable_program(r"C:\Windows\win.ini"));
        assert!(super::is_launchable_program(r"C:\Windows\System32\notepad.exe"));
    }
}
