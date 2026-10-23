
use anyhow::{Result, anyhow, bail};
use std::{
    ffi::c_void,
    fs::File,
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
pub use crate::recfmt::Settings;
use tiny_skia::{Paint, PathBuilder, Pixmap, Stroke, Transform};
use windows::{
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM},
        Graphics::Gdi::{
            AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, BitBlt, CAPTUREBLT, CreateCompatibleDC,
            CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, GetDC, HBITMAP, HDC, HGDIOBJ, ReleaseDC, SRCCOPY, SelectObject,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CURSOR_SHOWING, CURSORINFO, CreateWindowExW, DI_NORMAL, DefWindowProcW, DestroyWindow, DrawIconEx, GetCursorInfo, GetIconInfo,
            HICON, ICONINFO, RegisterClassExW, SW_SHOWNOACTIVATE, ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WNDCLASSEXW, WS_EX_LAYERED,
            WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
        },
    },
    core::w,
};

const MAX_PIXELS: u64 = 8 * 1024 * 1024;
const CLICK_MS: u128 = 350;
const BORDER_MARGIN: i32 = 5;

type Picture = (Instant, Vec<u8>);

pub struct Recorder {
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    full: Arc<AtomicBool>,
    grab: std::thread::Thread,
}

impl Recorder {
    pub fn set_paused(&self, on: bool) {
        self.paused.store(on, Ordering::SeqCst);
        self.grab.unpark();
    }

    pub fn stop(self) {
        drop(self);
    }

    pub fn reached_size_limit(&self) -> bool {
        self.full.load(Ordering::SeqCst)
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.grab.unpark();
    }
}

pub fn check_area(area: [i32; 4]) -> Result<()> {
    let [_, _, w, h] = area;
    if w < 16 || h < 16 {
        bail!("the region is too small to record");
    }
    if w as u64 * h as u64 > MAX_PIXELS || w > 4096 || h > 4096 {
        bail!("the region is too large to record");
    }
    Ok(())
}

pub fn start(area: [i32; 4], settings: Settings, done: impl FnOnce(Result<PathBuf, String>) + Send + 'static) -> Result<Recorder> {
    check_area(area)?;
    let [_, _, w, h] = area;
    let work = work_dir();
    sweep(&work, std::process::id(), process_alive);
    let path = work.join(format!(
        "snptu-rec-{}-{}.{}.part",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()),
        settings.ext()
    ));
    let pipeline = if settings.mp4 {
        crate::recfmt::Pipeline::new_mp4(settings, (w as u32, h as u32), path.clone())?
    } else {
        let file = File::create(&path).map_err(|e| anyhow!("can't create the recording file: {e}"))?;
        crate::recfmt::Pipeline::new(settings, (w as u32, h as u32), file).inspect_err(|_| {
            let _ = std::fs::remove_file(&path);
        })?
    };
    let stop = Arc::new(AtomicBool::new(false));
    let full = Arc::new(AtomicBool::new(false));
    let unreadable = Arc::new(OnceLock::<String>::new());
    let (tx, rx) = mpsc::sync_channel::<Picture>(4);
    let (back, used) = mpsc::sync_channel::<Vec<u8>>(1);

    let flag = stop.clone();
    let paused = Arc::new(AtomicBool::new(false));
    let pause_flag = paused.clone();
    let enc_unreadable = unreadable.clone();
    let grabber = std::thread::Builder::new().name("snptu-rec-grab".into()).spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        let mut grab = match Grabber::new(area, settings.pointer) {
            Ok(g) => g,
            Err(e) => {
                let _ = unreadable.set(format!("the screen can't be captured ({e:#})"));
                return;
            }
        };
        let step = Duration::from_micros(1_000_000 / settings.grab_fps().max(1));
        let begin = Instant::now();
        let mut next = begin;
        let limit = Duration::from_secs(settings.max_secs as u64);
        let mut cut = Duration::ZERO;
        let mut paused_since: Option<Instant> = None;
        let (mut tried, mut taken) = (false, false);
        while !flag.load(Ordering::SeqCst) && begin.elapsed().saturating_sub(cut + paused_since.map_or(Duration::ZERO, |t| t.elapsed())) < limit {
            if pause_flag.load(Ordering::SeqCst) {
                paused_since.get_or_insert_with(Instant::now);
                while used.try_recv().is_ok() {}
                std::thread::park_timeout(Duration::from_secs(1));
                continue;
            }
            if let Some(t) = paused_since.take() {
                cut += t.elapsed();
                next = Instant::now();
            }
            tried = true;
            if let Some(px) = grab.grab() {
                taken = true;
                let at = Instant::now().checked_sub(cut).unwrap_or(begin);
                if let Err(mpsc::TrySendError::Disconnected(_)) = tx.try_send((at, into_buffer(px, &used))) {
                    break;
                }
            }
            next += step;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else {
                next = now;
            }
        }
        if tried && !taken {
            let _ = unreadable.set("the screen can't be captured".to_string());
        }
    });
    let grab = match grabber {
        Ok(g) => g.thread().clone(),
        Err(e) => {
            drop(pipeline);
            let _ = std::fs::remove_file(&path);
            return Err(e.into());
        }
    };

    let enc_path = path.clone();
    let (enc_stop, enc_full) = (stop.clone(), full.clone());
    let encoder = std::thread::Builder::new().name("snptu-rec-encode".into()).spawn(move || {
        crate::par::lower_priority();
        let mut cut_short = None;
        let result = guarded(&enc_path, || {
            let (result, warning) = run_encoder(pipeline, rx, &back, &enc_path, &enc_stop, &enc_full, &enc_unreadable);
            cut_short = warning;
            result
        });
        done(result);
        if let Some(why) = cut_short {
            tell_cut_short(&why);
        }
    });
    if let Err(e) = encoder {
        stop.store(true, Ordering::SeqCst);
        grab.unpark();
        let _ = std::fs::remove_file(&path);
        return Err(e.into());
    }

    Ok(Recorder { stop, paused, full, grab })
}

