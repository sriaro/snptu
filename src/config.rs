
use crate::{
    convert::{Kind, Params},
    theme::ThemeMode,
};
use anyhow::{Result, anyhow};
use std::path::PathBuf;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::core::{PCWSTR, w};

pub const DEFAULT_HOTKEY: &str = "ALT+F1";
pub const DEFAULT_NESTED_HOTKEY: &str = "ALT+F2";
pub const DEFAULT_RECORD_HOTKEY: &str = "ALT+F3";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Snip,
    SnipCopy,
    Paste,
    TogglePins,
    ClickThrough,
    SnipNested,
    Record,
}

impl Action {
    pub const ALL: [Action; 7] = [
        Action::Snip,
        Action::SnipCopy,
        Action::Paste,
        Action::TogglePins,
        Action::ClickThrough,
        Action::SnipNested,
        Action::Record,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn key(self) -> &'static str {
        ["snip", "snip_copy", "paste", "toggle_pins", "click_through", "snip_nested", "record"][self.index()]
    }

    pub fn title(self) -> &'static str {
        ["Snip", "Snip and copy", "Paste as pin", "Hide / show pins", "Pin click-through", "Nested snip", "Record screen"][self.index()]
    }

    pub fn desc(self) -> &'static str {
        [
            "Start a capture",
            "Copy right after selecting",
            "Pin the clipboard image",
            "Hide or show all pins",
            "Clicks pass through pins",
            "Capture the capture screen itself",
            "Record a region; press again to stop",
        ][self.index()]
    }
}

pub const ACTION_COUNT: usize = Action::ALL.len();

pub const SHELL_OFF: u8 = 0;
pub const SHELL_MODERN: u8 = 1;
pub const SHELL_CLASSIC: u8 = 2;

pub struct Config {
    pub hotkeys: [String; ACTION_COUNT],
    pub auto_save: bool,
    pub save_dir: PathBuf,
    pub file_name: String,
    pub theme: ThemeMode,
    pub remember_colors: bool,
    pub colors: [String; 9],
    pub format: Kind,
    pub nested_capture: bool,
    pub shell_menu: u8,
    pub compression: u8,
    pub quality: u8,
    pub rec: crate::recfmt::Settings,
    pub convert: crate::convert_prefs::ConvertPrefs,
    pub unreadable: bool,
}

pub const OUTPUT_KINDS: [Kind; 3] = [Kind::Png, Kind::Jpeg, Kind::Webp];
pub const QUALITY_STEPS: [(&str, u8); 4] = [("Small", 60), ("Balanced", 80), ("High", 90), ("Maximum", 97)];

pub fn quality_step(q: u8) -> usize {
    (0..QUALITY_STEPS.len()).min_by_key(|i| (QUALITY_STEPS[*i].1 as i32 - q as i32).abs()).unwrap_or(1)
}

pub const CUSTOM: u8 = 1;
pub const LOSSLESS: u8 = 2;

pub fn parse_color(v: &str) -> Option<(usize, Option<[u8; 3]>)> {
    let v = v.trim();
    if let Some(n) = v.strip_prefix('p') {
        let n: usize = n.parse().ok()?;
        return (n < crate::annot::PALETTE.len()).then_some((n, None));
    }
    crate::picker::parse_hex(v.strip_prefix('#')?).map(|c| (0, Some(c)))
}

pub fn color_string(color: usize, custom: Option<[u8; 3]>) -> String {
    match custom {
        Some(c) => format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]),
        None => format!("p{color}"),
    }
}

pub const DEFAULT_FILE_NAME: &str = "$yyyy-MM-dd_HH-mm-ss$";

#[derive(Clone, Copy)]
pub struct Stamp {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    pub millis: u16,
}

pub fn now_stamp() -> Stamp {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let t = unsafe { GetLocalTime() };
    Stamp {
        year: t.wYear,
        month: t.wMonth,
        day: t.wDay,
        hour: t.wHour,
        minute: t.wMinute,
        second: t.wSecond,
        millis: t.wMilliseconds,
    }
}

