
use std::{
    ffi::OsString,
    os::windows::{ffi::OsStringExt, process::CommandExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{CLASS_E_NOAGGREGATION, E_NOTIMPL, S_FALSE, S_OK},
        System::{
            Com::{CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED, CoInitializeEx, CoRegisterClassObject, IBindCtx, IClassFactory, IClassFactory_Impl, REGCLS_MULTIPLEUSE},
            Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW},
        },
        UI::Shell::{
            ECF_HASSUBCOMMANDS, ECS_ENABLED, ECS_HIDDEN, IEnumExplorerCommand, IEnumExplorerCommand_Impl, IExplorerCommand, IExplorerCommand_Impl,
            IShellItem, IShellItemArray, SHStrDupW, SIGDN_FILESYSPATH,
        },
    },
    core::{BOOL, GUID, Interface, PCWSTR, PWSTR, Ref, implement},
};

const CLSID: GUID = GUID::from_u128(0x6f1f805f_ad7e_486e_acae_68ee86991e81);
const CLSID_TEXT: &str = "6F1F805F-AD7E-486E-ACAE-68EE86991E81";
const PACKAGE: &str = "Snptu.ShellExtension";
const PUBLISHER_ID: &str = "h4tqh4pm9m0tr";
const MANIFEST_VERSION: u32 = 2;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;


#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Root,
    Convert,
    Record,
    Capture,
}

static LIVE: AtomicU32 = AtomicU32::new(0);
static LOCKS: AtomicU32 = AtomicU32::new(0);
static LAST_USE: AtomicU64 = AtomicU64::new(0);

fn touch() {
    LAST_USE.store(START.get().map_or(0, |s| s.elapsed().as_millis() as u64), Ordering::Relaxed);
}
static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

#[implement(IExplorerCommand)]
struct Command {
    kind: Kind,
}

impl Command {
    fn create(kind: Kind) -> IExplorerCommand {
        LIVE.fetch_add(1, Ordering::Relaxed);
        Command { kind }.into()
    }
}

impl Drop for Command {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::Relaxed);
        touch();
    }
}

fn fs_path(item: &IShellItem) -> Option<PathBuf> {
    unsafe {
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = PathBuf::from(OsString::from_wide(name.as_wide()));
        windows::Win32::System::Com::CoTaskMemFree(Some(name.as_ptr() as *const _));
        Some(path)
    }
}

fn is_text(path: &Path) -> bool {
    path.to_str().is_some()
}

fn paths_of(items: Option<&IShellItemArray>) -> Vec<PathBuf> {
    let Some(items) = items else { return Vec::new() };
    let mut out = Vec::new();
    unsafe {
        for i in 0..items.GetCount().unwrap_or(0) {
            out.extend(items.GetItemAt(i).ok().and_then(|item| fs_path(&item)).filter(|p| is_text(p)));
        }
    }
    out
}

fn on_folder(items: Option<&IShellItemArray>) -> bool {
    let first = items.and_then(|a| unsafe { a.GetCount().ok().filter(|&n| n > 0).and_then(|_| a.GetItemAt(0).ok()) });
    first.is_none_or(|item| fs_path(&item).is_none_or(|p| p.is_dir()))
}

const MAX_COMMAND_LINE: usize = 30_000;

fn fits_command_line(exe: &Path, files: &[PathBuf]) -> bool {
    let cost = |p: &Path| p.as_os_str().len() + 3;
    cost(exe) + "--convert-ui".len() + 1 + files.iter().map(|f| cost(f)).sum::<usize>() <= MAX_COMMAND_LINE
}

fn queue_files(dir: &Path, files: &[PathBuf]) -> std::io::Result<()> {
    crate::app_convert::queue_request(dir, &files.iter().map(|f| f.to_string_lossy()).collect::<Vec<_>>().join("\n"))
}

fn dup(s: &str) -> windows::core::Result<PWSTR> {
    let w = crate::wide(s);
    unsafe { SHStrDupW(PCWSTR(w.as_ptr())) }
}