trait Encode {
    fn frame(&mut self, at: Instant, bgra: &mut [u8]) -> Result<crate::recfmt::Flow>;
    fn finish(self) -> Result<()>;
}

impl Encode for crate::recfmt::Pipeline {
    fn frame(&mut self, at: Instant, bgra: &mut [u8]) -> Result<crate::recfmt::Flow> {
        crate::recfmt::Pipeline::frame(self, at, bgra)
    }

    fn finish(self) -> Result<()> {
        crate::recfmt::Pipeline::finish(self)
    }
}

fn run_encoder(
    mut pipeline: impl Encode,
    rx: mpsc::Receiver<Picture>,
    back: &mpsc::SyncSender<Vec<u8>>,
    path: &Path,
    stop: &AtomicBool,
    full: &AtomicBool,
    unreadable: &OnceLock<String>,
) -> (Result<PathBuf, String>, Option<String>) {
    let mut failure = None;
    let (mut pictures, mut kept) = (false, false);
    while let Ok((at, mut bgra)) = rx.recv() {
        pictures = true;
        let flow = pipeline.frame(at, &mut bgra);
        let _ = back.try_send(bgra);
        match flow {
            Ok(crate::recfmt::Flow::Continue) => kept = true,
            Ok(crate::recfmt::Flow::Full) => {
                full.store(true, Ordering::SeqCst);
                stop.store(true, Ordering::SeqCst);
                break;
            }
            Err(e) => {
                failure = Some(failure_text(&e));
                stop.store(true, Ordering::SeqCst);
                break;
            }
        }
    }
    drop(rx);
    match failure {
        Some(e) if kept => match pipeline.finish() {
            Ok(()) => (Ok(path.to_path_buf()), Some(e)),
            Err(_) => (Err(e), None),
        },
        Some(e) => {
            drop(pipeline);
            (Err(e), None)
        }
        None if !pictures => {
            drop(pipeline);
            (Err(unreadable.get().cloned().unwrap_or_else(|| NOTHING_RECORDED.to_string())), None)
        }
        None => (pipeline.finish().map(|()| path.to_path_buf()).map_err(|e| failure_text(&e)), None),
    }
}

fn tell_cut_short(why: &str) {
    crate::log(&format!("record: ended early: {why}"));
    if crate::session::ending() {
        return;
    }
    let text = cut_short_text(why);
    std::thread::spawn(move || crate::convert::message_box(&text));
}

fn cut_short_text(why: &str) -> String {
    format!("The recording stopped early because of an error. What was recorded until then is kept.\n\n{}", crate::sentence(why))
}

fn into_buffer(px: &[u8], used: &mpsc::Receiver<Vec<u8>>) -> Vec<u8> {
    match used.try_recv() {
        Ok(mut buf) if buf.len() == px.len() => {
            buf.copy_from_slice(px);
            buf
        }
        _ => px.to_vec(),
    }
}

pub(crate) const NOTHING_RECORDED: &str = "nothing was recorded";