pub fn expand_name(template: &str, t: &Stamp) -> String {
    let mut out = String::new();
    for (i, part) in template.split('$').enumerate() {
        if i % 2 == 0 {
            out.push_str(part);
            continue;
        }
        let mut rest = part;
        while !rest.is_empty() {
            let (text, len) = if rest.starts_with("yyyy") {
                (format!("{:04}", t.year), 4)
            } else if rest.starts_with("yy") {
                (format!("{:02}", t.year % 100), 2)
            } else if rest.starts_with("MM") {
                (format!("{:02}", t.month), 2)
            } else if rest.starts_with("dd") {
                (format!("{:02}", t.day), 2)
            } else if rest.starts_with("HH") {
                (format!("{:02}", t.hour), 2)
            } else if rest.starts_with("mm") {
                (format!("{:02}", t.minute), 2)
            } else if rest.starts_with("ss") {
                (format!("{:02}", t.second), 2)
            } else if rest.starts_with("zzz") {
                (format!("{:03}", t.millis), 3)
            } else {
                let c = rest.chars().next().unwrap();
                (c.to_string(), c.len_utf8())
            };
            out.push_str(&text);
            rest = &rest[len..];
        }
    }
    let cleaned: String = out
        .chars()
        .map(|c| if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control() { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').chars().take(120).collect::<String>();
    if cleaned.trim().is_empty() {
        return expand_name(DEFAULT_FILE_NAME, t);
    }
    let base = cleaned.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    let device = matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (base.len() == 4 && (base.starts_with("COM") || base.starts_with("LPT")) && base.as_bytes()[3].is_ascii_digit());
    if device { format!("_{cleaned}") } else { cleaned }
}

const LOCATION_FILE: &str = "Snptu.location";

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(PathBuf::from)
}

fn writable(dir: &std::path::Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let pid = std::process::id();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    for n in 0..8 {
        let probe = dir.join(format!(".snptu-write-test-{pid}-{now}-{n}"));
        let opened = std::fs::OpenOptions::new().write(true).create_new(true).share_mode(FILE_SHARE_DELETE).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).open(&probe);
        match opened {
            Ok(f) => {
                let _ = std::fs::remove_file(&probe);
                drop(f);
                return true;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return false,
        }
    }
    false
}

fn custom_dir() -> Option<PathBuf> {
    let text = std::fs::read_to_string(exe_dir()?.join(LOCATION_FILE)).ok()?;
    let dir = PathBuf::from(text.lines().next()?.trim());
    dir.is_absolute().then_some(dir)
}

fn find_dir() -> Option<PathBuf> {
    custom_dir().filter(|d| writable(d)).or_else(default_dir)
}

fn default_dir() -> Option<PathBuf> {
    if let Some(dir) = exe_dir().filter(|d| writable(d)) {
        return Some(dir);
    }
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("Snptu"))
}

static DIR: std::sync::Mutex<Option<(Option<PathBuf>, bool)>> = std::sync::Mutex::new(None);

fn known_dir() -> (Option<PathBuf>, bool) {
    let mut slot = DIR.lock().unwrap_or_else(|e| e.into_inner());
    slot.get_or_insert_with(|| {
        let found = find_dir();
        let custom = found.is_some() && found == custom_dir();
        (found, custom)
    })
    .clone()
}

fn forget_dir() {
    *DIR.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

pub fn config_dir() -> Option<PathBuf> {
    known_dir().0
}

pub fn saved_theme() -> ThemeMode {
    let places = [custom_dir(), exe_dir()].into_iter().flatten().map(|d| d.join("config.txt")).chain(legacy_config_path());
    let Some(bytes) = places.into_iter().find_map(|p| std::fs::read(p).ok()) else { return ThemeMode::System };
    text_of(&bytes).lines().filter_map(|l| l.strip_prefix("theme=").and_then(ThemeMode::parse)).next_back().unwrap_or_default()
}

pub fn config_dir_is_custom() -> bool {
    known_dir().1
}

fn config_path() -> Option<PathBuf> {
    Some(config_dir()?.join("config.txt"))
}

fn legacy_config_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("Snptu").join("config.txt"))
}

const UNREADABLE: &str = "file or folder was unavailable at start; restart Snptu";

fn read_retrying(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    let mut res = std::fs::read(path);
    for _ in 0..3 {
        if res.as_ref().is_err_and(|e| e.kind() != std::io::ErrorKind::NotFound) {
            std::thread::sleep(std::time::Duration::from_millis(150));
            res = std::fs::read(path);
        }
    }
    res
}

