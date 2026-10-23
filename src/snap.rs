
use windows::Win32::{
    Foundation::{HWND, LPARAM, RECT},
    Graphics::{
        Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
        Gdi::{EnumDisplayMonitors, HDC, HMONITOR},
    },
    UI::WindowsAndMessaging::{
        EnumChildWindows, EnumWindows, GWL_EXSTYLE, GetClassNameW, GetWindowLongW, GetWindowRect,
        IsIconic, IsWindowVisible, WS_EX_LAYERED, WS_EX_TRANSPARENT,
    },
};

pub struct SnapWin {
    pub rect: [f32; 4],
    pub children: Vec<[f32; 4]>,
}

struct State {
    skip: HWND,
    out: Vec<SnapWin>,
    frame: (i32, i32, i32, i32),
    kids: Vec<[f32; 4]>,
}

fn to_frame(r: RECT, frame: (i32, i32, i32, i32), min: i32) -> Option<[f32; 4]> {
    let (fx, fy, fw, fh) = frame;
    let x0 = (r.left - fx).clamp(0, fw);
    let y0 = (r.top - fy).clamp(0, fh);
    let x1 = (r.right - fx).clamp(0, fw);
    let y1 = (r.bottom - fy).clamp(0, fh);
    (x1 - x0 >= min && y1 - y0 >= min).then_some([x0 as f32, y0 as f32, x1 as f32, y1 as f32])
}

fn clip_to(r: [f32; 4], outer: [f32; 4], min: f32) -> Option<[f32; 4]> {
    let c = [r[0].max(outer[0]), r[1].max(outer[1]), r[2].min(outer[2]), r[3].min(outer[3])];
    (c[2] - c[0] >= min && c[3] - c[1] >= min).then_some(c)
}

fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

unsafe extern "system" fn on_window(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    let st = unsafe { &mut *(lparam.0 as *mut State) };
    unsafe {
        if hwnd == st.skip || !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return true.into();
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_LAYERED.0 != 0 && ex & WS_EX_TRANSPARENT.0 != 0 {
            return true.into();
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4);
        if cloaked != 0 {
            return true.into();
        }
        if matches!(class_of(hwnd).as_str(), "Progman" | "WorkerW") {
            return true.into();
        }
        let mut r = RECT::default();
        let ok = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut _,
            size_of::<RECT>() as u32,
        )
        .is_ok();
        if !ok && GetWindowRect(hwnd, &mut r).is_err() {
            return true.into();
        }
        let Some(rect) = to_frame(r, st.frame, 24) else { return true.into() };
        st.kids.clear();
        let _ = EnumChildWindows(Some(hwnd), Some(on_child), LPARAM(st as *mut State as isize));
        let children = st.kids.drain(..).filter_map(|c| clip_to(c, rect, 16.0)).collect();
        st.out.push(SnapWin { rect, children });
    }
    true.into()
}

unsafe extern "system" fn on_child(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    let st = unsafe { &mut *(lparam.0 as *mut State) };
    unsafe {
        if st.kids.len() < 200 && IsWindowVisible(hwnd).as_bool() {
            let mut r = RECT::default();
            if GetWindowRect(hwnd, &mut r).is_ok()
                && let Some(rect) = to_frame(r, st.frame, 16)
            {
                st.kids.push(rect);
            }
        }
    }
    true.into()
}

unsafe extern "system" fn on_monitor(_: HMONITOR, _: HDC, r: *mut RECT, lparam: LPARAM) -> windows::core::BOOL {
    let st = unsafe { &mut *(lparam.0 as *mut State) };
    if let Some(rect) = to_frame(unsafe { *r }, st.frame, 24) {
        st.out.push(SnapWin { rect, children: Vec::new() });
    }
    true.into()
}

pub fn collect(frame: (i32, i32, i32, i32), skip: Option<HWND>) -> Vec<SnapWin> {
    let mut st = State { skip: skip.unwrap_or_default(), out: Vec::new(), frame, kids: Vec::new() };
    unsafe {
        let _ = EnumWindows(Some(on_window), LPARAM(&mut st as *mut State as isize));
        let _ = EnumDisplayMonitors(None, None, Some(on_monitor), LPARAM(&mut st as *mut State as isize));
    }
    st.out
}

fn contains(r: [f32; 4], p: (f32, f32)) -> bool {
    p.0 >= r[0] && p.0 < r[2] && p.1 >= r[1] && p.1 < r[3]
}

pub fn pick(list: &[SnapWin], p: (f32, f32)) -> Option<[f32; 4]> {
    let win = list.iter().find(|w| contains(w.rect, p))?;
    let area = |r: &[f32; 4]| (r[2] - r[0]) * (r[3] - r[1]);
    win.children
        .iter()
        .filter(|c| contains(**c, p) && area(c) < area(&win.rect) * 0.98)
        .min_by(|a, b| area(a).total_cmp(&area(b)))
        .copied()
        .or(Some(win.rect))
}