#[allow(non_snake_case)]
impl IExplorerCommand_Impl for Command_Impl {
    fn GetTitle(&self, _items: Ref<IShellItemArray>) -> windows::core::Result<PWSTR> {
        touch();
        dup(match self.kind {
            Kind::Root => "Snptu",
            Kind::Convert => "Convert\u{2026}",
            Kind::Record => "Record screen\u{2026}",
            Kind::Capture => "Capture screen",
        })
    }

    fn GetIcon(&self, _items: Ref<IShellItemArray>) -> windows::core::Result<PWSTR> {
        touch();
        match (self.kind, std::env::current_exe()) {
            (Kind::Root, Ok(exe)) => dup(&format!("{},0", exe.display())),
            _ => Err(E_NOTIMPL.into()),
        }
    }

    fn GetToolTip(&self, _items: Ref<IShellItemArray>) -> windows::core::Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }

    fn GetCanonicalName(&self) -> windows::core::Result<GUID> {
        Err(E_NOTIMPL.into())
    }

    fn GetState(&self, items: Ref<IShellItemArray>, _slow: BOOL) -> windows::core::Result<u32> {
        touch();
        let hidden = match self.kind {
            Kind::Convert => on_folder(items.as_ref()),
            Kind::Capture => !on_folder(items.as_ref()),
            Kind::Root | Kind::Record => false,
        };
        Ok(if hidden { ECS_HIDDEN.0 } else { ECS_ENABLED.0 } as u32)
    }

    fn Invoke(&self, items: Ref<IShellItemArray>, _ctx: Ref<IBindCtx>) -> windows::core::Result<()> {
        touch();
        let Ok(exe) = std::env::current_exe() else { return Err(E_NOTIMPL.into()) };
        let mut cmd = std::process::Command::new(&exe);
        match self.kind {
            Kind::Convert => {
                let files = paths_of(items.as_ref());
                let Some(first) = files.first() else { return Ok(()) };
                cmd.arg("--convert-ui");
                if fits_command_line(&exe, &files) {
                    cmd.args(&files);
                } else {
                    queue_files(&crate::app_convert::requests_dir(), &files)?;
                    cmd.arg(first);
                }
            }
            Kind::Record => {
                cmd.arg("--record");
            }
            Kind::Capture => {
                cmd.arg("--capture");
            }
            Kind::Root => return Ok(()),
        }
        cmd.creation_flags(CREATE_NO_WINDOW).spawn()?;
        Ok(())
    }

    fn GetFlags(&self) -> windows::core::Result<u32> {
        Ok(if self.kind == Kind::Root { ECF_HASSUBCOMMANDS.0 as u32 } else { 0 })
    }

    fn EnumSubCommands(&self) -> windows::core::Result<IEnumExplorerCommand> {
        touch();
        if self.kind != Kind::Root {
            return Err(E_NOTIMPL.into());
        }
        Ok(Sub { items: vec![Command::create(Kind::Convert), Command::create(Kind::Capture), Command::create(Kind::Record)], at: AtomicUsize::new(0) }.into())
    }
}

#[implement(IEnumExplorerCommand)]
struct Sub {
    items: Vec<IExplorerCommand>,
    at: AtomicUsize,
}

#[allow(non_snake_case)]
impl IEnumExplorerCommand_Impl for Sub_Impl {
    fn Next(&self, celt: u32, out: *mut Option<IExplorerCommand>, fetched: *mut u32) -> windows::core::HRESULT {
        touch();
        let mut n = 0u32;
        while n < celt {
            let Ok(at) = self.at.try_update(Ordering::SeqCst, Ordering::SeqCst, |at| (at < self.items.len()).then_some(at + 1)) else { break };
            unsafe { out.add(n as usize).write(Some(self.items[at].clone())) };
            n += 1;
        }
        if !fetched.is_null() {
            unsafe { *fetched = n };
        }
        if n == celt { S_OK } else { S_FALSE }
    }