pub fn set_config_dir(dir: Option<&std::path::Path>, cfg: &Config) -> Result<()> {
    if cfg.unreadable {
        return Err(anyhow!(UNREADABLE));
    }
    let exe = exe_dir().ok_or_else(|| anyhow!("no exe folder"))?;
    let home = if dir.is_none() { default_dir() } else { None };
    move_settings(&exe.join(LOCATION_FILE), dir, home, cfg)?;
    forget_dir();
    Ok(())
}

fn move_settings(pointer: &std::path::Path, dir: Option<&std::path::Path>, home: Option<PathBuf>, cfg: &Config) -> Result<()> {
    let folder = match dir {
        Some(d) if d.is_absolute() && writable(d) => d.to_path_buf(),
        Some(d) => return Err(anyhow!("can't write to {}", d.display())),
        None => home.ok_or_else(|| anyhow!("no settings folder"))?,
    };
    cfg.save_to(&folder.join("config.txt"))?;
    match dir {
        Some(d) => crate::convert::write_atomic(pointer, d.display().to_string().as_bytes())?,
        None => match std::fs::remove_file(pointer) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(anyhow!("remove {}: {e}", pointer.display())),
            _ => {}
        },
    }
    Ok(())
}

pub fn default_save_dir() -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::UI::Shell::{FOLDERID_Pictures, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    let known = unsafe { SHGetKnownFolderPath(&FOLDERID_Pictures, KF_FLAG_DEFAULT, None) }.ok().map(|p| unsafe {
        let path = PathBuf::from(std::ffi::OsString::from_wide(p.as_wide()));
        windows::Win32::System::Com::CoTaskMemFree(Some(p.as_ptr() as *const _));
        path
    });
    known
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Pictures")).filter(|p| p.is_absolute()))
        .or_else(exe_dir)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(std::env::temp_dir)
        .join("Snptu")
}

impl Config {
    pub fn defaults() -> Self {
        Self::parse("")
    }

    pub fn load() -> Self {
        let stranded = custom_dir().filter(|_| !config_dir_is_custom());
        let (cfg, first_run) = Self::read_from(config_path(), stranded, legacy_config_path());
        if first_run {
            let _ = cfg.save();
        }
        cfg
    }

    fn read_from(primary: Option<PathBuf>, stranded: Option<PathBuf>, legacy: Option<PathBuf>) -> (Self, bool) {
        if let Some(dir) = stranded {
            let bytes = std::fs::read(dir.join("config.txt")).ok().or_else(|| primary.and_then(|p| std::fs::read(p).ok()));
            let mut cfg = Self::parse(&bytes.map(|b| text_of(&b)).unwrap_or_default());
            cfg.unreadable = true;
            return (cfg, false);
        }
        let primary = primary.map(|p| read_retrying(&p));
        let first_run = match &primary {
            Some(Ok(_)) => false,
            Some(Err(e)) => e.kind() == std::io::ErrorKind::NotFound,
            None => true,
        };
        let unreadable = matches!(primary, Some(Err(_))) && !first_run;
        let bytes = match primary {
            Some(Ok(b)) => Some(b),
            _ if first_run => legacy.and_then(|p| std::fs::read(p).ok()),
            _ => None,
        };
        let mut cfg = Self::parse(&bytes.map(|b| text_of(&b)).unwrap_or_default());
        cfg.unreadable = unreadable;
        (cfg, first_run)
    }

