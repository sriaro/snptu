
use crate::wide;
use anyhow::Result;
use std::{
    ffi::c_void,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use windows::{
    Win32::{
        Foundation::{ERROR_CANCELLED, HWND},
        System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize},
        UI::Shell::{
            Common::COMDLG_FILTERSPEC, FDE_OVERWRITE_RESPONSE, FDE_SHAREVIOLATION_RESPONSE, FDEOR_DEFAULT, FDESVR_DEFAULT,
            FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, FileSaveDialog, IFileDialog,
            IFileDialogEvents, IFileDialogEvents_Impl, IFileOpenDialog, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName,
            SIGDN_FILESYSPATH,
        },
    },
    core::{GUID, HRESULT, PCWSTR, Ref, implement, w},
};

const SAVE_DIALOG_GUID: GUID = GUID::from_u128(0x5a3f1c52_7b0e_4d1a_9c3e_2f6b8e4a1d07);

struct Com(bool);

fn init_com() -> Com {
    Com(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() }
        }
    }
}

fn path_of(item: &IShellItem) -> Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    unsafe {
        let p = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = PathBuf::from(std::ffi::OsString::from_wide(p.as_wide()));
        CoTaskMemFree(Some(p.0 as *const c_void));
        Ok(path)
    }
}

const IMAGE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "jfif", "webp", "avif", "jxl", "gif", "tif", "tiff", "bmp", "ico", "heic", "heif", "hif", "jp2", "j2k", "j2c", "jpc", "psd", "svg", "svgz",
];

fn image_spec() -> Vec<u16> {
    let patterns: Vec<String> = IMAGE_EXTS.iter().chain(crate::readers::RAW_EXTS).map(|e| format!("*.{e}")).collect();
    wide(&patterns.join(";"))
}

fn image_types(spec: &[u16]) -> [COMDLG_FILTERSPEC; 2] {
    [COMDLG_FILTERSPEC { pszName: w!("Images"), pszSpec: PCWSTR(spec.as_ptr()) }, COMDLG_FILTERSPEC { pszName: w!("All files"), pszSpec: w!("*.*") }]
}

fn cancelled(e: &windows::core::Error) -> bool {
    e.code() == HRESULT::from_win32(ERROR_CANCELLED.0)
}

pub fn save_formats(owner: Option<HWND>, default_name: &str, types: &[(&str, &str)]) -> Result<Option<(PathBuf, usize)>> {
    save_formats_token(new_token(), owner, default_name, types)
}

struct Dialogs {
    open: Vec<(u64, u32)>,
    asked: Vec<u64>,
}

static DIALOGS: Mutex<Dialogs> = Mutex::new(Dialogs { open: Vec::new(), asked: Vec::new() });
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

fn dialogs() -> std::sync::MutexGuard<'static, Dialogs> {
    DIALOGS.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn new_token() -> u64 {
    NEXT_TOKEN.fetch_add(1, Ordering::Relaxed)
}

pub fn save_formats_token(token: u64, owner: Option<HWND>, default_name: &str, types: &[(&str, &str)]) -> Result<Option<(PathBuf, usize)>> {
    let tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
    {
        let mut d = dialogs();
        if let Some(i) = d.asked.iter().position(|t| *t == token) {
            d.asked.remove(i);
            return Ok(None);
        }
        d.open.push((token, tid));
    }
    let res = save_formats_inner(owner, default_name, types, token);
    let mut d = dialogs();
    d.open.retain(|o| o.0 != token);
    d.asked.retain(|t| *t != token);
    res
}

fn close_asked(token: u64) -> bool {
    dialogs().asked.contains(&token)
}

pub fn close_dialog(token: u64) {
    use windows::Win32::{
        Foundation::{LPARAM, WPARAM},
        UI::WindowsAndMessaging::{EnumThreadWindows, PostMessageW, WM_CLOSE},
    };
    unsafe extern "system" fn close(hwnd: HWND, _: LPARAM) -> windows::core::BOOL {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        true.into()
    }
    let tid = {
        let mut d = dialogs();
        if !d.asked.contains(&token) {
            if d.asked.len() >= 64 {
                d.asked.remove(0);
            }
            d.asked.push(token);
        }
        d.open.iter().find(|o| o.0 == token).map(|o| o.1)
    };
    if let Some(tid) = tid {
        unsafe {
            let _ = EnumThreadWindows(tid, Some(close), LPARAM(0));
        }
    }
}

#[implement(IFileDialogEvents)]
struct CloseIfAsked(u64);

impl IFileDialogEvents_Impl for CloseIfAsked_Impl {
    fn OnFileOk(&self, _: Ref<IFileDialog>) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnFolderChanging(&self, _: Ref<IFileDialog>, _: Ref<IShellItem>) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnFolderChange(&self, pfd: Ref<IFileDialog>) -> windows::core::Result<()> {
        if let (true, Some(dlg)) = (close_asked(self.0), pfd.as_ref()) {
            unsafe {
                let _ = dlg.Close(HRESULT::from_win32(ERROR_CANCELLED.0));
            }
        }
        Ok(())
    }
    fn OnSelectionChange(&self, _: Ref<IFileDialog>) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnShareViolation(&self, _: Ref<IFileDialog>, _: Ref<IShellItem>) -> windows::core::Result<FDE_SHAREVIOLATION_RESPONSE> {
        Ok(FDESVR_DEFAULT)
    }
    fn OnTypeChange(&self, _: Ref<IFileDialog>) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnOverwrite(&self, _: Ref<IFileDialog>, _: Ref<IShellItem>) -> windows::core::Result<FDE_OVERWRITE_RESPONSE> {
        Ok(FDEOR_DEFAULT)
    }
}