    fn Skip(&self, celt: u32) -> windows::core::Result<()> {
        let _ = self.at.try_update(Ordering::SeqCst, Ordering::SeqCst, |at| Some(at.saturating_add(celt as usize).min(self.items.len())));
        Ok(())
    }

    fn Reset(&self) -> windows::core::Result<()> {
        self.at.store(0, Ordering::SeqCst);
        Ok(())
    }

    fn Clone(&self) -> windows::core::Result<IEnumExplorerCommand> {
        Ok(Sub { items: self.items.clone(), at: AtomicUsize::new(self.at.load(Ordering::SeqCst)) }.into())
    }
}

#[implement(IClassFactory)]
struct Factory;

#[allow(non_snake_case)]
impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(&self, outer: Ref<windows::core::IUnknown>, iid: *const GUID, out: *mut *mut core::ffi::c_void) -> windows::core::Result<()> {
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        unsafe { Command::create(Kind::Root).query(iid, out).ok() }
    }

    fn LockServer(&self, lock: BOOL) -> windows::core::Result<()> {
        if lock.as_bool() {
            LOCKS.fetch_add(1, Ordering::SeqCst);
        } else {
            let _ = LOCKS.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1));
        }
        touch();
        Ok(())
    }
}

pub fn run_server() -> ! {
    let _ = START.set(Instant::now());
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let factory: IClassFactory = Factory.into();
        if CoRegisterClassObject(&CLSID, &factory, CLSCTX_LOCAL_SERVER, REGCLS_MULTIPLEUSE).is_err() {
            std::process::exit(1);
        }
    }
    touch();
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let idle = START.get().map_or(0, |s| s.elapsed().as_millis() as u64).saturating_sub(LAST_USE.load(Ordering::Relaxed));
        if unused(LIVE.load(Ordering::SeqCst), LOCKS.load(Ordering::SeqCst), idle) {
            std::process::exit(0);
        }
    }
}

fn unused(live: u32, locks: u32, idle_ms: u64) -> bool {
    live == 0 && locks == 0 && idle_ms > 30_000
}

fn is_server_flag(arg: &str) -> bool {
    arg.eq_ignore_ascii_case("-ServerEmbedding") || arg.eq_ignore_ascii_case("/ServerEmbedding")
}

pub fn is_server_launch(args: &[String]) -> bool {
    args.get(1).is_some_and(|a| is_server_flag(a))
}

fn is_server_command_line(line: &str) -> bool {
    use windows::Win32::{
        Foundation::{HLOCAL, LocalFree},
        UI::Shell::CommandLineToArgvW,
    };
    let line = crate::wide(line);
    let mut count = 0i32;
    let argv = unsafe { CommandLineToArgvW(PCWSTR(line.as_ptr()), &mut count) };
    if argv.is_null() {
        return false;
    }
    let flag = if count >= 2 { unsafe { (*argv.add(1)).to_string().ok() } } else { None };
    unsafe {
        let _ = LocalFree(Some(HLOCAL(argv.cast())));
    }
    flag.is_some_and(|f| is_server_flag(&f))
}


fn machine_dword(key: &str, name: &str) -> Option<u32> {
    let (key, name) = (crate::wide(key), crate::wide(name));
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    let r = unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, PCWSTR(key.as_ptr()), PCWSTR(name.as_ptr()), RRF_RT_REG_DWORD, None, Some(&mut value as *mut u32 as *mut core::ffi::c_void), Some(&mut size))
    };
    r.is_ok().then_some(value)
}

pub fn has_identity() -> bool {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentPackageFullName(length: *mut u32, name: *mut u16) -> i32;
    }
    const APPMODEL_ERROR_NO_PACKAGE: i32 = 15700;
    let mut len = 0u32;
    unsafe { GetCurrentPackageFullName(&mut len, std::ptr::null_mut()) != APPMODEL_ERROR_NO_PACKAGE }
}

