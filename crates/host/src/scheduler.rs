use crate::hw::com::{ComGuard, OwnedVariant};
use crate::settings;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{GetLastError, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
    DACL_SECURITY_INFORMATION, GetAce, GetAclInformation, GetTokenInformation, INHERIT_ONLY_ACE,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
    TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows::Win32::Storage::FileSystem::{
    FILE_APPEND_DATA, FILE_ATTRIBUTE_REPARSE_POINT, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA,
    FILE_WRITE_EA, GetFileAttributesW, INVALID_FILE_ATTRIBUTES,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegDeleteValueW,
    RegOpenKeyExW, RegQueryValueExW,
};
use windows::Win32::System::TaskScheduler::{
    IExecAction, ILogonTrigger, ITaskService, TASK_ACTION_EXEC, TASK_CREATE_OR_UPDATE,
    TASK_LOGON_INTERACTIVE_TOKEN, TASK_RUNLEVEL_HIGHEST, TASK_TRIGGER_LOGON, TaskScheduler,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_core::{BSTR, HSTRING, Interface, PCWSTR, PWSTR};

const TRUSTED_INSTALLER_SID: &str = "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";
const BUILTIN_ADMINS_SID: &str = "S-1-5-32-544";
const LOCAL_SYSTEM_SID: &str = "S-1-5-18";

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const WRITE_DELETE_MASK: u32 = FILE_WRITE_DATA.0
    | FILE_APPEND_DATA.0
    | FILE_WRITE_ATTRIBUTES.0
    | FILE_WRITE_EA.0
    | 0x0001_0000 // DELETE
    | 0x0000_0040 // FILE_DELETE_CHILD
    | 0x0004_0000 // WRITE_DAC
    | 0x0008_0000; // WRITE_OWNER

const LEGACY_RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "GrandHonor";

struct LocalFreeGuard(*mut c_void);
impl Drop for LocalFreeGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.0)));
            }
        }
    }
}

pub fn create_protected_directory(path: &Path, admins_and_system_only: bool) -> Result<(), String> {
    let sddl = if admins_and_system_only {
        "O:BAD:P(A;OICI;GA;;;BA)(A;OICI;GA;;;SY)"
    } else {
        let user_sid = current_user_sid()?;
        return create_directory_with_sddl(path, &format!("D:P(A;OICI;GA;;;{user_sid})"));
    };
    create_directory_with_sddl(path, sddl)
}

fn create_directory_with_sddl(path: &Path, sddl: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let sddl_h = HSTRING::from(sddl);
    let mut sd = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &sddl_h,
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
    }
    .map_err(|e| format!("ConvertStringSecurityDescriptor failed: {e}"))?;
    let _sd_guard = LocalFreeGuard(sd.0);

    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: false.into(),
    };
    let path_h = HSTRING::from(path.as_os_str());
    unsafe {
        windows::Win32::Storage::FileSystem::CreateDirectoryW(&path_h, Some(&sa))
    }
    .map_err(|e| format!("CreateDirectoryW failed: {e}"))?;
    Ok(())
}

pub fn current_user_sid() -> Result<String, String> {
    unsafe {
        let mut token = windows::Win32::Foundation::HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(|e| format!("OpenProcessToken failed: {e}"))?;
        let mut needed = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut needed);
        let mut buf = vec![0u8; needed as usize];
        let res = GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr() as *mut c_void),
            needed,
            &mut needed,
        );
        let _ = windows::Win32::Foundation::CloseHandle(token);
        res.map_err(|e| format!("GetTokenInformation failed: {e}"))?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        sid_to_string(user.User.Sid)
    }
}

fn sid_to_string(sid: PSID) -> Result<String, String> {
    let mut pwstr = PWSTR::null();
    unsafe { ConvertSidToStringSidW(sid, &mut pwstr) }
        .map_err(|e| format!("ConvertSidToStringSidW failed: {e}"))?;
    let _guard = LocalFreeGuard(pwstr.0 as *mut c_void);
    Ok(unsafe { pwstr.to_string().unwrap_or_default() })
}

fn is_trusted_sid(sid_str: &str) -> bool {
    sid_str.eq_ignore_ascii_case(BUILTIN_ADMINS_SID)
        || sid_str.eq_ignore_ascii_case(LOCAL_SYSTEM_SID)
        || sid_str.eq_ignore_ascii_case(TRUSTED_INSTALLER_SID)
}

pub fn install_root() -> PathBuf {
    let pf = std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    pf.join("GrandHonor")
}

pub fn is_installation_path() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let expected = install_root().join("GrandHonor.exe");
    exe.to_string_lossy()
        .eq_ignore_ascii_case(&expected.to_string_lossy())
}