fn guarded(path: &Path, encode: impl FnOnce() -> Result<PathBuf, String>) -> Result<PathBuf, String> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(encode)).unwrap_or_else(|_| Err("the recorder stopped unexpectedly".to_string()));
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
}

fn failure_text(e: &anyhow::Error) -> String {
    match e.chain().find_map(|c| c.downcast_ref::<std::io::Error>()) {
        Some(io) => format!("the recording file can't be written: {io}"),
        None => format!("{e:#}"),
    }
}

pub fn ext_of(tmp: &Path) -> &'static str {
    let name = tmp.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if name.contains(".mp4.") {
        "mp4"
    } else if name.contains(".webp.") {
        "webp"
    } else {
        "gif"
    }
}

pub(crate) fn work_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("Snptu").join("tmp"))
        .filter(|d| std::fs::create_dir_all(d).is_ok())
        .unwrap_or_else(std::env::temp_dir)
}

pub fn clean_stale() {
    let own = std::process::id();
    if let Some(dir) = std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Snptu").join("tmp")) {
        sweep(&dir, own, process_alive);
    }
    sweep(&std::env::temp_dir(), own, process_alive);
}

fn sweep(dir: &Path, own: u32, alive: impl Fn(u32) -> bool) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        if rec_pid(&e.file_name().to_string_lossy()).is_some_and(|pid| pid != own && !alive(pid)) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn rec_pid(name: &str) -> Option<u32> {
    name.strip_prefix("snptu-rec-")?.split('-').next()?.parse().ok()
}

fn process_alive(pid: u32) -> bool {
    use windows::Win32::{
        Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, WAIT_TIMEOUT},
        System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    };
    unsafe {
        let process = match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            Ok(p) => p,
            Err(e) => return e.code() != ERROR_INVALID_PARAMETER.to_hresult(),
        };
        let running = WaitForSingleObject(process, 0) == WAIT_TIMEOUT;
        let _ = CloseHandle(process);
        running
    }
}


struct Grabber {
    screen: HDC,
    mem: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut c_void,
    area: [i32; 4],
    pointer: u8,
    last_press: Option<Instant>,
}

impl Grabber {
    fn new(area: [i32; 4], pointer: u8) -> Result<Self> {
        unsafe {
            let screen = GetDC(None);
            if screen.is_invalid() {
                bail!("GetDC failed");
            }
            let mem = CreateCompatibleDC(Some(screen));
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: area[2],
                    biHeight: -area[3],
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bmp = CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0);
            match bmp {
                Ok(bmp) if !bits.is_null() => {
                    let old = SelectObject(mem, HGDIOBJ(bmp.0));
                    Ok(Self { screen, mem, bmp, old, bits, area, pointer, last_press: None })
                }
                _ => {
                    let _ = DeleteDC(mem);
                    ReleaseDC(None, screen);
                    bail!("CreateDIBSection failed")
                }
            }
        }
    }

    fn grab(&mut self) -> Option<&[u8]> {
        let [x, y, w, h] = self.area;
        unsafe {
            BitBlt(self.mem, 0, 0, w, h, Some(self.screen), x, y, SRCCOPY | CAPTUREBLT).ok()?;
            let _ = GdiFlush();
            let mut ci = CURSORINFO { cbSize: size_of::<CURSORINFO>() as u32, ..Default::default() };
            if self.pointer >= 1 && GetCursorInfo(&mut ci).is_ok() && ci.flags.0 & CURSOR_SHOWING.0 != 0 {
                if self.pointer >= 2 {
                    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON};
                    let down = [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON].iter().any(|k| GetAsyncKeyState(k.0 as i32) as u16 & 0x8000 != 0);
                    if down {
                        self.last_press = Some(Instant::now());
                    }
                    if let Some(t) = self.last_press {
                        let age = t.elapsed().as_millis();
                        if age < CLICK_MS {
                            let buf = std::slice::from_raw_parts_mut(self.bits as *mut u8, (w * h * 4) as usize);
                            highlight(buf, w as usize, h as usize, ci.ptScreenPos.x - x, ci.ptScreenPos.y - y, 1.0 - age as f32 / CLICK_MS as f32);
                        }
                    }
                }
                let icon = HICON(ci.hCursor.0);
                let mut info = ICONINFO::default();
                if GetIconInfo(icon, &mut info).is_ok() {
                    let _ = DrawIconEx(
                        self.mem,
                        ci.ptScreenPos.x - info.xHotspot as i32 - x,
                        ci.ptScreenPos.y - info.yHotspot as i32 - y,
                        icon,
                        0,
                        0,
                        0,
                        None,
                        DI_NORMAL,
                    );
                    if !info.hbmMask.is_invalid() {
                        let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
                    }
                    if !info.hbmColor.is_invalid() {
                        let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
                    }
                }
            }
            let _ = GdiFlush();
            Some(std::slice::from_raw_parts(self.bits as *const u8, (w * h * 4) as usize))
        }
    }
}