fn installed_packages() -> Option<Vec<String>> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetPackagesByPackageFamily(family: *const u16, count: *mut u32, names: *mut *mut u16, buffer_len: *mut u32, buffer: *mut u16) -> i32;
    }
    const ERROR_INSUFFICIENT_BUFFER: i32 = 122;
    let family = crate::wide(&format!("{PACKAGE}_{PUBLISHER_ID}"));
    let (mut count, mut len) = (0u32, 0u32);
    unsafe {
        match GetPackagesByPackageFamily(family.as_ptr(), &mut count, std::ptr::null_mut(), &mut len, std::ptr::null_mut()) {
            0 => return Some(Vec::new()),
            ERROR_INSUFFICIENT_BUFFER => {}
            _ => return None,
        }
        let mut names = vec![std::ptr::null_mut(); count as usize];
        let mut buffer = vec![0u16; len as usize];
        if GetPackagesByPackageFamily(family.as_ptr(), &mut count, names.as_mut_ptr(), &mut len, buffer.as_mut_ptr()) != 0 {
            return None;
        }
        names.iter().take(count as usize).map(|&n| PCWSTR(n).to_string().ok()).collect()
    }
}

fn is_current(full_name: &str) -> bool {
    full_name.eq_ignore_ascii_case(&format!("{PACKAGE}_1.0.{MANIFEST_VERSION}.0_x64__{PUBLISHER_ID}"))
}

fn is_windows_11() -> bool {
    crate::config::reg_string(HKEY_LOCAL_MACHINE, "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion", Some("CurrentBuildNumber"))
        .and_then(|b| b.parse::<u32>().ok())
        .is_some_and(|b| b >= 22000)
}

pub fn developer_mode() -> bool {
    machine_dword("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\AppModelUnlock", "AllowDevelopmentWithoutDevLicense") == Some(1)
}

pub fn needs_developer_mode() -> bool {
    is_windows_11() && !developer_mode()
}

fn package_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Snptu").join("shellext"))
}

fn marker(dir: &Path) -> PathBuf {
    dir.join("registered.txt")
}

fn stamp(exe: &Path) -> String {
    format!("{MANIFEST_VERSION}\n{}", exe.display())
}

fn item_types() -> Vec<String> {
    crate::shell_menu::extensions().map(|e| format!(".{e}")).collect()
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn manifest(exe_name: &str, unvirtualized: bool) -> String {
    let (ns, ignorable, props, caps) = if unvirtualized {
        (
            "\n  xmlns:desktop6=\"http://schemas.microsoft.com/appx/manifest/desktop/windows10/6\"",
            " desktop6",
            "\n    <desktop6:RegistryWriteVirtualization>disabled</desktop6:RegistryWriteVirtualization>",
            "<rescap:Capability Name=\"unvirtualizedResources\"/>",
        )
    } else {
        ("", "", "", "")
    };
    let verb = |t: &str| format!("        <desktop5:ItemType Type=\"{t}\"><desktop5:Verb Id=\"Snptu\" Clsid=\"{CLSID_TEXT}\"/></desktop5:ItemType>\n");
    let mut types: String = item_types().iter().map(|t| verb(t)).collect();
    types.push_str(&verb("Directory\\Background"));
    let exe = escape(exe_name);
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"
  xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10"
  xmlns:uap10="http://schemas.microsoft.com/appx/manifest/uap/windows10/10"
  xmlns:rescap="http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities"
  xmlns:com="http://schemas.microsoft.com/appx/manifest/com/windows10"
  xmlns:desktop4="http://schemas.microsoft.com/appx/manifest/desktop/windows10/4"
  xmlns:desktop5="http://schemas.microsoft.com/appx/manifest/desktop/windows10/5"{ns}
  IgnorableNamespaces="uap uap10 rescap com desktop4 desktop5{ignorable}">
  <Identity Name="{PACKAGE}" Publisher="CN=Snptu" Version="1.0.{MANIFEST_VERSION}.0" ProcessorArchitecture="x64"/>
  <Properties>
    <DisplayName>Snptu</DisplayName>
    <PublisherDisplayName>Snptu</PublisherDisplayName>
    <Logo>Assets\StoreLogo.png</Logo>
    <uap10:AllowExternalContent>true</uap10:AllowExternalContent>{props}
  </Properties>
  <Resources><Resource Language="en-us"/></Resources>
  <Dependencies><TargetDeviceFamily Name="Windows.Desktop" MinVersion="10.0.22000.0" MaxVersionTested="10.0.26100.0"/></Dependencies>
  <Capabilities><rescap:Capability Name="runFullTrust"/>{caps}</Capabilities>
  <Applications>
    <Application Id="Snptu" Executable="{exe}" uap10:TrustLevel="mediumIL" uap10:RuntimeBehavior="win32App">
      <uap:VisualElements DisplayName="Snptu" Description="Snptu" BackgroundColor="transparent" AppListEntry="none"
        Square150x150Logo="Assets\Square150x150Logo.png" Square44x44Logo="Assets\Square44x44Logo.png"/>
      <Extensions>
        <com:Extension Category="windows.comServer">
          <com:ComServer>
            <com:ExeServer Executable="{exe}" Arguments="-ServerEmbedding" DisplayName="Snptu" LaunchAndActivationPermission="O:PSG:BUD:(A;;11;;;WD)">
              <com:Class Id="{CLSID_TEXT}" DisplayName="Snptu"/>
            </com:ExeServer>
          </com:ComServer>
        </com:Extension>
        <desktop4:Extension Category="windows.fileExplorerContextMenus">
          <desktop4:FileExplorerContextMenus>
{types}          </desktop4:FileExplorerContextMenus>
        </desktop4:Extension>
      </Extensions>
    </Application>
  </Applications>
</Package>
"#
    )
}