    fn parse(text: &str) -> Self {
        let mut cfg = Self {
            hotkeys: [
                DEFAULT_HOTKEY.to_string(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                DEFAULT_NESTED_HOTKEY.to_string(),
                DEFAULT_RECORD_HOTKEY.to_string(),
            ],
            auto_save: false,
            save_dir: default_save_dir(),
            file_name: DEFAULT_FILE_NAME.to_string(),
            theme: ThemeMode::System,
            remember_colors: true,
            colors: std::array::from_fn(|_| String::new()),
            format: Kind::Png,
            nested_capture: true,
            shell_menu: SHELL_MODERN,
            compression: CUSTOM,
            quality: 80,
            rec: crate::recfmt::Settings::default(),
            convert: crate::convert_prefs::ConvertPrefs::default(),
            unreadable: false,
        };
        let mut legacy_balanced = false;
        for line in text.lines() {
            let legacy = line.strip_prefix("hotkey=").filter(|v| !v.trim().is_empty()).map(|v| (Action::Snip, v));
            let hotkey_line = legacy
                .or_else(|| Action::ALL.iter().find_map(|a| line.strip_prefix(&format!("hotkey.{}=", a.key())).map(|v| (*a, v))));
            if let Some((action, v)) = hotkey_line {
                let v = v.trim();
                if v.is_empty() {
                    cfg.hotkeys[action.index()] = String::new();
                } else if validate_hotkey(v).is_ok() {
                    cfg.hotkeys[action.index()] = v.to_string();
                }
            } else if let Some(v) = line.strip_prefix("auto_save=") {
                cfg.auto_save = v.trim() == "1";
            } else if let Some(v) = line.strip_prefix("shell_menu=") {
                cfg.shell_menu = match v.trim() {
                    "0" => SHELL_OFF,
                    "2" => SHELL_CLASSIC,
                    _ => SHELL_MODERN,
                };
            } else if let Some(v) = line.strip_prefix("nested_capture=") {
                cfg.nested_capture = v.trim() == "1";
            } else if let Some(v) = line.strip_prefix("format=") {
                if let Some(k) = Kind::from_extension(v.trim()).filter(|k| OUTPUT_KINDS.contains(k)) {
                    cfg.format = k;
                }
            } else if let Some(v) = line.strip_prefix("compression=") {
                match v.trim() {
                    "balanced" => {
                        cfg.compression = CUSTOM;
                        legacy_balanced = true;
                    }
                    "custom" => cfg.compression = CUSTOM,
                    "lossless" => cfg.compression = LOSSLESS,
                    _ => {}
                }
            } else if let Some(v) = line.strip_prefix("quality=") {
                if let Ok(q) = v.trim().parse::<u32>() {
                    cfg.quality = QUALITY_STEPS[quality_step(q.clamp(1, 100) as u8)].1;
                }
            } else if cfg.rec.parse_line(line) || cfg.convert.parse_line(line) {
            } else if let Some(v) = line.strip_prefix("remember_colors=") {
                cfg.remember_colors = v.trim() != "0";
            } else if let Some((i, v)) = line
                .strip_prefix("color.")
                .and_then(|r| r.split_once('='))
                .and_then(|(i, v)| Some((i.parse::<usize>().ok().filter(|i| *i < 9)?, v)))
            {
                if parse_color(v).is_some() {
                    cfg.colors[i] = v.trim().to_string();
                }
            } else if let Some(v) = line.strip_prefix("theme=") {
                if let Some(m) = ThemeMode::parse(v) {
                    cfg.theme = m;
                }
            } else if let Some(v) = line.strip_prefix("file_name=") {
                if !v.trim().is_empty() && v.trim().chars().count() <= 120 {
                    cfg.file_name = v.trim().to_string();
                }
            } else if let Some(v) = line.strip_prefix("save_dir=") {
                let dir = PathBuf::from(v.trim());
                if dir.is_absolute() && !v.contains('\u{FFFD}') {
                    cfg.save_dir = dir;
                }
            }
        }
        if legacy_balanced {
            cfg.quality = QUALITY_STEPS[1].1;
        }
        cfg
    }

    pub fn effective_compression(&self) -> u8 {
        match self.format {
            Kind::Png => LOSSLESS,
            Kind::Webp if self.compression == LOSSLESS => LOSSLESS,
            _ => CUSTOM,
        }
    }

    pub fn output_params(&self) -> Params {
        let mut p = Params::new(self.format);
        match self.effective_compression() {
            LOSSLESS if self.format.has_lossless_switch() => p.lossless = true,
            CUSTOM if self.format.has_quality() => p.quality = self.quality,
            _ => {}
        }
        p
    }

    pub fn save(&self) -> Result<()> {
        if self.unreadable {
            return Err(anyhow!(UNREADABLE));
        }
        self.save_to(&config_path().ok_or_else(|| anyhow!("no settings folder"))?)
    }

    fn save_to(&self, path: &std::path::Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        crate::convert::write_atomic(
            path,
            format!(
                "{}auto_save={}\nsave_dir={}\nfile_name={}\ntheme={}\nremember_colors={}\nnested_capture={}\nshell_menu={}\nformat={}\ncompression={}\nquality={}\n{}{}{}",
                Action::ALL
                    .iter()
                    .map(|a| format!("hotkey.{}={}\n", a.key(), self.hotkeys[a.index()]))
                    .collect::<String>(),
                if self.auto_save { 1 } else { 0 },
                self.save_dir.display(),
                self.file_name,
                self.theme.key(),
                if self.remember_colors { 1 } else { 0 },
                if self.nested_capture { 1 } else { 0 },
                self.shell_menu.min(SHELL_CLASSIC),
                self.format.ext(),
                if self.compression == LOSSLESS { "lossless" } else { "custom" },
                self.quality,
                self.rec.lines(),
                self.convert.lines(),
                self.colors
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| !c.is_empty())
                    .map(|(i, c)| format!("color.{i}={c}\n"))
                    .collect::<String>()
            )
            .as_bytes(),
        )?;
        Ok(())
    }
}