impl Drop for Grabber {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.mem, self.old);
            let _ = DeleteObject(HGDIOBJ(self.bmp.0));
            let _ = DeleteDC(self.mem);
            ReleaseDC(None, self.screen);
        }
    }
}


fn highlight(buf: &mut [u8], w: usize, h: usize, cx: i32, cy: i32, strength: f32) {
    const R: i32 = 22;
    let (x0, x1) = ((cx - R).max(0), (cx + R + 1).min(w as i32));
    let (y0, y1) = ((cy - R).max(0), (cy + R + 1).min(h as i32));
    for y in y0..y1 {
        for x in x0..x1 {
            let d = (((x - cx).pow(2) + (y - cy).pow(2)) as f32).sqrt();
            if d > R as f32 + 0.5 {
                continue;
            }
            let a = if d > R as f32 - 2.5 { 0.85 } else { 0.38 } * strength;
            let i = (y as usize * w + x as usize) * 4;
            for (c, tint) in [(0usize, 20.0f32), (1, 205.0), (2, 255.0)] {
                buf[i + c] = (buf[i + c] as f32 * (1.0 - a) + tint * a) as u8;
            }
        }
    }
}


pub fn move_file(rec: &mut PathBuf, to: &Path) -> std::io::Result<()> {
    if std::fs::rename(&*rec, to).is_ok() {
        return Ok(());
    }
    copy_file(rec, to, std::convert::identity)
}

fn copy_file<R: std::io::Read>(rec: &mut PathBuf, to: &Path, read: impl FnOnce(File) -> R) -> std::io::Result<()> {
    if let Ok(kept) = keep(rec) {
        *rec = kept;
    }
    let copied = File::open(&*rec).and_then(|f| crate::convert::copy_atomic(read(f), to));
    if let Err(e) = copied {
        if std::fs::metadata(to).is_ok_and(|m| m.len() == 0) {
            let _ = std::fs::remove_file(to);
        }
        return Err(e);
    }
    let _ = std::fs::remove_file(&*rec);
    Ok(())
}

pub fn keep(rec: &Path) -> std::io::Result<PathBuf> {
    let name = rec.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if name.starts_with("snptu-kept-") {
        return Ok(rec.to_path_buf());
    }
    let kept = rec.with_file_name(format!("snptu-kept-{}", name.trim_start_matches("snptu-rec-").trim_end_matches(".part")));
    std::fs::rename(rec, &kept).map(|()| kept)
}