fn is_path_protected(path: &Path) -> bool {
    let wide = HSTRING::from(path.as_os_str());
    let attrs = unsafe { GetFileAttributesW(&wide) };
    if attrs == INVALID_FILE_ATTRIBUTES || (attrs & FILE_ATTRIBUTE_REPARSE_POINT.0) != 0 {
        return false;
    }

    let mut owner_sid = PSID::default();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let err = unsafe {
        GetNamedSecurityInfoW(
            &wide,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut owner_sid),
            None,
            Some(&mut dacl),
            None,
            &mut sd,
        )
    };
    if err.0 != 0 || sd.is_invalid() {
        return false;
    }
    let _guard = LocalFreeGuard(sd.0);

    let Ok(owner_str) = sid_to_string(owner_sid) else {
        return false;
    };
    if !is_trusted_sid(&owner_str) || dacl.is_null() {
        return false;
    }

    let mut size_info = ACL_SIZE_INFORMATION::default();
    if unsafe {
        GetAclInformation(
            dacl,
            &mut size_info as *mut _ as *mut c_void,
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    }
    .is_err()
    {
        return false;
    }

    for i in 0..size_info.AceCount {
        let mut ace_ptr: *mut c_void = std::ptr::null_mut();
        if unsafe { GetAce(dacl, i, &mut ace_ptr) }.is_err() || ace_ptr.is_null() {
            return false;
        }
        let header = unsafe { &*(ace_ptr as *const ACE_HEADER) };
        if header.AceType != ACCESS_ALLOWED_ACE_TYPE {
            continue;
        }
        if (header.AceFlags as u32 & INHERIT_ONLY_ACE.0) != 0 {
            continue;
        }
        let ace = unsafe { &*(ace_ptr as *const ACCESS_ALLOWED_ACE) };
        if (ace.Mask & WRITE_DELETE_MASK) == 0 {
            continue;
        }
        let sid_ptr = PSID(&ace.SidStart as *const u32 as *mut c_void);
        let Ok(sid_str) = sid_to_string(sid_ptr) else {
            return false;
        };
        if !is_trusted_sid(&sid_str) {
            return false;
        }
    }
    true
}

pub fn has_protected_installation() -> bool {
    if !is_installation_path() {
        return false;
    }
    let root = install_root();
    let Some(program_files) = root.parent() else {
        return false;
    };
    let host = root.join("GrandHonor.exe");
    let panel = root.join("GrandHonor.Panel.exe");
    is_path_protected(&host)
        && is_path_protected(&panel)
        && is_path_protected(&root)
        && is_path_protected(program_files)
}

fn startup_task_name() -> Result<String, String> {
    Ok(format!("GrandHonor Startup {}", current_user_sid()?))
}

fn connect_task_service() -> Result<ITaskService, String> {
    let service: ITaskService = unsafe {
        CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)
    }
    .map_err(|e| format!("Windows Task Scheduler is unavailable: {e}"))?;
    let empty = OwnedVariant::default();
    unsafe { service.Connect(&empty.0, &empty.0, &empty.0, &empty.0) }
        .map_err(|e| format!("Task Scheduler Connect failed: {e}"))?;
    Ok(service)
}