fn write_package(dir: &Path, exe_name: &str, unvirtualized: bool) -> anyhow::Result<()> {
    let assets = dir.join("Assets");
    std::fs::create_dir_all(&assets)?;
    for (name, size) in [("StoreLogo.png", 50u32), ("Square44x44Logo.png", 44), ("Square150x150Logo.png", 150)] {
        let img = image::RgbaImage::from_raw(size, size, crate::icon::render_rgba(size)).ok_or_else(|| anyhow::anyhow!("icon size"))?;
        let mut file = std::io::BufWriter::new(std::fs::File::create(assets.join(name))?);
        image::ImageEncoder::write_image(image::codecs::png::PngEncoder::new(&mut file), img.as_raw(), size, size, image::ExtendedColorType::Rgba8)?;
    }
    std::fs::write(dir.join("AppxManifest.xml"), manifest(exe_name, unvirtualized))?;
    Ok(())
}

const DEPLOY_LIMIT: Duration = Duration::from_secs(120);

fn settle(status: impl Fn() -> windows::core::Result<i32>, cancel: impl Fn(), limit: Duration) -> Result<i32, String> {
    const STARTED: i32 = 0;
    let by = Instant::now() + limit;
    loop {
        let state = status().map_err(|e| e.message())?;
        if state != STARTED {
            return Ok(state);
        }
        if Instant::now() >= by {
            cancel();
            return Err("timed out".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn outcome(state: i32, result: windows::core::Result<windows::Management::Deployment::DeploymentResult>) -> Result<(), String> {
    const COMPLETED: i32 = 1;
    if state == COMPLETED {
        return Ok(());
    }
    let detail = result.ok().map(|r| {
        let text = r.ErrorText().map(|t| t.to_string()).unwrap_or_default();
        let code = r.ExtendedErrorCode().map(|c| format!("{:#010X}", c.0 as u32)).unwrap_or_default();
        format!("{code} {text}").trim().replace(['\r', '\n'], " ")
    });
    Err(detail.filter(|d| !d.is_empty()).unwrap_or_else(|| format!("deployment ended in state {state}")))
}

fn uri(path: &Path) -> Result<windows::Foundation::Uri, String> {
    let text = path.to_str().ok_or_else(|| format!("{} is not a Unicode path", path.display()))?;
    let text = text.strip_prefix(r"\\?\UNC\").map(|rest| format!(r"\\{rest}")).unwrap_or_else(|| text.strip_prefix(r"\\?\").unwrap_or(text).to_string());
    windows::Foundation::Uri::CreateUri(&windows::core::HSTRING::from(text)).map_err(|e| e.message())
}
fn command_line(process: windows::Win32::Foundation::HANDLE) -> Option<String> {
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQueryInformationProcess(process: windows::Win32::Foundation::HANDLE, class: u32, info: *mut core::ffi::c_void, length: u32, returned: *mut u32) -> i32;
    }
    const PROCESS_COMMAND_LINE_INFORMATION: u32 = 60;
    let mut needed = 0u32;
    unsafe { NtQueryInformationProcess(process, PROCESS_COMMAND_LINE_INFORMATION, std::ptr::null_mut(), 0, &mut needed) };
    if !(16..=1 << 20).contains(&needed) {
        return None;
    }
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    let size = (buf.len() * 8) as u32;
    if unsafe { NtQueryInformationProcess(process, PROCESS_COMMAND_LINE_INFORMATION, buf.as_mut_ptr().cast(), size, &mut needed) } < 0 {
        return None;
    }
    let bytes = buf[0] as u16 as usize;
    let text = buf[1] as usize;
    let (start, end) = (buf.as_ptr() as usize, buf.as_ptr() as usize + buf.len() * 8);
    if bytes == 0 {
        return Some(String::new());
    }
    if text < start || !text.is_multiple_of(2) || text + bytes > end {
        return None;
    }
    Some(String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text as *const u16, bytes / 2) }))
}

fn end_servers(exe_name: &std::ffi::OsStr) -> usize {
    use windows::Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0},
        System::{
            Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS},
            Threading::{GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject},
        },
    };
    let Some(want) = exe_name.to_str() else { return 0 };
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else { return 0 };
    let me = unsafe { GetCurrentProcessId() };
    let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    let mut ended = 0;
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
    while more {
        let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
        let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
        if entry.th32ProcessID != me && name.eq_ignore_ascii_case(want) {
            let access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE;
            if let Ok(process) = unsafe { OpenProcess(access, false, entry.th32ProcessID) } {
                let server = command_line(process).is_some_and(|c| is_server_command_line(&c));
                if server && unsafe { TerminateProcess(process, 1) }.is_ok() {
                    ended += 1;
                    let _ = unsafe { WaitForSingleObject(process, 5000) } == WAIT_OBJECT_0;
                }
                let _ = unsafe { CloseHandle(process) };
            }
        }
        more = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
    }
    let _ = unsafe { CloseHandle(snapshot) };
    ended
}

