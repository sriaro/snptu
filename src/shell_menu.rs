
use anyhow::{Result, anyhow};
use std::path::Path;
use windows::{
    Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW,
        RegEnumKeyExW, RegOpenKeyExW, RegSetValueExW,
    },
    core::{PCWSTR, PWSTR},
};

const EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "jfif", "webp", "avif", "jxl", "gif", "tif", "tiff", "bmp", "ico", "heic", "heif", "hif", "jp2", "j2k", "j2c",
    "jpc", "psd", "svg", "svgz",
];
const VERB: &str = "Snptu";
const OLD_VERB: &str = "SnptuConvert";
const OLD_MENU_KEY: &str = "Software\\Classes\\Snptu.ConvertMenu";
const BACKGROUND_KEY: &str = "Software\\Classes\\Directory\\Background\\shell\\Snptu";

pub fn extensions() -> impl Iterator<Item = &'static str> {
    EXTS.iter().copied().chain(crate::readers::RAW_EXTS.iter().copied())
}

fn verb_key(ext: &str) -> String {
    format!("Software\\Classes\\SystemFileAssociations\\.{ext}\\shell\\{VERB}")
}

fn old_verb_key(ext: &str) -> String {
    format!("Software\\Classes\\SystemFileAssociations\\.{ext}\\shell\\{OLD_VERB}")
}

fn command_line(exe: &Path) -> String {
    format!("\"{}\" --convert-ui \"%1\"", exe.display())
}

fn bare_command(exe: &Path, flag: &str) -> String {
    format!("\"{}\" {flag}", exe.display())
}

fn put(path: &str, name: Option<&str>, value: &str) -> Result<()> {
    let (p, v) = (crate::wide(path), crate::wide(value));
    let n = name.map(crate::wide);
    let mut key = HKEY::default();
    unsafe {
        RegCreateKeyExW(HKEY_CURRENT_USER, PCWSTR(p.as_ptr()), None, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_WRITE, None, &mut key, None)
            .ok()
            .map_err(|e| anyhow!("create {path}: {e}"))?;
        let bytes: Vec<u8> = v.iter().flat_map(|u| u.to_le_bytes()).collect();
        let res = RegSetValueExW(key, n.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr())), None, REG_SZ, Some(&bytes)).ok();
        let _ = RegCloseKey(key);
        res.map_err(|e| anyhow!("write {path}: {e}"))
    }
}

pub fn register(exe: &Path) -> Result<()> {
    let icon = format!("\"{}\",0", exe.display());
    let submenu = |key: &str, items: &[(&str, &str, String)]| -> Result<()> {
        put(key, Some("MUIVerb"), "Snptu")?;
        put(key, Some("Icon"), &icon)?;
        put(key, Some("SubCommands"), "")?;
        for (name, label, command) in items {
            put(&format!("{key}\\shell\\{name}"), Some("MUIVerb"), label)?;
            put(&format!("{key}\\shell\\{name}\\command"), None, command)?;
        }
        Ok(())
    };
    let files = [
        ("1convert", "Convert\u{2026}", command_line(exe)),
        ("2record", "Record screen\u{2026}", bare_command(exe, "--record")),
    ];
    for ext in extensions() {
        submenu(&verb_key(ext), &files)?;
    }
    submenu(
        BACKGROUND_KEY,
        &[
            ("1capture", "Capture screen", bare_command(exe, "--capture")),
            ("2record", "Record screen\u{2026}", bare_command(exe, "--record")),
        ],
    )
}

fn delete_tree(path: &str) {
    let k = crate::wide(path);
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()));
    }
}

fn purge_old_verbs() {
    let base = crate::wide("Software\\Classes\\SystemFileAssociations");
    let mut names = Vec::new();
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(base.as_ptr()), None, KEY_READ, &mut key).ok().is_err() {
            return;
        }
        let mut i = 0;
        loop {
            let mut buf = [0u16; 256];
            let mut len = buf.len() as u32;
            if RegEnumKeyExW(key, i, Some(PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None).0 != 0 {
                break;
            }
            names.push(String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())]));
            i += 1;
        }
        let _ = RegCloseKey(key);
    }
    for n in names {
        delete_tree(&format!("Software\\Classes\\SystemFileAssociations\\{n}\\shell\\{OLD_VERB}"));
    }
}

static MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(crate::config::SHELL_OFF);

pub fn wants_modern() -> bool {
    MODE.load(std::sync::atomic::Ordering::SeqCst) == crate::config::SHELL_MODERN
}

fn unregister() {
    for ext in extensions() {
        delete_tree(&verb_key(ext));
        delete_tree(&old_verb_key(ext));
    }
    delete_tree(BACKGROUND_KEY);
    delete_tree(OLD_MENU_KEY);
}

fn registered_command(key: &str) -> Option<String> {
    crate::config::reg_string(HKEY_CURRENT_USER, key, None)
}

static CLASSIC: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_classic() -> std::sync::MutexGuard<'static, ()> {
    CLASSIC.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn package_took_over() {
    let _classic = lock_classic();
    if wants_modern() {
        unregister();
    }
}

pub fn sync(mode: u8) -> Result<()> {
    use crate::config::{SHELL_CLASSIC, SHELL_MODERN};
    let classic = lock_classic();
    MODE.store(mode, std::sync::atomic::Ordering::SeqCst);
    purge_old_verbs();
    let convert = registered_command(&format!("{}\\shell\\1convert\\command", verb_key("png")));
    let record = registered_command(&format!("{BACKGROUND_KEY}\\shell\\2record\\command"));
    let old = registered_command(&format!("{}\\command", old_verb_key("png")));
    let classic_there = convert.is_some() || record.is_some() || old.is_some();
    let add_classic = |always: bool| -> Result<()> {
        let exe = std::env::current_exe()?;
        if always || convert.as_deref() != Some(command_line(&exe).as_str()) || record.as_deref() != Some(bare_command(&exe, "--record").as_str()) {
            delete_tree(OLD_MENU_KEY);
            for ext in extensions() {
                delete_tree(&old_verb_key(ext));
            }
            register(&exe)?;
        }
        Ok(())
    };
    let package = if mode == SHELL_MODERN && crate::shell_pkg::active() {
        if classic_there {
            unregister();
        }
        None
    } else if mode == SHELL_MODERN {
        add_classic(false)?;
        Some(true)
    } else if mode == SHELL_CLASSIC {
        add_classic(true)?;
        Some(false)
    } else {
        if classic_there {
            unregister();
        }
        Some(false)
    };
    drop(classic);
    if let Some(enabled) = package {
        crate::shell_pkg::sync(enabled);
    }
    Ok(())
}