fn save_formats_inner(owner: Option<HWND>, default_name: &str, types: &[(&str, &str)], token: u64) -> Result<Option<(PathBuf, usize)>> {
    if types.is_empty() {
        return Ok(None);
    }
    let _com = init_com();
    let names: Vec<Vec<u16>> = types.iter().map(|(n, _)| wide(n)).collect();
    let specs: Vec<Vec<u16>> = types.iter().map(|(_, e)| wide(&format!("*.{e}"))).collect();
    let filters: Vec<COMDLG_FILTERSPEC> = names
        .iter()
        .zip(&specs)
        .map(|(n, s)| COMDLG_FILTERSPEC { pszName: PCWSTR(n.as_ptr()), pszSpec: PCWSTR(s.as_ptr()) })
        .collect();
    unsafe {
        let dlg: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?;
        dlg.SetFileTypes(&filters)?;
        dlg.SetFileTypeIndex(1)?;
        let ext = wide(types[0].1);
        dlg.SetDefaultExtension(PCWSTR(ext.as_ptr()))?;
        let name = wide(default_name);
        dlg.SetFileName(PCWSTR(name.as_ptr()))?;
        dlg.SetClientGuid(&SAVE_DIALOG_GUID)?;
        let opts = dlg.GetOptions()?;
        dlg.SetOptions(opts | FOS_FORCEFILESYSTEM | FOS_OVERWRITEPROMPT | FOS_PATHMUSTEXIST)?;
        if close_asked(token) {
            return Ok(None);
        }
        let events: IFileDialogEvents = CloseIfAsked(token).into();
        let cookie = dlg.Advise(&events).ok();
        let shown = dlg.Show(owner);
        if let Some(c) = cookie {
            let _ = dlg.Unadvise(c);
        }
        match shown {
            Ok(()) => {}
            Err(e) if cancelled(&e) => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        let index = (dlg.GetFileTypeIndex()? as usize).clamp(1, types.len()) - 1;
        Ok(Some((path_of(&dlg.GetResult()?)?, index)))
    }
}

pub fn open_image(owner: Option<HWND>) -> Result<Option<PathBuf>> {
    let _com = init_com();
    let spec = image_spec();
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dlg.SetFileTypes(&image_types(&spec))?;
        let opts = dlg.GetOptions()?;
        dlg.SetOptions(opts | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST)?;
        match dlg.Show(owner) {
            Ok(()) => {}
            Err(e) if cancelled(&e) => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        Ok(Some(path_of(&dlg.GetResult()?)?))
    }
}

pub fn open_images(owner: Option<HWND>) -> Result<Option<Vec<PathBuf>>> {
    use windows::Win32::UI::Shell::FOS_ALLOWMULTISELECT;
    let _com = init_com();
    let spec = image_spec();
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dlg.SetFileTypes(&image_types(&spec))?;
        let opts = dlg.GetOptions()?;
        dlg.SetOptions(opts | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST | FOS_ALLOWMULTISELECT)?;
        match dlg.Show(owner) {
            Ok(()) => {}
            Err(e) if cancelled(&e) => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        let items = dlg.GetResults()?;
        let mut out = Vec::new();
        for i in 0..items.GetCount()? {
            out.push(path_of(&items.GetItemAt(i)?)?);
        }
        Ok(Some(out))
    }
}

pub fn pick_folder(owner: Option<HWND>, start: &Path) -> Result<Option<PathBuf>> {
    let _com = init_com();
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        let opts = dlg.GetOptions()?;
        dlg.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST)?;
        if start.is_dir() {
            let s = wide(&start.to_string_lossy());
            if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(s.as_ptr()), None) {
                let _ = dlg.SetFolder(&item);
            }
        }
        match dlg.Show(owner) {
            Ok(()) => {}
            Err(e) if cancelled(&e) => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        Ok(Some(path_of(&dlg.GetResult()?)?))
    }
}

static BUSY: Mutex<Vec<u64>> = Mutex::new(Vec::new());

struct Busy(u64);

fn claim(key: u64) -> Option<Busy> {
    let mut busy = BUSY.lock().unwrap_or_else(|e| e.into_inner());
    if busy.contains(&key) {
        return None;
    }
    busy.push(key);
    Some(Busy(key))
}

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.lock().unwrap_or_else(|e| e.into_inner()).retain(|k| *k != self.0);
    }
}

pub fn in_background(key: u64, owner: Option<HWND>, show: impl FnOnce(Option<HWND>) + Send + 'static) {
    let Some(busy) = claim(key) else { return };
    let owner = owner.map(|h| h.0 as isize);
    std::thread::spawn(move || {
        let _busy = busy;
        show(owner.map(|v| HWND(v as *mut c_void)));
    });
}