fn exe_file_name() -> PathBuf {
    std::env::current_exe().ok().and_then(|e| e.file_name().map(PathBuf::from)).unwrap_or_else(|| PathBuf::from("Snptu.exe"))
}

fn remove_installed(exe_name: &std::ffi::OsStr) -> Result<(), String> {
    end_servers(exe_name);
    let installed = installed_packages().ok_or("the installed packages could not be listed")?;
    if installed.is_empty() {
        return Ok(());
    }
    let manager = windows::Management::Deployment::PackageManager::new().map_err(|e| e.message())?;
    for name in installed {
        let op = manager.RemovePackageAsync(&windows::core::HSTRING::from(name.as_str())).map_err(|e| e.message())?;
        let state = settle(|| op.Status().map(|s| s.0), || drop(op.Cancel()), DEPLOY_LIMIT)?;
        outcome(state, op.GetResults()).map_err(|e| format!("removing {name}: {e}"))?;
    }
    Ok(())
}

fn add_package(manifest: &Path, folder: &Path) -> Result<(), String> {
    use windows::Management::Deployment::{PackageManager, RegisterPackageOptions};
    let manager = PackageManager::new().map_err(|e| e.message())?;
    let options = RegisterPackageOptions::new().map_err(|e| e.message())?;
    options.SetDeveloperMode(true).map_err(|e| e.message())?;
    options.SetExternalLocationUri(&uri(folder)?).map_err(|e| e.message())?;
    let op = manager.RegisterPackageByUriAsync(&uri(manifest)?, &options).map_err(|e| e.message())?;
    let state = settle(|| op.Status().map(|s| s.0), || drop(op.Cancel()), DEPLOY_LIMIT)?;
    outcome(state, op.GetResults())
}