fn text_of(bytes: &[u8]) -> String {
    let utf16 = |b: &[u8], unit: fn([u8; 2]) -> u16| {
        String::from_utf16_lossy(&b.as_chunks::<2>().0.iter().map(|c| unit(*c)).collect::<Vec<u16>>())
    };
    if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        return utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        return utf16(rest, u16::from_be_bytes);
    }
    String::from_utf8_lossy(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes)).into_owned()
}

pub fn validate_hotkey(spec: &str) -> std::result::Result<(), &'static str> {
    use global_hotkey::hotkey::{Code, HotKey, Modifiers};
    use std::str::FromStr;
    if spec.trim().is_empty() {
        return Err("Empty shortcut");
    }
    let hk = HotKey::from_str(spec.trim()).map_err(|_| "That key can't be used")?;
    let (key, mods) = (hk.key, hk.mods);
    let ctrl = mods.contains(Modifiers::CONTROL);
    let alt = mods.contains(Modifiers::ALT);
    let shift = mods.contains(Modifiers::SHIFT);
    let win = mods.contains(Modifiers::SUPER);
    let count = [ctrl, alt, shift, win].iter().filter(|b| **b).count();
    let function_key = matches!(
        key,
        Code::F1 | Code::F2 | Code::F3 | Code::F4 | Code::F5 | Code::F6 | Code::F7 | Code::F8 | Code::F9 | Code::F10
            | Code::F11 | Code::F12 | Code::F13 | Code::F14 | Code::F15 | Code::F16 | Code::F17 | Code::F18
            | Code::F19 | Code::F20 | Code::F21 | Code::F22 | Code::F23 | Code::F24
    );
    let standalone = function_key || matches!(key, Code::PrintScreen | Code::Pause | Code::ScrollLock);
    if !standalone && !(ctrl || win || count >= 2) {
        return Err("Hold Ctrl or Win (or two modifiers) with the key");
    }
    let only = |c, a, s, w| (ctrl, alt, shift, win) == (c, a, s, w);
    let reserved = (only(false, true, false, false) && matches!(key, Code::F4 | Code::Space | Code::Tab | Code::Escape))
        || (only(true, false, false, false)
            && matches!(
                key,
                Code::Escape
                    | Code::F4
                    | Code::KeyC
                    | Code::KeyV
                    | Code::KeyX
                    | Code::KeyZ
                    | Code::KeyY
                    | Code::KeyA
                    | Code::KeyS
                    | Code::KeyF
                    | Code::KeyP
                    | Code::KeyW
                    | Code::KeyT
                    | Code::KeyN
                    | Code::KeyO
                    | Code::KeyR
                    | Code::KeyH
            ))
        || (only(false, false, true, false) && matches!(key, Code::Delete | Code::F10));
    if reserved {
        return Err("Windows and apps use that one; pick another");
    }
    Ok(())
}

pub fn same_hotkey(a: &str, b: &str) -> bool {
    use global_hotkey::hotkey::HotKey;
    use std::str::FromStr;
    match (HotKey::from_str(a.trim()), HotKey::from_str(b.trim())) {
        (Ok(x), Ok(y)) => x.mods == y.mods && x.key == y.key,
        _ => false,
    }
}