pub fn claim_name(dir: &Path, stem: &str, ext: &str) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind};
    use std::os::windows::fs::OpenOptionsExt;
    let open_link = windows::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT.0;
    if !dir.is_absolute() {
        return Err(Error::new(ErrorKind::InvalidInput, "save folder must be an absolute path"));
    }
    std::fs::create_dir_all(dir)?;
    for n in 0..10_000u32 {
        let name = if n == 0 { format!("{stem}.{ext}") } else { format!("{stem}_{n}.{ext}") };
        let path = dir.join(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).custom_flags(open_link).open(&path) {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(Error::other("no free file name"))
}


pub struct Border {
    hwnd: HWND,
    margin: i32,
    rgb: [u8; 3],
    editable: bool,
}

const HANDLE_MARGIN: i32 = 14;

type BorderHook = fn(u32, HWND) -> bool;

thread_local! {
    static BORDER_HOOK: std::cell::Cell<Option<BorderHook>> = const { std::cell::Cell::new(None) };
}

pub fn set_border_hook(hook: BorderHook) {
    BORDER_HOOK.with(|h| h.set(Some(hook)));
}

unsafe extern "system" fn border_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{WM_CAPTURECHANGED, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_SETCURSOR};
    let for_hook = match msg {
        WM_CAPTURECHANGED => lparam.0 != hwnd.0 as isize,
        _ => matches!(msg, WM_LBUTTONDOWN | WM_LBUTTONUP | WM_MOUSEMOVE | WM_SETCURSOR),
    };
    if for_hook
        && let Some(hook) = BORDER_HOOK.with(|h| h.get())
        && std::panic::catch_unwind(|| hook(msg, hwnd)).unwrap_or(false)
    {
        return LRESULT(if msg == WM_SETCURSOR { 1 } else { 0 });
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

impl Border {
    pub fn new(area: [i32; 4], rgb: [u8; 3]) -> Option<Self> {
        Self::make(area, rgb, false)
    }

    pub fn editable(area: [i32; 4], rgb: [u8; 3]) -> Option<Self> {
        Self::make(area, rgb, true)
    }

    pub fn move_to(&self, area: [i32; 4]) {
        use windows::Win32::UI::WindowsAndMessaging::{SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos};
        unsafe {
            let _ = SetWindowPos(self.hwnd, None, area[0] - self.margin, area[1] - self.margin, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }

    pub fn set_area(&self, area: [i32; 4]) {
        self.draw(area);
    }

    fn make(area: [i32; 4], rgb: [u8; 3], editable: bool) -> Option<Self> {
        use std::sync::Once;
        static ONCE: Once = Once::new();
        let hinst = unsafe { GetModuleHandleW(None) }.ok().map(|h| HINSTANCE(h.0))?;
        ONCE.call_once(|| unsafe {
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(border_proc),
                hInstance: hinst,
                lpszClassName: w!("SnptuRecBorder"),
                ..Default::default()
            };
            RegisterClassExW(&wc);
        });
        let style = if editable {
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE
        } else {
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE
        };
        let hwnd = unsafe { CreateWindowExW(style, w!("SnptuRecBorder"), w!(""), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst), None) }.ok()?;
        let border = Self { hwnd, margin: if editable { HANDLE_MARGIN } else { BORDER_MARGIN }, rgb, editable };
        border.draw(area)?;
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        Some(border)
    }

    fn draw(&self, area: [i32; 4]) -> Option<()> {
        let m = self.margin;
        let (ww, wh) = (area[2] + 2 * m, area[3] + 2 * m);
        let mut pix = Pixmap::new(ww as u32, wh as u32)?;
        let mut paint = Paint::default();
        paint.set_color_rgba8(self.rgb[0], self.rgb[1], self.rgb[2], 255);
        paint.anti_alias = true;
        if self.editable {
            pix.fill(tiny_skia::Color::from_rgba8(0, 0, 0, 1));
        }
        let (lx, ly, lw, lh) = ((m - 3) as f32, (m - 3) as f32, (area[2] + 6) as f32, (area[3] + 6) as f32);
        let ring = {
            let mut pb = PathBuilder::new();
            pb.push_rect(tiny_skia::Rect::from_xywh(lx, ly, lw, lh)?);
            pb.finish()?
        };
        pix.stroke_path(&ring, &paint, &Stroke { width: 2.0, ..Default::default() }, Transform::identity(), None);
        if self.editable {
            let mut fill = Paint::default();
            fill.set_color_rgba8(255, 255, 255, 255);
            fill.anti_alias = true;
            let (xs, ys) = ([lx, lx + lw / 2.0, lx + lw], [ly, ly + lh / 2.0, ly + lh]);
            for (i, &x) in xs.iter().enumerate() {
                for (j, &y) in ys.iter().enumerate() {
                    if i == 1 && j == 1 {
                        continue;
                    }
                    let Some(dot) = PathBuilder::from_circle(x, y, 5.0) else { continue };
                    pix.fill_path(&dot, &fill, tiny_skia::FillRule::Winding, Transform::identity(), None);
                    pix.stroke_path(&dot, &paint, &Stroke { width: 2.0, ..Default::default() }, Transform::identity(), None);
                }
            }
        }
        unsafe {
            let dc = CreateCompatibleDC(None);
            let Some((bmp, bits)) = crate::menu::make_dib(dc, ww, wh) else {
                let _ = DeleteDC(dc);
                return None;
            };
            let old = SelectObject(dc, HGDIOBJ(bmp.0));
            let dst = std::slice::from_raw_parts_mut(bits as *mut u8, (ww * wh * 4) as usize);
            crate::menu::rgba_to_bgra(dst, pix.data());
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
            let pos = POINT { x: area[0] - m, y: area[1] - m };
            let size = SIZE { cx: ww, cy: wh };
            let src = POINT { x: 0, y: 0 };
            let _ = UpdateLayeredWindow(self.hwnd, None, Some(&pos), Some(&size), Some(dc), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
            SelectObject(dc, old);
            let _ = DeleteObject(HGDIOBJ(bmp.0));
            let _ = DeleteDC(dc);
        }
        Some(())
    }
}

impl Drop for Border {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