fn with_runtime<T>(job: impl FnOnce() -> T) -> T {
    static MTA: std::sync::Once = std::sync::Once::new();
    MTA.call_once(|| {
        let _ = unsafe { windows::Win32::System::Com::CoIncrementMTAUsage() };
    });
    job()
}
fn register(exe: &Path) -> anyhow::Result<()> {
    let dir = package_dir().ok_or_else(|| anyhow::anyhow!("no LOCALAPPDATA"))?;
    let name = exe.file_name().and_then(|n| n.to_str()).ok_or_else(|| anyhow::anyhow!("exe name"))?;
    let folder = exe.parent().ok_or_else(|| anyhow::anyhow!("exe folder"))?;
    let _ = std::fs::remove_file(marker(&dir));
    let manifest = dir.join("AppxManifest.xml");
    let result = with_runtime(|| -> anyhow::Result<()> {
        remove_installed(std::ffi::OsStr::new(name)).map_err(|e| anyhow::anyhow!("removing the old package: {e}"))?;
        let mut result = Ok(());
        for unvirtualized in [true, false] {
            write_package(&dir, name, unvirtualized)?;
            result = add_package(&manifest, folder);
            if result.is_ok() {
                break;
            }
        }
        result.map_err(|e| anyhow::anyhow!("registering the package: {e}"))
    });
    result?;
    std::fs::write(marker(&dir), stamp(exe))?;
    Ok(())
}

fn unregister(dir: &Path) {
    if with_runtime(|| remove_installed(exe_file_name().as_os_str())).is_ok() {
        let _ = std::fs::remove_file(marker(dir));
    }
}

fn is_registered(dir: &Path, exe: &Path) -> bool {
    std::fs::read_to_string(marker(dir)).ok().as_deref() == Some(stamp(exe).as_str())
        && installed_packages().is_none_or(|installed| installed.iter().any(|n| is_current(n)))
}

fn is_left(dir: &Path) -> bool {
    marker(dir).exists() || installed_packages().is_some_and(|installed| !installed.is_empty())
}

pub fn active() -> bool {
    let (Some(dir), Ok(exe)) = (package_dir(), std::env::current_exe()) else { return false };
    PENDING.load(std::sync::atomic::Ordering::SeqCst) == 0 && is_registered(&dir, &exe)
}

static PKG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static PENDING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn sync(enabled: bool) {
    let Some(dir) = package_dir() else { return };
    if enabled {
        let Ok(exe) = std::env::current_exe() else { return };
        if !is_windows_11() || !developer_mode() || active() {
            return;
        }
        if has_identity() {
            crate::log("windows 11 menu: the package is registered with another manifest; it is updated the next time Snptu starts normally");
            return;
        }
        PENDING.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::thread::spawn(move || {
            let guard = PKG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            if crate::shell_menu::wants_modern() {
                let result = if is_registered(&dir, &exe) { Ok(()) } else { register(&exe) };
                match result {
                    Ok(()) => crate::shell_menu::package_took_over(),
                    Err(e) => crate::log(&format!("windows 11 menu: {e:#}")),
                }
            }
            drop(guard);
            PENDING.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        });
    } else if PENDING.load(std::sync::atomic::Ordering::SeqCst) > 0 || is_left(&dir) {
        if has_identity() {
            crate::log("windows 11 menu: still registered; it is removed the next time Snptu starts normally");
            return;
        }
        PENDING.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::thread::spawn(move || {
            let guard = PKG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            if !crate::shell_menu::wants_modern() && is_left(&dir) {
                unregister(&dir);
            }
            drop(guard);
            PENDING.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        });
    }
}
