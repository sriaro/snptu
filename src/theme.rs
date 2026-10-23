
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::core::w;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ThemeMode {
    #[default]
    System,
    Dark,
    Light,
}

impl ThemeMode {
    pub const ALL: [ThemeMode; 3] = [ThemeMode::System, ThemeMode::Dark, ThemeMode::Light];

    pub fn key(self) -> &'static str {
        match self {
            ThemeMode::System => "system",
            ThemeMode::Dark => "dark",
            ThemeMode::Light => "light",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|m| m.key().eq_ignore_ascii_case(s.trim()))
    }

    pub fn is_dark(self) -> bool {
        match self {
            ThemeMode::Dark => true,
            ThemeMode::Light => false,
            ThemeMode::System => system_is_dark(),
        }
    }
}

pub fn system_is_dark() -> bool {
    let mut value = 1u32;
    let mut size = size_of::<u32>() as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut std::ffi::c_void),
            Some(&mut size),
        )
    };
    r.is_err() || value == 0
}

static UI_DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_ui_dark(dark: bool) {
    UI_DARK.store(dark, std::sync::atomic::Ordering::Relaxed);
}

pub fn ui_dark() -> bool {
    UI_DARK.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn accent(dark: bool) -> ([u8; 3], [u8; 3]) {
    if dark { ([18, 140, 132], [50, 96, 204]) } else { ([20, 184, 170], [59, 120, 245]) }
}

pub fn success(dark: bool) -> [u8; 3] {
    if dark { [44, 178, 132] } else { [16, 158, 118] }
}

#[derive(Clone, Copy)]
pub struct Pal {
    pub dark: bool,
    pub text: [u8; 3],
    pub dim: [u8; 3],
    pub danger: [u8; 3],
    pub switch_off: [u8; 3],
    pub tint: [u8; 4],
    pub card: [u8; 4],
    pub edge: [[u8; 4]; 3],
    pub sep: [u8; 4],
    pub chip: [[u8; 4]; 2],
    pub chip_border: [u8; 4],
    pub lift: [u8; 4],
    pub field: [u8; 4],
    pub field_edit: [u8; 4],
}

impl Pal {
    pub fn new(dark: bool) -> Self {
        if dark {
            Self {
                dark,
                text: [239, 243, 250],
                dim: [151, 165, 186],
                danger: [255, 115, 120],
                switch_off: [59, 70, 89],
                tint: [10, 15, 26, 102],
                card: [255, 255, 255, 18],
                edge: [[255, 255, 255, 76], [255, 255, 255, 28], [255, 255, 255, 44]],
                sep: [255, 255, 255, 20],
                chip: [[255, 255, 255, 40], [255, 255, 255, 18]],
                chip_border: [255, 255, 255, 36],
                lift: [255, 255, 255, 26],
                field: [255, 255, 255, 14],
                field_edit: [255, 255, 255, 24],
            }
        } else {
            Self {
                dark,
                text: [22, 32, 56],
                dim: [84, 98, 124],
                danger: [214, 48, 62],
                switch_off: [168, 180, 200],
                tint: [244, 248, 255, 85],
                card: [255, 255, 255, 153],
                edge: [[255, 255, 255, 232], [255, 255, 255, 190], [255, 255, 255, 210]],
                sep: [20, 32, 64, 26],
                chip: [[255, 255, 255, 224], [255, 255, 255, 128]],
                chip_border: [255, 255, 255, 232],
                lift: [59, 120, 245, 40],
                field: [255, 255, 255, 120],
                field_edit: [255, 255, 255, 190],
            }
        }
    }
}