pub fn startup_task_enabled() -> bool {
    let _com = ComGuard::mta();
    let Ok(name) = startup_task_name() else {
        return false;
    };
    let Ok(service) = connect_task_service() else {
        return false;
    };
    let Ok(folder) = (unsafe { service.GetFolder(&BSTR::from(r"\")) }) else {
        return false;
    };
    let Ok(task) = (unsafe { folder.GetTask(&BSTR::from(name.as_str())) }) else {
        return false;
    };
    unsafe { task.Enabled() }.map(|b| b.as_bool()).unwrap_or(false)
}

pub fn ensure_registered() -> Result<(), String> {
    if !has_protected_installation() {
        return Err(
            "Windows startup requires an administrator-owned installation in Program Files\\GrandHonor."
                .to_string(),
        );
    }
    let _com = ComGuard::mta();
    let sid = current_user_sid()?;
    let task_name = format!("GrandHonor Startup {sid}");
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;

    let service = connect_task_service()?;
    let folder = unsafe { service.GetFolder(&BSTR::from(r"\")) }.map_err(|e| e.to_string())?;
    let def = unsafe { service.NewTask(0) }.map_err(|e| e.to_string())?;

    unsafe {
        let reg = def.RegistrationInfo().map_err(|e| e.to_string())?;
        reg.SetDescription(&BSTR::from("Start GrandHonor at Windows sign-in"))
            .map_err(|e| e.to_string())?;

        let principal = def.Principal().map_err(|e| e.to_string())?;
        principal
            .SetUserId(&BSTR::from(sid.as_str()))
            .map_err(|e| e.to_string())?;
        principal
            .SetLogonType(TASK_LOGON_INTERACTIVE_TOKEN)
            .map_err(|e| e.to_string())?;
        principal
            .SetRunLevel(TASK_RUNLEVEL_HIGHEST)
            .map_err(|e| e.to_string())?;

        let settings_obj = def.Settings().map_err(|e| e.to_string())?;
        settings_obj
            .SetDisallowStartIfOnBatteries(false.into())
            .map_err(|e| e.to_string())?;
        settings_obj
            .SetStopIfGoingOnBatteries(false.into())
            .map_err(|e| e.to_string())?;
        settings_obj
            .SetExecutionTimeLimit(&BSTR::from("PT0S"))
            .map_err(|e| e.to_string())?;

        let triggers = def.Triggers().map_err(|e| e.to_string())?;
        let trigger = triggers.Create(TASK_TRIGGER_LOGON).map_err(|e| e.to_string())?;
        let logon: ILogonTrigger = trigger.cast().map_err(|e| e.to_string())?;
        logon
            .SetUserId(&BSTR::from(sid.as_str()))
            .map_err(|e| e.to_string())?;

        let actions = def.Actions().map_err(|e| e.to_string())?;
        let action = actions.Create(TASK_ACTION_EXEC).map_err(|e| e.to_string())?;
        let exec: IExecAction = action.cast().map_err(|e| e.to_string())?;
        exec.SetPath(&BSTR::from(exe.to_string_lossy().as_ref()))
            .map_err(|e| e.to_string())?;
        exec.SetArguments(&BSTR::from("--background"))
            .map_err(|e| e.to_string())?;

        let empty = OwnedVariant::default();
        folder
            .RegisterTaskDefinition(
                &BSTR::from(task_name.as_str()),
                &def,
                TASK_CREATE_OR_UPDATE.0,
                &empty.0,
                &empty.0,
                TASK_LOGON_INTERACTIVE_TOKEN,
                &empty.0,
            )
            .map_err(|e| e.to_string())?;
    }
    let _ = settings::read_hklm_string;
    Ok(())
}

pub fn remove_registered() -> Result<(), String> {
    remove_tasks(true)
}

pub fn remove_legacy_hardware_task() -> Result<(), String> {
    remove_tasks(false)
}

fn remove_tasks(include_startup: bool) -> Result<(), String> {
    let _com = ComGuard::mta();
    let sid = current_user_sid()?;
    let service = connect_task_service()?;
    let folder = unsafe { service.GetFolder(&BSTR::from(r"\")) }.map_err(|e| e.to_string())?;
    if include_startup {
        let _ = unsafe { folder.DeleteTask(&BSTR::from(format!("GrandHonor Startup {sid}")), 0) };
    }
    let _ = unsafe { folder.DeleteTask(&BSTR::from(format!("GrandHonor Hardware {sid}")), 0) };
    Ok(())
}

pub fn is_startup_enabled() -> bool {
    is_installation_path() && startup_task_enabled()
}

pub fn set_startup_enabled(enabled: bool) -> Result<(), String> {
    if enabled {
        ensure_registered()?;
    } else {
        remove_registered()?;
    }
    cleanup_legacy_run_entries();
    Ok(())
}

pub fn migrate_installed_startup() {
    if !is_installation_path() {
        return;
    }
    let _ = remove_legacy_hardware_task();
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let expected = format!("\"{}\" --background", exe.to_string_lossy());
    if let Some(prev) = read_hkcu_run_value()
        && prev.eq_ignore_ascii_case(&expected)
        && let Err(err) = set_startup_enabled(true)
    {
        crate::log::error(&format!(
            "Could not migrate GrandHonor Windows startup: {err}"
        ));
    }
}

fn read_hkcu_run_value() -> Option<String> {
    let subkey = HSTRING::from(LEGACY_RUN_KEY);
    let mut key = windows::Win32::System::Registry::HKEY::default();
    if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &subkey, Some(0), KEY_READ, &mut key) }.0 != 0 {
        return None;
    }
    let name = HSTRING::from(VALUE_NAME);
    let mut ty = REG_VALUE_TYPE::default();
    let mut bytes = 0u32;
    let st = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut ty),
            None,
            Some(&mut bytes),
        )
    };
    if st.0 != 0 || ty != REG_SZ || bytes < 2 {
        unsafe {
            let _ = RegCloseKey(key);
        }
        return None;
    }
    let mut buf = vec![0u16; (bytes as usize).div_ceil(2)];
    let st = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut ty),
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut bytes),
        )
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    if st.0 != 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn cleanup_legacy_run_entries() {
    let subkey = HSTRING::from(LEGACY_RUN_KEY);
    let mut key = windows::Win32::System::Registry::HKEY::default();
    if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &subkey, Some(0), KEY_WRITE, &mut key) }.0 == 0 {
        let name = HSTRING::from(VALUE_NAME);
        unsafe {
            let _ = RegDeleteValueW(key, &name);
            let _ = RegCloseKey(key);
        }
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let lnk = PathBuf::from(appdata)
            .join(r"Microsoft\Windows\Start Menu\Programs\Startup")
            .join("GrandHonor.lnk");
        let _ = std::fs::remove_file(lnk);
    }
    let _ = unsafe { GetLastError() };
}