pub fn pretty_hotkey(s: &str) -> String {
    s.split('+')
        .map(|part| match part.to_ascii_uppercase().as_str() {
            "CONTROL" | "CTRL" => "Ctrl".to_string(),
            "ALT" => "Alt".to_string(),
            "SHIFT" => "Shift".to_string(),
            "SUPER" | "META" | "WIN" => "Win".to_string(),
            _ => part
                .strip_prefix("Key")
                .or_else(|| part.strip_prefix("Digit"))
                .unwrap_or(part)
                .to_string(),
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

const VALUE_NAME: windows::core::PCWSTR = w!("Snptu");
const RUN_KEY: windows::core::PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const APPROVED_KEY: windows::core::PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run");

fn open_key(path: windows::core::PCWSTR, access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Result<HKEY> {
    let mut key = HKEY::default();
    unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path, None, access, &mut key) }
        .ok()
        .map_err(|e| anyhow!("open registry key: {e}"))?;
    Ok(key)
}

pub(crate) fn reg_string(root: HKEY, key: &str, name: Option<&str>) -> Option<String> {
    let (key, name) = (crate::wide(key), name.map(crate::wide));
    let (key, name) = (PCWSTR(key.as_ptr()), name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr())));
    let mut size = 0u32;
    unsafe { RegGetValueW(root, key, name, RRF_RT_REG_SZ, None, None, Some(&mut size)) }.ok().ok()?;
    let mut buf = vec![0u16; size as usize / 2 + 1];
    size = (buf.len() * 2) as u32;
    unsafe { RegGetValueW(root, key, name, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut size)) }.ok().ok()?;
    let len = (size as usize / 2).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]).trim_end_matches('\0').to_string())
}

fn run_value() -> Option<String> {
    reg_string(HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Run", Some("Snptu"))
}

fn turned_off(approved: &[u8]) -> bool {
    approved.first().is_some_and(|b| b & 1 == 1)
}

fn switched_off_in_windows() -> bool {
    let Ok(key) = open_key(APPROVED_KEY, KEY_QUERY_VALUE) else { return false };
    let mut buf = [0u8; 16];
    let mut len = buf.len() as u32;
    let ok = unsafe { RegQueryValueExW(key, VALUE_NAME, None, None, Some(buf.as_mut_ptr()), Some(&mut len)) }.is_ok();
    unsafe {
        let _ = RegCloseKey(key);
    }
    ok && turned_off(&buf[..(len as usize).min(buf.len())])
}

pub fn autostart_enabled() -> bool {
    let Some(value) = run_value() else { return false };
    let ours = std::env::current_exe().is_ok_and(|exe| value.eq_ignore_ascii_case(&format!("\"{}\"", exe.display())));
    ours && !switched_off_in_windows()
}

pub fn set_autostart(on: bool) -> Result<()> {
    let cmd = if on { Some(format!("\"{}\"", std::env::current_exe()?.display())) } else { None };
    let key = if on {
        let mut key = HKEY::default();
        unsafe { RegCreateKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut key, None) }
            .ok()
            .map_err(|e| anyhow!("create registry key: {e}"))?;
        key
    } else {
        open_key(RUN_KEY, KEY_SET_VALUE)?
    };
    let res = if let Some(cmd) = cmd {
        let bytes: Vec<u8> = crate::wide(&cmd).iter().flat_map(|u| u.to_le_bytes()).collect();
        unsafe { RegSetValueExW(key, VALUE_NAME, None, REG_SZ, Some(&bytes)) }.ok()
    } else {
        let _ = unsafe { RegDeleteValueW(key, VALUE_NAME) };
        Ok(())
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    res.map_err(|e| anyhow!("write Run key: {e}"))?;
    if on
        && let Ok(key) = open_key(APPROVED_KEY, KEY_SET_VALUE)
    {
        let gone = unsafe { RegDeleteValueW(key, VALUE_NAME) };
        unsafe {
            let _ = RegCloseKey(key);
        }
        if gone.is_err() && gone != ERROR_FILE_NOT_FOUND {
            return Err(anyhow!("clear the startup switch: {gone:?}"));
        }
    }
    Ok(())
}
