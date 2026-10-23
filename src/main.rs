#![windows_subsystem = "windows"]

mod anim;
mod annot;
mod app_pins;
mod app_settings;
mod capture;
mod config;
mod dialogs;
mod failmsg;
mod gfx;
mod icon;
mod app_convert;
mod app_menu;
mod convert_prefs;
mod convert_ui;
mod color;
mod deep;
mod convert;
mod loupe;
mod menu;
mod msgbox;
mod par;
mod picker;
mod theme;
mod mem;
mod pin;
mod readers;
mod recfmt;
mod recbar;
mod mp4;
mod shell_pkg;
mod record;
mod session;
mod settings;
mod shell_menu;
mod shaped;
mod snap;
mod text;
mod toolbar;

use annot::{Annot, Ctx, Layer, PALETTE, Style, Tool};
use anyhow::Result;
use config::{Action, Config};
use pin::{PendingPin, Pin, PinCmd};
use settings::SettingsWindow;
use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::HotKey,
};
use cosmic_text::FontSystem;
use shaped::Shaper;
use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex, Weak},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use text::Text;
use unicode_segmentation::UnicodeSegmentation;
use tiny_skia::Pixmap;
use toolbar::{Item, Toolbar, UiState};
use tray_icon::{
    Icon, MouseButton as TrayButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::MenuId,
};
use winit::{
    application::ApplicationHandler,
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey},
    platform::windows::{EventLoopBuilderExtWindows, WindowAttributesExtWindows},
    window::{CursorIcon, Window, WindowId, WindowLevel},
};

const GPU_WARM: Duration = Duration::from_secs(45);

const GPU_HOLD: Duration = Duration::from_secs(300);

const QUIT_WAIT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy)]
enum MenuAction {
    Capture,
    Record,
    Settings,
    Convert,
    ConvertPick(app_convert::ConvertPick),
    ToggleHidePins,
    DestroyAllPins,
    Quit,
    Pin(WindowId, PinCmd),
}

enum UserEvent {
    Menu(MenuId),
    Hotkey(u32),
    Capture,
    ConvertRequest,
    ConvertProgress,
    TrayMenu(i32, i32),
    Char(char),
    RecordDone(u64, Result<PathBuf, String>),
    RecordSaved,
    SessionEnding,
    CloseApp,
    ShotWritten,
    RecBar(recbar::Ev),
    PickColor(usize, usize, Option<[u8; 3]>, bool),
    SaveChosen(u64, Result<Option<PathBuf>, String>),
    PinFile(WindowId, Result<Option<(Vec<u8>, u32, u32)>, String>),
    PastePin(Option<(Vec<u8>, u32, u32)>, (i32, i32)),
    SettingsFolder(WindowId, settings::Hit, Result<Option<PathBuf>, String>),
    ConvertFolder(WindowId, Result<Option<PathBuf>, String>),
    ConvertFiles(WindowId, Result<Option<Vec<PathBuf>>, String>),
    GpuWarmed,
    ConvertEstimate(u64, Result<convert::Estimate, String>),
}

struct PendingSave {
    id: u64,
    dialog: u64,
    params: convert::Params,
    w: u32,
    h: u32,
    px: Vec<u8>,
}

struct SaveDone(EventLoopProxy<UserEvent>);

impl Drop for SaveDone {
    fn drop(&mut self) {
        let _ = self.0.send_event(UserEvent::RecordSaved);
    }
}

struct ShotDone(EventLoopProxy<UserEvent>);

impl Drop for ShotDone {
    fn drop(&mut self) {
        let _ = self.0.send_event(UserEvent::ShotWritten);
    }
}

#[derive(Clone, Copy)]
enum Sel {
    None,
    Dragging { from: (f32, f32), to: (f32, f32) },
    Done([f32; 4]),
}

struct TbState {
    tb: Toolbar,
    pix: Pixmap,
    origin: (f32, f32),
    hover: Option<usize>,
}

fn text_size(level: u32, scale: f32) -> f32 {
    (11.0 + 2.0 * level as f32) * scale
}

impl Session {
    fn pick(&self, p: (f32, f32), text: Option<&Text>, shaper: Option<&RefCell<Shaper>>) -> (Option<usize>, bool) {
        if self.tool == Some(Tool::Eraser) {
            return (None, false);
        }
        let Some(l) = self.layer.as_ref() else { return (None, false) };
        let cx = Ctx { frame: &self.frame, origin: l.origin, text, shaper, scale: self.scale };
        let handle = if self.editing { None } else { l.handle_at(p, HANDLE_REACH * self.scale, &cx) };
        (handle, handle.is_none() && l.hit_any(p, PICK_REACH * self.scale, &cx))
    }

    fn clamp_to_layer(&self, p: (f32, f32)) -> (f32, f32) {
        let b = self.layer_reach().unwrap_or([0.0, 0.0, self.frame.w as f32, self.frame.h as f32]);
        (p.0.clamp(b[0], b[2]), p.1.clamp(b[1], b[3]))
    }

    fn layer_reach(&self) -> Option<[f32; 4]> {
        let l = self.layer.as_ref()?;
        Some([l.origin.0 as f32, l.origin.1 as f32, (l.origin.0 + l.w as i32) as f32, (l.origin.1 + l.h as i32) as f32])
    }

    fn apply_slider(&mut self, idx: usize, px: f32) {
        let (Some(t), Some(tool)) = (self.tb.as_ref(), self.tool) else { return };
        let x = px - t.origin.0;
        let (level, pos) = (t.tb.slider_level(idx, x), t.tb.slider_pos(idx, x));
        let style = &mut self.styles[tool.index()];
        if t.tb.items[idx].0 == Item::Strength {
            style.strength = level as u8;
            self.strength_t = Some(pos);
        } else if style.uses_size(tool) {
            style.width = level;
            self.slider_t = Some(pos);
        }
    }
}

struct Session {
    frame: capture::Frame,
    geom_fixes: u8,
    copy_on_select: bool,
    record: bool,
    windows: Vec<snap::SnapWin>,
    hover: Option<[f32; 4]>,
    press_hover: Option<[f32; 4]>,
    hex_mode: bool,
    dark: bool,
    color_copied: bool,
    sel: Sel,
    adjust: Option<Adjust>,
    cur_icon: CursorIcon,
    last_press: Option<Instant>,
    last_press_pos: (f32, f32),
    scale: f32,
    layer: Option<Layer>,
    tb: Option<TbState>,
    tool: Option<Tool>,
    styles: [Style; 9],
    slider_drag: Option<usize>,
    slider_t: Option<f32>,
    strength_t: Option<f32>,
    ui_dirty: bool,
    dragging: bool,
    editing: bool,
    edit: Option<EditDrag>,
}

struct EditDrag {
    handle: Option<usize>,
    start: (f32, f32),
    orig: Annot,
    grab: (f32, f32),
    lifted: bool,
}

const HANDLE_REACH: f32 = 8.0;
const PICK_REACH: f32 = 5.0;

fn handle_icon(a: Option<&Annot>, h: usize) -> CursorIcon {
    if matches!(a, Some(Annot::Arrow { .. })) {
        return CursorIcon::Crosshair;
    }
    match h {
        0 | 4 => CursorIcon::NwseResize,
        2 | 6 => CursorIcon::NeswResize,
        1 | 5 => CursorIcon::NsResize,
        _ => CursorIcon::EwResize,
    }
}

enum LayerOp {
    SetLive(Option<Annot>),
    Commit(Annot),
    Undo,
    Redo,
    Erase((f32, f32), f32),
    Refresh,
    Deselect,
    SelectAt((f32, f32), f32),
    BeginEdit,
    EndEdit(bool),
    Delete,
    Restyle(Tool, Style),
}

struct App {
    proxy: EventLoopProxy<UserEvent>,
    window: Option<Arc<Window>>,
    gfx: Option<gfx::Gfx>,
    text: Option<Arc<Text>>,
    ui_bold: Option<Arc<Text>>,
    loupe_info: loupe::Info,
    shaper: Option<Rc<RefCell<Shaper>>>,
    shaper_job: Option<JoinHandle<FontSystem>>,
    redraw_retries: u8,
    gfx_deadline: Option<Instant>,
    warm: Option<std::thread::JoinHandle<anyhow::Result<gfx::Gfx>>>,
    suspended: Vec<(Action, HotKey)>,
    pending_pins: Vec<PendingPin>,
    release_after_pins: bool,
    pins: Vec<Pin>,
    pins_click_through: bool,
    menu_actions: HashMap<MenuId, MenuAction>,
    session: Option<Session>,
    cursor: (f32, f32),
    mods: ModifiersState,
    config: Config,
    autostart: bool,
    settings: Option<SettingsWindow>,
    convert: Option<convert_ui::ConvertWindow>,
    hotkeys: [Option<HotKey>; config::ACTION_COUNT],
    recorder: Option<record::Recorder>,
    pending_save: Option<PendingSave>,
    save_seq: u64,
    rec_area: [i32; 4],
    rec_seq: u64,
    rec_id: u64,
    finishing: HashMap<u64, bool>,
    saving: u32,
    quitting: bool,
    writing: u32,
    quit_by: Option<Instant>,
    converting: Vec<Arc<convert::Progress>>,
    hotkey_mgr: Option<GlobalHotKeyManager>,
    _tray: Option<TrayIcon>,
    tray_check: Option<(Instant, usize)>,
}

fn rect_of(a: (f32, f32), b: (f32, f32), w: f32, h: f32) -> [f32; 4] {
    let (x0, x1) = (a.0.min(b.0).clamp(0.0, w), a.0.max(b.0).clamp(0.0, w));
    let (y0, y1) = (a.1.min(b.1).clamp(0.0, h), a.1.max(b.1).clamp(0.0, h));
    [x0, y0, x1, y1]
}

fn is_click(from: (f32, f32), to: (f32, f32)) -> bool {
    (to.0 - from.0).abs() < 4.0 && (to.1 - from.1).abs() < 4.0
}

fn drag_rect(from: (f32, f32), to: (f32, f32), press_hover: Option<[f32; 4]>, w: f32, h: f32) -> [f32; 4] {
    match press_hover {
        Some(r) if is_click(from, to) => r,
        _ => rect_of(from, to, w, h),
    }
}

fn inside(r: [f32; 4], p: (f32, f32)) -> bool {
    p.0 >= r[0] && p.0 <= r[2] && p.1 >= r[1] && p.1 <= r[3]
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct Grip {
    l: bool,
    t: bool,
    r: bool,
    b: bool,
}

impl Grip {
    const MOVE: Grip = Grip { l: true, t: true, r: true, b: true };

    fn is_move(self) -> bool {
        self == Self::MOVE
    }

    fn icon(self) -> CursorIcon {
        match (self.l, self.t, self.r, self.b) {
            (true, true, true, true) => CursorIcon::Move,
            (true, true, false, false) | (false, false, true, true) => CursorIcon::NwseResize,
            (false, true, true, false) | (true, false, false, true) => CursorIcon::NeswResize,
            (true, false, false, false) | (false, false, true, false) => CursorIcon::EwResize,
            _ => CursorIcon::NsResize,
        }
    }
}

#[derive(Clone, Copy)]
struct Adjust {
    grip: Grip,
    start: [f32; 4],
    press: (f32, f32),
}

const MIN_SEL: f32 = 3.0;

fn hit_grip(r: [f32; 4], p: (f32, f32), tol: f32, allow_move: bool) -> Option<Grip> {
    let near_x = p.0 >= r[0] - tol && p.0 <= r[2] + tol;
    let near_y = p.1 >= r[1] - tol && p.1 <= r[3] + tol;
    let pick = |lo: f32, hi: f32, v: f32, on: bool| -> (bool, bool) {
        if !on {
            return (false, false);
        }
        let (dl, dh) = ((v - lo).abs(), (v - hi).abs());
        (dl <= tol && dl <= dh, dh <= tol && dh < dl)
    };
    let (l, rr) = pick(r[0], r[2], p.0, near_y);
    let (t, b) = pick(r[1], r[3], p.1, near_x);
    let g = Grip { l, t, r: rr, b };
    if l || t || rr || b {
        Some(g)
    } else if allow_move && inside(r, p) {
        Some(Grip::MOVE)
    } else {
        None
    }
}

fn adjusted(adj: Adjust, cursor: (f32, f32), fw: f32, fh: f32) -> [f32; 4] {
    let [x0, y0, x1, y1] = adj.start;
    let (mut dx, mut dy) = ((cursor.0 - adj.press.0).round(), (cursor.1 - adj.press.1).round());
    if adj.grip.is_move() {
        dx = dx.clamp(-x0, fw - x1);
        dy = dy.clamp(-y0, fh - y1);
        return [x0 + dx, y0 + dy, x1 + dx, y1 + dy];
    }
    let g = adj.grip;
    let nx0 = if g.l { (x0 + dx).clamp(0.0, x1 - MIN_SEL) } else { x0 };
    let nx1 = if g.r { (x1 + dx).clamp(x0 + MIN_SEL, fw) } else { x1 };
    let ny0 = if g.t { (y0 + dy).clamp(0.0, y1 - MIN_SEL) } else { y0 };
    let ny1 = if g.b { (y1 + dy).clamp(y0 + MIN_SEL, fh) } else { y1 };
    [nx0, ny0, nx1, ny1]
}

impl App {
    fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    pub(crate) fn begin_capture(&mut self, copy_on_select: bool) {
        if self.session.is_some() {
            return;
        }
        self.start_capture(copy_on_select, false);
    }

    pub(crate) fn begin_nested_capture(&mut self) {
        if self.session.is_none() {
            return self.start_capture(false, false);
        }
        if picker::is_open() {
            picker::close(false);
        }
        menu::dismiss();
        self.cancel_text();
        let copy = self.session.as_ref().is_some_and(|s| s.copy_on_select);
        self.start_capture(copy, false);
        if let Some(p) = self.pending_save.take() {
            dialogs::close_dialog(p.dialog);
        }
    }

    pub(crate) fn toggle_record(&mut self) {
        if recbar::is_open() {
            recbar::shortcut();
            return;
        }
        if self.stop_recorder(false) {
            return;
        }
        if self.session.is_none() {
            self.start_capture(false, true);
        }
    }

    fn stop_recorder(&mut self, discard: bool) -> bool {
        let Some(r) = self.recorder.take() else { return false };
        self.finishing.insert(self.rec_id, discard);
        r.stop();
        true
    }

    fn begin_recording(&mut self, area: [i32; 4]) {
        let scale = self.session.as_ref().map_or(1.0, |s| s.scale);
        let shot = self.session.take();
        if let Some(w) = &self.window {
            w.set_visible(false);
        }
        let (Some(text), Ok(())) = (self.text.clone(), record::check_area(area)) else {
            drop(shot);
            self.end_capture();
            beep();
            return;
        };
        self.rec_area = area;
        let proxy = self.proxy.clone();
        let opened = recbar::open(area, self.config.rec, scale, self.config.theme.is_dark(), text, shot.as_ref().map(|s| &s.frame), move |ev| {
            let _ = proxy.send_event(UserEvent::RecBar(ev));
        });
        drop(shot);
        self.end_capture();
        if !opened {
            beep();
        }
    }

    pub(crate) fn capture_request(&mut self, record: bool) {
        if self.session.is_none() && self.recorder.is_none() && !recbar::is_open() {
            self.start_capture(false, record);
        }
    }

    fn on_rec_bar(&mut self, ev: recbar::Ev) {
        use recbar::Ev;
        match ev {
            Ev::Begin => {
                if menu::finish_fade() {
                    unsafe {
                        let _ = windows::Win32::Graphics::Dwm::DwmFlush();
                    }
                }
                let proxy = self.proxy.clone();
                let id = self.rec_seq + 1;
                match record::start(self.rec_area, self.config.rec, move |res| {
                    let _ = proxy.send_event(UserEvent::RecordDone(id, res));
                }) {
                    Ok(r) => {
                        self.rec_seq = id;
                        self.rec_id = id;
                        self.recorder = Some(r);
                    }
                    Err(e) => {
                        log(&format!("record: {e:#}"));
                        recbar::close();
                        record_failed(&format!("{e:#}"), None);
                    }
                }
            }
            Ev::TooSmall(w, h) => {
                if let Some(why) = mp4::too_small((w, h)) {
                    static TOO_SMALL_UP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                    if let Some(up) = OneNotice::take(&TOO_SMALL_UP) {
                        let text = too_small_text(&why);
                        std::thread::spawn(move || {
                            convert::message_box(&text);
                            drop(up);
                        });
                    }
                }
            }
            Ev::Pause(on) => {
                if let Some(r) = &self.recorder {
                    r.set_paused(on);
                }
            }
            Ev::Stop | Ev::Discard => {
                recbar::close();
                self.stop_recorder(ev == Ev::Discard);
            }
            Ev::Cancel => recbar::close(),
            Ev::Area(a) => self.rec_area = a,
            Ev::Reselect => {
                recbar::close();
                if self.session.is_none() {
                    self.start_capture(false, true);
                }
            }
            Ev::Change(s) => {
                self.config.rec = s;
                if let Some(w) = &self.settings {
                    w.window.request_redraw();
                }
                if let Err(e) = self.config.save() {
                    log(&format!("config save: {e:#}"));
                }
            }
        }
    }

    fn warm_up_gpu(&mut self) {
        let Some(window) = self.window.clone() else { return };
        let start = match gfx::Gfx::begin(window) {
            Ok(s) => s,
            Err(e) => return log(&format!("gpu warm-up failed: {e:#}")),
        };
        let proxy = self.proxy.clone();
        self.warm = Some(std::thread::spawn(move || {
            let g = gfx::Gfx::finish(start);
            let _ = proxy.send_event(UserEvent::GpuWarmed);
            g
        }));
    }

    fn adopt_warm_gpu(&mut self) {
        let Some(handle) = self.warm.take() else { return };
        match handle.join() {
            Ok(Ok(mut g)) if self.gfx.is_none() => {
                g.set_text(self.text.clone());
                log(&format!("gpu {} warmed up", g.adapter_name));
                self.gfx = Some(g);
                if self.session.is_none() {
                    self.gfx_deadline = Some(Instant::now() + GPU_WARM);
                }
            }
            Ok(Err(e)) => log(&format!("gpu warm-up failed: {e:#}")),
            _ => {}
        }
    }

    fn finish_recording(&mut self, id: u64, res: Result<PathBuf, String>) {
        let stopped = self.finishing.remove(&id);
        let mut full = false;
        if stopped.is_none() && self.rec_id == id {
            full = self.recorder.take().is_some_and(|r| r.reached_size_limit());
            recbar::close();
        }
        if self.recorder.is_none() && self.finishing.is_empty() && self.session.is_none() && !self.quitting && !session::ending() {
            mem::trim();
        }
        let quiet = session::ending();
        let discard = stopped.unwrap_or(false);
        let tmp = match res {
            Ok(p) if discard => {
                let _ = std::fs::remove_file(&p);
                return;
            }
            Ok(p) => p,
            Err(_) if discard => return,
            Err(e) => {
                log(&format!("record: {e}"));
                if quiet {
                    return;
                }
                self.saving += 1;
                record_failed(&e, Some(SaveDone(self.proxy.clone())));
                return;
            }
        };
        let ext = record::ext_of(&tmp);
        let stem = config::expand_name(&self.config.file_name, &config::now_stamp());
        let auto_save = self.config.auto_save || quiet;
        let unsaved = Unsaved::new(tmp, stem, self.config.save_dir.clone());
        self.saving += 1;
        let done = SaveDone(self.proxy.clone());
        std::thread::spawn(move || {
            let _done = done;
            let u = unsaved;
            if full && !auto_save {
                convert::message_box(&size_limit_text(ext));
            }
            let chosen = (!auto_save).then(|| {
                let label = match ext {
                    "mp4" => "MP4 video",
                    "webp" => "WebP animation",
                    _ => "GIF animation",
                };
                match dialogs::save_formats_token(u.dialog, None, &u.stem, &[(label, ext)]) {
                    Ok(Some((mut path, _))) => {
                        ensure_extension(&mut path, ext);
                        Ok(Some(path))
                    }
                    Ok(None) => Ok(None),
                    Err(e) => Err(format!("{e:#}")),
                }
            });
            let mut file = locked(&u.file);
            let Some(tmp) = file.take() else { return };
            let dest = match chosen {
                None => record::claim_name(&u.save_dir, &u.stem, ext).map_err(|e| e.to_string()),
                Some(Ok(Some(path))) => Ok(path),
                Some(Ok(None)) => {
                    let _ = std::fs::remove_file(&tmp);
                    return;
                }
                Some(Err(e)) => Err(e),
            };
            let told = store_recording(&tmp, dest, &u.stem, ext, quiet);
            let quiet = quiet || session::ending();
            drop(file);
            if let Some(text) = told.filter(|_| !quiet) {
                convert::message_box(&text);
            }
            if full && auto_save && !quiet {
                convert::message_box(&size_limit_text(ext));
            }
        });
    }

    fn quit(&mut self, el: &ActiveEventLoop) {
        self.remember_convert_options();
        recbar::close();
        self.stop_recorder(false);
        app_convert::cancel_all(&self.converting);
        if self.quitting || quit_ready(self.recordings_done(), self.writes_pending(), None, Instant::now()) {
            return el.exit();
        }
        self.quitting = true;
        self.quit_by = Some(Instant::now() + QUIT_WAIT);
        self.hide_while_quitting();
    }

    fn hide_while_quitting(&mut self) {
        if !mark_quitting(QUITTING) {
            log("quitting mark unavailable");
        }
        self.tray_check = None;
        TRAY_HWND.store(0, std::sync::atomic::Ordering::Relaxed);
        self._tray = None;
        if self.settings.is_some() {
            self.close_settings();
        }
        self.unregister_hotkeys();
        if self.convert.is_some() {
            self.close_convert();
        }
        for p in &mut self.pins {
            p.set_hidden(true);
        }
    }

    fn session_ending(&mut self) {
        recbar::close();
        self.stop_recorder(false);
    }

    fn recordings_done(&self) -> bool {
        self.recorder.is_none() && self.finishing.is_empty() && self.saving == 0
    }

    fn writes_pending(&self) -> u32 {
        self.writing + app_convert::running(&self.converting)
    }

    fn quit_if_waiting(&self, el: &ActiveEventLoop) {
        if self.quitting && quit_ready(self.recordings_done(), self.writes_pending(), self.quit_by, Instant::now()) {
            el.exit();
        }
    }

    fn write_shot(&mut self, target: SaveTarget, params: convert::Params, w: u32, h: u32, px: Vec<u8>) {
        self.writing += 1;
        write_in_background(target, params, w, h, px, ShotDone(self.proxy.clone()));
    }

    fn start_capture(&mut self, copy_on_select: bool, record: bool) {
        if self.gfx.as_ref().is_some_and(|g| g.probe_lost()) {
            log("gpu device lost; recreating the renderer");
            self.gfx = None;
        }
        let Some(window) = self.window.clone() else { return };
        self.gfx_deadline = None;
        if menu::finish_fade() {
            unsafe {
                let _ = windows::Win32::Graphics::Dwm::DwmFlush();
            }
        }
        let frame = match capture::capture_virtual_screen() {
            Ok(f) => f,
            Err(e) => {
                log(&format!("capture failed: {e:#}"));
                self.gfx_deadline = Some(Instant::now() + GPU_WARM);
                beep();
                return;
            }
        };
        if self.gfx.is_none() && self.warm.is_none() {
            self.warm_up_gpu();
        }
        let windows = snap::collect((frame.x, frame.y, frame.w as i32, frame.h as i32), hwnd_of(&window));
        self.adopt_warm_gpu();
        if self.gfx.is_none() {
            let started = Instant::now();
            match gfx::Gfx::new(window.clone()) {
                Ok(mut g) => {
                    g.set_text(self.text.clone());
                    log(&format!("gpu {} created in {} ms", g.adapter_name, started.elapsed().as_millis()));
                    self.gfx = Some(g);
                }
                Err(e) => {
                    log(&format!("gpu init failed: {e:#}"));
                    self.gfx_deadline = Some(Instant::now() + GPU_WARM);
                    beep();
                    return;
                }
            }
        }
        let Some(gfx) = &mut self.gfx else { return };
        let window = &window;
        if let Err(e) = gfx.set_frame(frame.w, frame.h, frame.bgra()) {
            log(&format!("upload failed: {e:#}"));
            self.gfx_deadline = Some(Instant::now() + GPU_WARM);
            beep();
            return;
        }
        {
            use windows::Win32::{Foundation::POINT, UI::WindowsAndMessaging::GetCursorPos};
            let mut p = POINT::default();
            if unsafe { GetCursorPos(&mut p) }.is_ok() {
                self.cursor = ((p.x - frame.x) as f32, (p.y - frame.y) as f32);
            }
        }
        let hover = snap::pick(&windows, self.cursor);
        let scale = window.scale_factor();
        gfx.set_scale(scale);
        gfx.clear_layer();
        gfx.clear_ui();
        place_overlay(window, &frame);
        window.set_cursor(CursorIcon::Crosshair);
        gfx.draw(None);
        window.set_visible(true);
        window.focus_window();
        window.request_redraw();
        self.gfx_deadline = None;
        self.session = Some(Session {
            frame,
            geom_fixes: 0,
            copy_on_select,
            record,
            windows,
            hover,
            press_hover: None,
            hex_mode: false,
            dark: self.config.theme.is_dark(),
            color_copied: false,
            adjust: None,
            cur_icon: CursorIcon::Crosshair,
            sel: Sel::None,
            last_press: None,
            last_press_pos: (0.0, 0.0),
            scale: scale.clamp(1.0, 4.0) as f32,
            layer: None,
            tb: None,
            tool: None,
            styles: initial_styles(&self.config),
            slider_drag: None,
            slider_t: None,
            strength_t: None,
            ui_dirty: false,
            dragging: false,
            editing: false,
            edit: None,
        });
    }

    fn end_capture(&mut self) {
        if let Some(p) = self.pending_save.take() {
            dialogs::close_dialog(p.dialog);
        }
        if picker::is_open() {
            picker::close(false);
        }
        self.session = None;
        if let Some(w) = &self.window {
            w.set_ime_allowed(false);
            w.set_visible(false);
        }
        self.shaper = None;
        self.shaper_job = None;
        if self.pending_pins.is_empty() {
            self.release_gpu();
        } else {
            self.release_after_pins = true;
        }
    }

    fn release_gpu(&mut self) {
        if let Some(g) = &mut self.gfx {
            g.release();
        }
        mem::trim();
        self.gfx_deadline = Some(Instant::now() + GPU_HOLD);
    }

    fn enforce_overlay_geometry(&mut self) {
        let (Some(w), Some(s)) = (&self.window, self.session.as_mut()) else { return };
        let size = w.inner_size();
        let pos = w.outer_position().ok();
        let ok = size.width == s.frame.w
            && size.height == s.frame.h
            && pos.is_some_and(|p| p.x == s.frame.x && p.y == s.frame.y);
        if !ok && s.geom_fixes < 4 {
            s.geom_fixes += 1;
            place_overlay(w, &s.frame);
        }
    }

    fn current_rect(&self) -> Option<[f32; 4]> {
        let s = self.session.as_ref()?;
        match s.sel {
            Sel::None => s.hover,
            Sel::Dragging { from, to } => Some(drag_rect(from, to, s.press_hover, s.frame.w as f32, s.frame.h as f32)),
            Sel::Done(r) => Some(r),
        }
    }


    fn layer_op(&mut self, op: LayerOp) {
        let (Some(s), Some(g)) = (self.session.as_mut(), self.gfx.as_mut()) else { return };
        let Sel::Done(_) = s.sel else { return };
        let Some(layer) = s.layer.as_mut() else { return };
        let cx = Ctx {
            frame: &s.frame,
            origin: layer.origin,
            text: self.text.as_deref(),
            shaper: self.shaper.as_deref(),
            scale: s.scale,
        };
        let region = match op {
            LayerOp::SetLive(a) => layer.set_live(a, &cx),
            LayerOp::Commit(a) => layer.commit(a, &cx),
            LayerOp::Undo => layer.undo(&cx),
            LayerOp::Redo => layer.redo(&cx),
            LayerOp::Erase(p, tol) => layer.erase_at(p, tol, &cx).unwrap_or([0; 4]),
            LayerOp::Refresh => layer.refresh(&cx),
            LayerOp::Deselect => layer.deselect(&cx),
            LayerOp::SelectAt(p, tol) => layer.select_at(p, tol, &cx).unwrap_or([0; 4]),
            LayerOp::BeginEdit => layer.begin_edit(&cx).map_or([0; 4], |(_, r)| r),
            LayerOp::EndEdit(changed) => layer.end_edit(changed, &cx),
            LayerOp::Delete => layer.delete_selected(&cx).unwrap_or([0; 4]),
            LayerOp::Restyle(tool, st) if layer.selected().is_some_and(|a| a.tool() == tool) => {
                layer.restyle_selected(&st, s.scale, &cx).unwrap_or([0; 4])
            }
            LayerOp::Restyle(..) => [0; 4],
        };
        g.update_layer(region, layer.region_bytes(region));
        self.request_redraw();
    }

    fn layer_query<R>(&self, f: impl FnOnce(&Layer, &Ctx) -> R) -> Option<R> {
        let s = self.session.as_ref()?;
        let layer = s.layer.as_ref()?;
        let cx = Ctx { frame: &s.frame, origin: layer.origin, text: self.text.as_deref(), shaper: self.shaper.as_deref(), scale: s.scale };
        Some(f(layer, &cx))
    }

    fn sync_tool_to_selection(&mut self) {
        let Some(s) = self.session.as_mut() else { return };
        let Some(a) = s.layer.as_ref().and_then(|l| l.selected()) else { return };
        let tool = a.tool();
        let look = a.look(&s.styles[tool.index()], s.scale);
        s.styles[tool.index()] = look;
        s.slider_t = None;
        s.strength_t = None;
        let changed = s.tool != Some(tool);
        s.tool = Some(tool);
        if changed {
            self.place_toolbar();
        }
        self.rerender_ui();
    }

    fn restyle_selected(&mut self) {
        let Some(s) = self.session.as_ref() else { return };
        let Some(tool) = s.tool else { return };
        let st = s.styles[tool.index()];
        self.layer_op(LayerOp::Restyle(tool, st));
    }

    fn edit_move(&mut self, shift: bool) {
        let Some(s) = self.session.as_mut() else { return };
        let p = s.clamp_to_layer(self.cursor);
        let sc = s.scale;
        let grab = s.edit.as_ref().map_or((0.0, 0.0), |e| e.grab);
        let held = s.clamp_to_layer((self.cursor.0 + grab.0, self.cursor.1 + grab.1));
        let Some(e) = s.edit.as_mut() else { return };
        if !e.lifted && e.handle.is_none() && (p.0 - e.start.0).hypot(p.1 - e.start.1) < 3.0 * sc {
            return;
        }
        let first = !e.lifted;
        e.lifted = true;
        let (handle, start, orig) = (e.handle, e.start, e.orig.clone());
        if first {
            self.layer_op(LayerOp::BeginEdit);
        }
        let next = match handle {
            None => {
                let mut a = orig;
                a.translate(p.0 - start.0, p.1 - start.1);
                a
            }
            Some(h) => self.layer_query(|_, cx| orig.dragged(h, held, shift, cx)).unwrap_or(orig),
        };
        self.layer_op(LayerOp::SetLive(Some(next)));
    }

    fn finish_edit(&mut self) {
        let Some(s) = self.session.as_mut() else { return };
        let Some(e) = s.edit.take() else { return };
        if !e.lifted {
            return;
        }
        let changed = s.layer.as_ref().and_then(|l| l.live.as_ref()).is_some_and(|a| *a != e.orig);
        self.layer_op(LayerOp::EndEdit(changed));
        self.rerender_ui();
    }

    fn poll_shaper(&mut self, block: bool) {
        if self.shaper.is_some() {
            return;
        }
        let job = self.shaper_job.get_or_insert_with(|| std::thread::spawn(FontSystem::new));
        if (block || job.is_finished())
            && let Some(job) = self.shaper_job.take()
        {
            match job.join() {
                Ok(fs) => self.shaper = Some(Rc::new(RefCell::new(Shaper::new(fs)))),
                Err(_) => log("font system failed to load"),
            }
        }
    }

    fn rerender_ui(&mut self) {
        if let Some(s) = self.session.as_mut() {
            s.ui_dirty = true;
        }
        self.request_redraw();
    }

    fn flush_ui(&mut self) {
        let (Some(s), Some(g)) = (self.session.as_mut(), self.gfx.as_mut()) else { return };
        if !std::mem::take(&mut s.ui_dirty) {
            return;
        }
        let Some(t) = s.tb.as_mut() else { return };
        let style = s.tool.map_or(Style::default(), |t| s.styles[t.index()]);
        let st = UiState {
            tool: s.tool,
            style,
            can_undo: s.layer.as_ref().is_some_and(|l| l.can_undo()),
            can_redo: s.layer.as_ref().is_some_and(|l| l.can_redo()),
            knob: s.slider_t.filter(|t| 1 + (t * 11.0).round() as u32 == style.width),
            knob2: s.strength_t.filter(|t| 1 + (t * 11.0).round() as u32 == style.strength as u32),
            dark: s.dark,
        };
        t.tb.draw(&mut t.pix, t.hover, &st);
        g.update_ui(t.pix.data());
    }

    fn finalize_selection(&mut self) {
        let (Some(s), Some(g)) = (self.session.as_mut(), self.gfx.as_mut()) else { return };
        let Sel::Dragging { from, to } = s.sel else { return };
        let (fw, fh) = (s.frame.w as f32, s.frame.h as f32);
        let r = rect_of(from, to, fw, fh);
        let r = [r[0].floor(), r[1].floor(), r[2].ceil().min(fw), r[3].ceil().min(fh)];
        let (w, h) = ((r[2] - r[0]) as u32, (r[3] - r[1]) as u32);
        if s.record {
            if w >= 16 && h >= 16 {
                let area = [s.frame.x + r[0] as i32, s.frame.y + r[1] as i32, w as i32, h as i32];
                self.begin_recording(area);
            } else {
                s.sel = Sel::None;
                self.request_redraw();
            }
            return;
        }
        let (lw, lh) = (s.frame.w, s.frame.h);
        let layer = if w >= 3 && h >= 3 { Layer::new(lw, lh, (0, 0)) } else { None };
        let Some(layer) = layer else {
            s.sel = Sel::None;
            self.request_redraw();
            return;
        };
        s.sel = Sel::Done(r);
        s.layer = Some(layer);
        g.set_layer(lw, lh, (0.0, 0.0));

        self.place_toolbar();
        if self.session.as_ref().is_some_and(|s| s.copy_on_select) {
            self.copy_selection();
        }
    }

    fn finish_adjust(&mut self) {
        let (Some(s), Some(_)) = (self.session.as_mut(), self.gfx.as_ref()) else { return };
        if s.adjust.take().is_none() || !matches!(s.sel, Sel::Done(_)) {
            return;
        }
        if s.tb.is_none() {
            self.place_toolbar();
        }
        self.request_redraw();
    }

    fn place_toolbar(&mut self) {
        let (Some(s), Some(g)) = (self.session.as_mut(), self.gfx.as_mut()) else { return };
        let Sel::Done(r) = s.sel else { return };
        let (fw, fh) = (s.frame.w as f32, s.frame.h as f32);
        let (center_x, center_y) = ((r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0);
        let [mx0, my0, mx1, my1] = self
            .window
            .as_ref()
            .and_then(|w| {
                w.available_monitors().find_map(|m| {
                    let (p, sz) = (m.position(), m.size());
                    let b = [
                        (p.x - s.frame.x) as f32,
                        (p.y - s.frame.y) as f32,
                        (p.x - s.frame.x) as f32 + sz.width as f32,
                        (p.y - s.frame.y) as f32 + sz.height as f32,
                    ];
                    (center_x >= b[0] && center_x < b[2] && center_y >= b[1] && center_y < b[3]).then_some(b)
                })
            })
            .unwrap_or([0.0, 0.0, fw, fh]);
        let (tb, origin) = toolbar_layout(s.scale, s.tool, r, [mx0, my0, mx1, my1]);
        g.set_ui(tb.w, tb.h, origin, tb.pill, tb.opt_pill);
        if let Some(pix) = Pixmap::new(tb.w, tb.h) {
            s.tb = Some(TbState { tb, pix, origin, hover: None });
        }
        self.rerender_ui();
    }

    fn clear_selection(&mut self) {
        self.cancel_text();
        if let (Some(s), Some(g)) = (self.session.as_mut(), self.gfx.as_mut()) {
            s.sel = Sel::None;
            s.layer = None;
            s.tb = None;
            s.tool = None;
            s.dragging = false;
            s.adjust = None;
            s.edit = None;
            s.cur_icon = CursorIcon::Crosshair;
            s.hover = snap::pick(&s.windows, self.cursor);
            g.clear_layer();
            g.clear_ui();
        }
        if let Some(w) = &self.window {
            w.set_cursor(CursorIcon::Crosshair);
        }
        self.request_redraw();
    }


    fn frame_cursor(&self) -> Option<(f32, f32)> {
        let s = self.session.as_ref()?;
        let b = s.layer_reach()?;
        Some((self.cursor.0.clamp(b[0], b[2]), self.cursor.1.clamp(b[1], b[3])))
    }

    fn start_annot(&mut self) {
        self.finish_text();
        self.layer_op(LayerOp::Deselect);
        let Some(p) = self.frame_cursor() else { return };
        let Some(s) = self.session.as_mut() else { return };
        let Some(tool) = s.tool else { return };
        let style = s.styles[tool.index()];
        let (color, level, sc) = (style.rgb(), style.width as f32, s.scale);
        let width = level * sc;
        let dash = style.dash != 0;
        let annot = match tool {
            Tool::Rect => Annot::Rect { a: p, b: p, color, width, fill: style.fill, dash },
            Tool::Ellipse => Annot::Ellipse { a: p, b: p, color, width, fill: style.fill, dash },
            Tool::Arrow => Annot::Arrow { a: p, b: p, color, width, style: style.arrow, dash },
            Tool::Pen => Annot::Pen { pts: vec![p], color, width, dash },
            Tool::Marker => Annot::Marker { pts: vec![p], color, width },
            Tool::Mosaic => {
                let blur = style.mosaic == 1;
                let block = annot::mosaic_block(style.strength as f32, sc);
                if style.fill == 0 {
                    let d = annot::brush_diameter(level, sc);
                    Annot::Mosaic { a: p, b: p, pts: vec![p], brush: d, blur, block }
                } else {
                    Annot::Mosaic { a: p, b: p, pts: Vec::new(), brush: 0.0, blur, block }
                }
            }
            Tool::Eraser => {
                s.dragging = true;
                self.layer_op(LayerOp::Erase(p, (3.0 + 1.2 * level) * sc));
                self.rerender_ui();
                return;
            }
            Tool::Number => {
                let n = s
                    .layer
                    .as_ref()
                    .map_or(0, |l| l.items.iter().filter(|a| matches!(a, Annot::Number { .. })).count());
                let n = n as u32 + 1;
                self.layer_op(LayerOp::Commit(Annot::Number { pos: p, n, color, radius: annot::number_radius(level, sc) }));
                self.rerender_ui();
                return;
            }
            Tool::Text => {
                self.finish_text();
                self.poll_shaper(true);
                let Some(s) = self.session.as_mut() else { return };
                let size = text_size(style.width, sc);
                s.editing = true;
                let pos = (p.0, p.1 - size * 0.7);
                if let Some(w) = &self.window {
                    w.set_ime_allowed(true);
                    w.set_ime_cursor_area(
                        PhysicalPosition::new(pos.0, pos.1 + size * 1.4),
                        PhysicalSize::new(10u32, 20u32),
                    );
                }
                self.layer_op(LayerOp::SetLive(Some(Annot::Text {
                    pos,
                    text: String::new(),
                    preedit: String::new(),
                    color,
                    size,
                    bold: style.bold != 0,
                    caret: true,
                })));
                return;
            }
        };
        s.dragging = true;
        self.layer_op(LayerOp::SetLive(Some(annot)));
    }

    fn finish_annot(&mut self) {
        let Some(s) = self.session.as_mut() else { return };
        s.dragging = false;
        if s.tool == Some(Tool::Eraser) {
            self.rerender_ui();
            return;
        }
        let live = s.layer.as_ref().and_then(|l| l.live.clone());
        match live {
            Some(a) if !a.is_tiny() => self.layer_op(LayerOp::Commit(a)),
            _ => self.layer_op(LayerOp::SetLive(None)),
        }
        self.rerender_ui();
    }

    fn finish_text(&mut self) {
        let Some(s) = self.session.as_mut() else { return };
        if !s.editing {
            return;
        }
        s.editing = false;
        let live = s.layer.as_mut().and_then(|l| l.live.take());
        if let Some(w) = &self.window {
            w.set_ime_allowed(false);
        }
        match live {
            Some(Annot::Text { pos, text, color, size, bold, .. }) if !text.trim().is_empty() => {
                self.layer_op(LayerOp::Commit(Annot::Text {
                    pos,
                    text,
                    preedit: String::new(),
                    color,
                    size,
                    bold,
                    caret: false,
                }));
            }
            _ => self.layer_op(LayerOp::SetLive(None)),
        }
        self.rerender_ui();
    }

    fn cancel_text(&mut self) {
        let Some(s) = self.session.as_mut() else { return };
        if !s.editing {
            return;
        }
        s.editing = false;
        if let Some(w) = &self.window {
            w.set_ime_allowed(false);
        }
        self.layer_op(LayerOp::SetLive(None));
    }

    fn type_text(&mut self, input: &str) {
        let Some(s) = self.session.as_mut() else { return };
        let Some(l) = s.layer.as_mut() else { return };
        if let Some(t) = l.live_text_mut() {
            t.extend(input.chars().filter(|c| !c.is_control()));
            l.set_live_preedit(String::new());
            self.layer_op(LayerOp::Refresh);
            self.update_ime_area();
        }
    }

    fn preedit_text(&mut self, input: String) {
        let Some(s) = self.session.as_mut() else { return };
        if !s.editing {
            return;
        }
        if let Some(l) = s.layer.as_mut() {
            l.set_live_preedit(input);
        }
        self.layer_op(LayerOp::Refresh);
    }

    fn on_char(&mut self, c: char) {
        if picker::is_open() {
            return;
        }
        if self.settings_char(c) || self.convert_char(c) {
            return;
        }
        if !self.session.as_ref().is_some_and(|s| s.editing) {
            return;
        }
        self.type_text(c.encode_utf8(&mut [0; 4]));
    }

    fn backspace_text(&mut self) {
        let Some(s) = self.session.as_mut() else { return };
        if let Some(t) = s.layer.as_mut().and_then(|l| l.live_text_mut()) {
            if let Some((i, _)) = t.grapheme_indices(true).next_back() {
                t.truncate(i);
            }
            self.layer_op(LayerOp::Refresh);
            self.update_ime_area();
        }
    }

    fn update_ime_area(&mut self) {
        let (Some(w), Some(s), Some(sh)) = (&self.window, &self.session, &self.shaper) else { return };
        let Some(Annot::Text { pos, text, size, bold, .. }) = s.layer.as_ref().and_then(|l| l.live.as_ref()) else { return };
        let width = sh.borrow_mut().width(text, *size, *bold);
        w.set_ime_cursor_area(
            PhysicalPosition::new(pos.0 + width, pos.1 + size * 1.3),
            PhysicalSize::new(10u32, 20u32),
        );
    }

    fn resize_live_text(&mut self) {
        let Some(s) = self.session.as_mut() else { return };
        if !s.editing || s.tool != Some(Tool::Text) {
            return;
        }
        let size = text_size(s.styles[Tool::Text.index()].width, s.scale);
        if s.layer.as_mut().is_some_and(|l| l.set_live_size(size)) {
            self.layer_op(LayerOp::Refresh);
            self.update_ime_area();
        }
    }

    fn open_color_picker(&mut self) {
        let Some(text) = self.text.clone() else { return };
        let Some(s) = self.session.as_ref() else { return };
        let (Some(t), Some(tool)) = (s.tb.as_ref(), s.tool) else { return };
        let Some(r) = t.tb.items.iter().find(|(i, _)| *i == Item::CustomColor).map(|(_, r)| *r) else { return };
        let style = s.styles[tool.index()];
        let at = (
            (s.frame.x as f32 + t.origin.0 + r[0]).round() as i32,
            (s.frame.y as f32 + t.origin.1 + r[1] + r[3] + 8.0 * s.scale).round() as i32,
        );
        let (ti, prev_color, prev_custom) = (tool.index(), style.color, style.custom);
        let proxy = self.proxy.clone();
        picker::show(at, style.rgb(), s.scale, s.dark, text, move |rgb, pick| {
            let ev = match pick {
                picker::Pick::Cancel => UserEvent::PickColor(ti, prev_color, prev_custom, true),
                _ => UserEvent::PickColor(ti, prev_color, Some(rgb), pick == picker::Pick::Commit),
            };
            let _ = proxy.send_event(ev);
        });
    }

    fn on_pick_color(&mut self, tool: usize, color: usize, custom: Option<[u8; 3]>, done: bool) {
        let Some(s) = self.session.as_mut() else { return };
        if tool >= s.styles.len() {
            return;
        }
        s.styles[tool].color = color;
        s.styles[tool].custom = custom;
        let rgb = s.styles[tool].rgb();
        if s.tool.is_some_and(|t| t.index() == tool)
            && let Some(l) = s.layer.as_mut()
        {
            l.set_live_color(rgb);
        }
        self.restyle_selected();
        self.layer_op(LayerOp::Refresh);
        self.rerender_ui();
        if done {
            self.remember_color(tool, color, custom);
        }
    }

    fn remember_color(&mut self, tool: usize, color: usize, custom: Option<[u8; 3]>) {
        if !self.config.remember_colors || tool >= self.config.colors.len() {
            return;
        }
        let v = config::color_string(color, custom);
        if self.config.colors[tool] == v {
            return;
        }
        self.config.colors[tool] = v;
        if let Err(e) = self.config.save() {
            log(&format!("config save: {e:#}"));
        }
    }

    fn on_item(&mut self, item: Item) {
        match item {
            Item::Tool(t) => {
                self.finish_text();
                self.layer_op(LayerOp::Deselect);
                if t == Tool::Text {
                    self.poll_shaper(false);
                }
                if let Some(s) = self.session.as_mut() {
                    s.tool = if s.tool == Some(t) { None } else { Some(t) };
                }
                self.place_toolbar();
            }
            Item::Swatch(i) => {
                let mut picked = None;
                if let Some(s) = self.session.as_mut() {
                    if let Some(t) = s.tool {
                        s.styles[t.index()].color = i;
                        s.styles[t.index()].custom = None;
                        picked = Some(t.index());
                    }
                    if let Some(l) = s.layer.as_mut() {
                        l.set_live_color(PALETTE[i]);
                    }
                }
                if let Some(t) = picked {
                    self.remember_color(t, i, None);
                }
                self.restyle_selected();
                self.layer_op(LayerOp::Refresh);
                self.rerender_ui();
            }
            Item::CustomColor => self.open_color_picker(),
            Item::Fill(_) | Item::ArrowStyle(_) | Item::Dash | Item::MosaicKind(_) | Item::MosaicShape(_) | Item::Bold => {
                if let Some(s) = self.session.as_mut()
                    && let Some(t) = s.tool
                {
                    let st = &mut s.styles[t.index()];
                    match item {
                        Item::Fill(v) => st.fill = v,
                        Item::ArrowStyle(v) => st.arrow = v,
                        Item::Dash if st.uses_dash(t) => st.dash = 1 - st.dash,
                        Item::MosaicKind(v) => st.mosaic = v,
                        Item::MosaicShape(v) => st.fill = v,
                        Item::Bold => st.bold = 1 - st.bold,
                        _ => {}
                    }
                    let bold = st.bold != 0;
                    if let Some(l) = s.layer.as_mut() {
                        l.set_live_bold(bold);
                    }
                }
                self.restyle_selected();
                self.layer_op(LayerOp::Refresh);
                self.rerender_ui();
            }
            Item::Slider | Item::Strength => {}
            Item::Undo => {
                self.finish_text();
                self.layer_op(LayerOp::Undo);
                self.sync_tool_to_selection();
                self.rerender_ui();
            }
            Item::Redo => {
                self.finish_text();
                self.layer_op(LayerOp::Redo);
                self.sync_tool_to_selection();
                self.rerender_ui();
            }
            Item::Pin => self.pin_selection(),
            Item::Save => self.save_selection(),
            Item::Close => self.end_capture(),
            Item::Confirm => self.copy_selection(),
        }
    }


    fn selection_pixels(&self) -> Option<(u32, u32, Vec<u8>)> {
        let s = self.session.as_ref()?;
        let Sel::Done(r) = s.sel else { return None };
        let (w, h, mut px) = s.frame.crop_rgba(r[0] as u32, r[1] as u32, r[2] as u32, r[3] as u32);
        if w == 0 || h == 0 {
            return None;
        }
        if let Some(l) = &s.layer {
            l.composite_over_at(&mut px, w as usize, h as usize, (r[0] as i32 - l.origin.0, r[1] as i32 - l.origin.1));
        }
        Some((w, h, px))
    }

    fn copy_selection(&mut self) {
        self.finish_text();
        let Some((w, h, px)) = self.selection_pixels() else { return };
        let res = arboard::Clipboard::new().and_then(|mut c| {
            c.set_image(arboard::ImageData {
                width: w as usize,
                height: h as usize,
                bytes: std::borrow::Cow::Borrowed(&px),
            })
        });
        drop(px);
        match res {
            Ok(()) => self.end_capture(),
            Err(e) => {
                log(&format!("clipboard failed: {e}"));
                beep();
            }
        }
    }

    fn save_selection(&mut self) {
        self.finish_text();
        let Some((w, h, px)) = self.selection_pixels() else { return };
        let stem = config::expand_name(&self.config.file_name, &config::now_stamp());
        let params = self.config.output_params();
        let ext = params.kind.ext();
        let target = if self.config.auto_save {
            SaveTarget::Folder { dir: self.config.save_dir.clone(), stem }
        } else {
            if self.pending_save.is_some() {
                return;
            }
            self.save_seq += 1;
            let id = self.save_seq;
            let owner = self.window.as_ref().and_then(|w| hwnd_of(w)).map(|h| h.0 as isize);
            let proxy = self.proxy.clone();
            let label = params.kind.label();
            let dialog = dialogs::new_token();
            std::thread::spawn(move || {
                let owner = owner.map(|v| windows::Win32::Foundation::HWND(v as *mut std::ffi::c_void));
                let res = dialogs::save_formats_token(dialog, owner, &stem, &[(label, ext)]).map(|r| r.map(|(p, _)| p)).map_err(|e| format!("{e:#}"));
                let _ = proxy.send_event(UserEvent::SaveChosen(id, res));
            });
            self.pending_save = Some(PendingSave { id, dialog, params, w, h, px });
            return;
        };
        self.end_capture();
        self.write_shot(target, params, w, h, px);
    }

    fn update_loupe(&mut self) {
        let (Some(s), Some(g)) = (self.session.as_ref(), self.gfx.as_mut()) else { return };
        if !matches!(s.sel, Sel::None | Sel::Dragging { .. }) {
            g.set_loupe(None, None);
            self.loupe_info.reset();
            return;
        }
        let (fw, fh) = (s.frame.w as i32, s.frame.h as i32);
        let (px, py) = ((self.cursor.0 as i32).clamp(0, fw - 1), (self.cursor.1 as i32).clamp(0, fh - 1));
        let i = ((py * fw + px) * 4) as usize;
        let b = s.frame.bgra();
        let rgb = [b[i + 2], b[i + 1], b[i]];
        let gpu = loupe::layout(self.cursor, (fw as f32, fh as f32), s.scale);
        let screen = (s.frame.x + px, s.frame.y + py);
        let info = self.text.as_deref().and_then(|t| {
            self.loupe_info.render(t, self.ui_bold.as_deref(), &gpu, screen, rgb, s.hex_mode, s.color_copied, s.dark, s.scale)
        });
        g.set_loupe(Some(gpu), info);
    }

    fn copy_color(&mut self) {
        let Some(s) = self.session.as_mut() else { return };
        let (fw, fh) = (s.frame.w as i32, s.frame.h as i32);
        let (px, py) = ((self.cursor.0 as i32).clamp(0, fw - 1), (self.cursor.1 as i32).clamp(0, fh - 1));
        let i = ((py * fw + px) * 4) as usize;
        let b = s.frame.bgra();
        let text = loupe::color_text([b[i + 2], b[i + 1], b[i]], s.hex_mode);
        match arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
            Ok(()) => s.color_copied = true,
            Err(e) => {
                log(&format!("copy color: {e}"));
                beep();
            }
        }
        self.request_redraw();
    }

    fn redraw(&mut self) {
        self.flush_ui();
        self.update_loupe();
        if let (Some(s), Some(g)) = (self.session.as_ref(), self.gfx.as_mut()) {
            g.set_handles(if matches!(s.sel, Sel::Done(_)) { 4.5 * s.scale } else { 0.0 });
            g.set_light(!s.dark);
        }
        let rect = self.current_rect();
        let retry = self.gfx.as_mut().is_some_and(|g| g.draw(rect));
        if self.gfx.as_ref().is_some_and(|g| g.is_lost()) {
            self.gfx = None;
            self.end_capture();
            beep();
            return;
        }
        if retry && self.redraw_retries < 5 {
            self.redraw_retries += 1;
            self.request_redraw();
        } else {
            self.redraw_retries = 0;
            if self.gfx.as_ref().is_some_and(|g| g.ui_animating()) {
                self.request_redraw();
            }
        }
    }


    fn on_move(&mut self) {
        let cursor = self.cursor;
        let shift = self.mods.shift_key();
        let ctrl = self.mods.control_key();
        let Some(s) = self.session.as_mut() else { return };
        if let Some(idx) = s.slider_drag {
            s.apply_slider(idx, cursor.0);
            self.rerender_ui();
            self.resize_live_text();
            self.restyle_selected();
            return;
        }
        if s.edit.is_some() {
            self.edit_move(shift);
            return;
        }
        if let Some(adj) = s.adjust {
            let r = adjusted(adj, cursor, s.frame.w as f32, s.frame.h as f32);
            if r != adj.start && s.tb.is_some() {
                s.tb = None;
                if let Some(g) = self.gfx.as_mut() {
                    g.hide_ui();
                }
            }
            s.sel = Sel::Done(r);
            self.request_redraw();
            return;
        }
        match s.sel {
            Sel::Dragging { from, .. } => {
                s.sel = Sel::Dragging { from, to: cursor };
                s.color_copied = false;
                self.request_redraw();
            }
            Sel::Done(_) if s.dragging => {
                let b = s.layer_reach().unwrap_or([0.0, 0.0, s.frame.w as f32, s.frame.h as f32]);
                let p = (cursor.0.clamp(b[0], b[2]), cursor.1.clamp(b[1], b[3]));
                if s.tool == Some(Tool::Eraser) {
                    let tol = (3.0 + 1.2 * s.styles[Tool::Eraser.index()].width as f32) * s.scale;
                    self.layer_op(LayerOp::Erase(p, tol));
                    self.rerender_ui();
                    return;
                }
                if let Some(a) = s.layer.as_mut().and_then(|l| l.live.as_mut()) {
                    a.set_end(p, shift);
                }
                self.layer_op(LayerOp::Refresh);
            }
            Sel::Done(r) => {
                let Some(t) = s.tb.as_mut() else { return };
                let (lx, ly) = (cursor.0 - t.origin.0, cursor.1 - t.origin.1);
                let hover = t.tb.hit(lx, ly);
                let over = t.tb.in_pill(lx, ly);
                let changed = hover != t.hover;
                t.hover = hover;
                let icon = if over {
                    CursorIcon::Default
                } else {
                    let grip = hit_grip(r, cursor, 6.0 * s.scale, s.tool.is_none());
                    let (handle, body) = if ctrl { (None, false) } else { s.pick(s.clamp_to_layer(cursor), self.text.as_deref(), self.shaper.as_deref()) };
                    if let Some(h) = handle {
                        handle_icon(s.layer.as_ref().and_then(|l| l.selected()), h)
                    } else if let Some(g) = grip.filter(|g| !g.is_move()) {
                        g.icon()
                    } else if body {
                        CursorIcon::Move
                    } else {
                        grip.map_or(CursorIcon::Crosshair, Grip::icon)
                    }
                };
                if icon != s.cur_icon {
                    s.cur_icon = icon;
                    if let Some(w) = &self.window {
                        w.set_cursor(icon);
                    }
                }
                if changed {
                    self.rerender_ui();
                }
            }
            Sel::None => {
                s.hover = snap::pick(&s.windows, cursor);
                s.color_copied = false;
                self.request_redraw();
            }
        }
    }

    fn on_press(&mut self) {
        enum Action {
            Item(usize, Item),
            Swallow,
            Annot,
            Edit(Option<usize>),
            Copy,
            NewSel,
            Grab(Grip),
        }
        let cursor = self.cursor;
        let ctrl = self.mods.control_key();
        let (text, shaper) = (self.text.as_deref(), self.shaper.as_deref());
        let action = {
            let Some(s) = self.session.as_mut() else { return };
            let now = Instant::now();
            let window = Duration::from_millis(unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() } as u64);
            let near = (cursor.0 - s.last_press_pos.0).abs() <= 6.0 && (cursor.1 - s.last_press_pos.1).abs() <= 6.0;
            let dbl = near && s.last_press.is_some_and(|t| now - t < window);
            s.last_press = Some(now);
            s.last_press_pos = cursor;
            match s.sel {
                Sel::Done(r) => {
                    let on_pill = s.tb.as_ref().and_then(|t| {
                        let (lx, ly) = (cursor.0 - t.origin.0, cursor.1 - t.origin.1);
                        t.tb.in_pill(lx, ly).then(|| t.tb.hit(lx, ly).map(|i| (i, t.tb.items[i].0)))
                    });
                    match on_pill {
                        Some(Some((i, item))) => Action::Item(i, item),
                        Some(None) => Action::Swallow,
                        None => {
                            let grip = hit_grip(r, cursor, 6.0 * s.scale, s.tool.is_none());
                            let (handle, body) = if ctrl { (None, false) } else { s.pick(s.clamp_to_layer(cursor), text, shaper) };
                            if handle.is_some() {
                                Action::Edit(handle)
                            } else if let Some(g) = grip.filter(|g| !g.is_move()) {
                                Action::Grab(g)
                            } else if body {
                                Action::Edit(None)
                            } else {
                                match grip {
                                    Some(g) if g.is_move() && dbl => Action::Copy,
                                    Some(g) => Action::Grab(g),
                                    None if s.tool.is_some() => Action::Annot,
                                    None => Action::NewSel,
                                }
                            }
                        }
                    }
                }
                _ => Action::NewSel,
            }
        };
        match action {
            Action::Item(i, Item::Slider | Item::Strength) => {
                if let Some(s) = self.session.as_mut() {
                    s.slider_drag = Some(i);
                    s.apply_slider(i, cursor.0);
                }
                self.rerender_ui();
                self.resize_live_text();
                self.restyle_selected();
            }
            Action::Item(_, item) => self.on_item(item),
            Action::Swallow => {}
            Action::Edit(handle) => {
                self.finish_text();
                let Some(p) = self.frame_cursor() else { return };
                if handle.is_none() {
                    let reach = PICK_REACH * self.session.as_ref().map_or(1.0, |s| s.scale);
                    self.layer_op(LayerOp::SelectAt(p, reach));
                }
                let orig = self.session.as_ref().and_then(|s| s.layer.as_ref()).and_then(|l| l.selected().cloned());
                let Some(orig) = orig else { return };
                let grab = handle
                    .and_then(|h| self.layer_query(|_, cx| orig.handles(cx).get(h).map(|c| (c.0 - p.0, c.1 - p.1))))
                    .flatten()
                    .unwrap_or((0.0, 0.0));
                let Some(s) = self.session.as_mut() else { return };
                s.edit = Some(EditDrag { handle, start: p, orig, grab, lifted: false });
                self.sync_tool_to_selection();
            }
            Action::Annot => {
                if self.session.as_ref().is_some_and(|s| s.editing && s.tool != Some(Tool::Text)) {
                    self.finish_text();
                }
                self.start_annot();
            }
            Action::Copy => self.copy_selection(),
            Action::Grab(grip) => {
                self.finish_text();
                self.layer_op(LayerOp::Deselect);
                if let Some(s) = self.session.as_mut()
                    && let Sel::Done(r) = s.sel
                {
                    s.adjust = Some(Adjust { grip, start: r, press: cursor });
                }
                self.request_redraw();
            }
            Action::NewSel => {
                self.finish_text();
                self.layer_op(LayerOp::Deselect);
                self.clear_selection();
                if let Some(s) = self.session.as_mut() {
                    s.press_hover = s.hover;
                    s.sel = Sel::Dragging { from: cursor, to: cursor };
                }
            }
        }
    }

    fn on_release(&mut self) {
        if self.session.as_mut().is_some_and(|s| s.slider_drag.take().is_some()) {
            return;
        }
        enum Kind {
            Sel,
            Annot,
            Edit,
            Adjust,
            Nothing,
        }
        let kind = match self.session.as_ref() {
            Some(s) if s.adjust.is_some() => Kind::Adjust,
            Some(s) if s.edit.is_some() => Kind::Edit,
            Some(s) if matches!(s.sel, Sel::Dragging { .. }) => Kind::Sel,
            Some(s) if s.dragging => Kind::Annot,
            _ => Kind::Nothing,
        };
        if let Some(s) = self.session.as_mut()
            && let Sel::Dragging { from, to } = s.sel
            && is_click(from, to)
            && let Some(r) = s.press_hover.take()
        {
            s.sel = Sel::Dragging { from: (r[0], r[1]), to: (r[2], r[3]) };
        }
        match kind {
            Kind::Sel => self.finalize_selection(),
            Kind::Annot => self.finish_annot(),
            Kind::Edit => self.finish_edit(),
            Kind::Adjust => self.finish_adjust(),
            Kind::Nothing => {}
        }
    }

    fn on_key(&mut self, ev: &KeyEvent) {
        if !matches!(&ev.logical_key, Key::Named(NamedKey::Shift | NamedKey::Control | NamedKey::Alt | NamedKey::AltGraph | NamedKey::Super | NamedKey::Meta)) {
            self.finish_edit();
        }
        let editing = self.session.as_ref().is_some_and(|s| s.editing);
        let ctrl = self.mods.control_key();
        if editing && !ctrl {
            match &ev.logical_key {
                Key::Named(NamedKey::Escape) => self.cancel_text(),
                Key::Named(NamedKey::Enter) => self.finish_text(),
                Key::Named(NamedKey::Backspace) => self.backspace_text(),
                _ => {}
            }
            return;
        }
        let choosing = self.session.as_ref().is_some_and(|s| matches!(s.sel, Sel::None | Sel::Dragging { .. }));
        if choosing && !ctrl {
            match &ev.logical_key {
                Key::Named(NamedKey::Shift) if !ev.repeat => {
                    if let Some(s) = self.session.as_mut() {
                        s.hex_mode = !s.hex_mode;
                    }
                    self.request_redraw();
                    return;
                }
                _ if shortcut_letter(&ev.logical_key, &ev.physical_key) == Some('c') => {
                    self.copy_color();
                    return;
                }
                _ => {}
            }
        }
        match &ev.logical_key {
            Key::Named(NamedKey::Escape) => self.end_capture(),
            Key::Named(NamedKey::Enter) => self.copy_selection(),
            Key::Named(NamedKey::Delete | NamedKey::Backspace) => {
                self.layer_op(LayerOp::Delete);
                self.rerender_ui();
            }
            _ if ctrl => match shortcut_letter(&ev.logical_key, &ev.physical_key) {
                Some('c') => self.copy_selection(),
                Some('s') => self.save_selection(),
                Some('z') => {
                    if self.mods.shift_key() {
                        self.on_item(Item::Redo);
                    } else {
                        self.on_item(Item::Undo);
                    }
                }
                Some('y') => self.on_item(Item::Redo),
                Some('t') => self.on_item(Item::Pin),
                _ => {}
            },
            _ => {}
        }
    }
}

static TRAY_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

const TRAY_CHECKS: [Duration; 4] = [Duration::from_secs(2), Duration::from_secs(5), Duration::from_secs(15), Duration::from_secs(60)];

impl App {
    fn check_tray(&mut self) -> Option<Instant> {
        use windows::{
            Win32::{
                Foundation::{HWND, LPARAM, WPARAM},
                UI::WindowsAndMessaging::{PostMessageW, RegisterWindowMessageW},
            },
            core::w,
        };
        let (at, n) = self.tray_check?;
        if Instant::now() < at {
            return Some(at);
        }
        self.tray_check = None;
        let tray = self._tray.as_ref().filter(|t| t.rect().is_none())?;
        unsafe {
            let msg = RegisterWindowMessageW(w!("TaskbarCreated"));
            let _ = PostMessageW(Some(HWND(tray.window_handle())), msg, WPARAM(0), LPARAM(0));
        }
        let next = (Instant::now() + *TRAY_CHECKS.get(n + 1)?, n + 1);
        self.tray_check = Some(next);
        Some(next.0)
    }
}

fn record_failed(why: &str, hold: Option<SaveDone>) {
    beep();
    if let Some(text) = record_failure_text(why) {
        std::thread::spawn(move || {
            convert::message_box(&text);
            drop(hold);
        });
    }
}

fn record_failure_text(why: &str) -> Option<String> {
    if why.contains("Media Foundation is not available") {
        return Some(format!("Snptu can't record MP4 video.\n\n{}\n\nChoose GIF or WebP in the recording bar instead.", why.trim_start_matches("MP4: ")));
    }
    if why == record::NOTHING_RECORDED {
        return None;
    }
    Some(format!("Snptu couldn't record the screen, and nothing was saved.\n\n{}", sentence(why)))
}

fn too_small_text(why: &str) -> String {
    format!("This region is too small to record as MP4.\n\n{}", sentence(why))
}

struct OneNotice(&'static std::sync::atomic::AtomicBool);

impl OneNotice {
    fn take(up: &'static std::sync::atomic::AtomicBool) -> Option<OneNotice> {
        (!up.swap(true, std::sync::atomic::Ordering::AcqRel)).then(|| OneNotice(up))
    }
}

impl Drop for OneNotice {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

fn sentence(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

fn size_limit_text(ext: &str) -> String {
    let format = if ext == "webp" { "WebP" } else { "GIF" };
    format!(
        "The recording stopped because the {format} file reached the size limit of {} GB.\n\nEverything recorded until then is kept. For longer recordings, choose MP4, or a smaller region or scale.",
        recfmt::BUDGET_BYTES >> 30
    )
}

struct Unsaved {
    file: Mutex<Option<PathBuf>>,
    stem: String,
    save_dir: PathBuf,
    dialog: u64,
}

static UNSAVED: Mutex<Vec<Weak<Unsaved>>> = Mutex::new(Vec::new());

static RECORDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Unsaved {
    fn new(tmp: PathBuf, stem: String, save_dir: PathBuf) -> Arc<Unsaved> {
        let u = Arc::new(Unsaved { file: Mutex::new(Some(tmp)), stem, save_dir, dialog: dialogs::new_token() });
        let mut all = locked(&UNSAVED);
        all.retain(|w| w.strong_count() > 0);
        all.push(Arc::downgrade(&u));
        u
    }

    fn waiting(&self) -> bool {
        match self.file.try_lock() {
            Ok(f) => f.is_some(),
            Err(std::sync::TryLockError::WouldBlock) => true,
            Err(std::sync::TryLockError::Poisoned(f)) => f.into_inner().is_some(),
        }
    }

    fn save_now(&self) {
        let mut file = match self.file.try_lock() {
            Ok(f) => f,
            Err(std::sync::TryLockError::WouldBlock) => return,
            Err(std::sync::TryLockError::Poisoned(f)) => f.into_inner(),
        };
        let Some(tmp) = file.take() else { return };
        dialogs::close_dialog(self.dialog);
        let ext = record::ext_of(&tmp);
        let dest = record::claim_name(&self.save_dir, &self.stem, ext).map_err(|e| e.to_string());
        let _ = store_recording(&tmp, dest, &self.stem, ext, true);
    }
}

fn unsaved() -> Vec<Arc<Unsaved>> {
    locked(&UNSAVED).iter().filter_map(Weak::upgrade).collect()
}

fn recordings_unsaved() -> bool {
    RECORDING.load(std::sync::atomic::Ordering::Acquire) || unsaved().iter().any(|u| u.waiting())
}

fn save_waiting() {
    for u in unsaved() {
        u.save_now();
    }
}

fn store_recording(tmp: &Path, dest: Result<PathBuf, String>, stem: &str, ext: &str, quiet: bool) -> Option<String> {
    let mut rec = tmp.to_path_buf();
    let moved = dest.and_then(|p| record::move_file(&mut rec, &p).map_err(|e| format!("{}: {e}", p.display())));
    let Err(e) = moved else { return None };
    log(&format!("save recording: {e}"));
    if !quiet && !session::ending() {
        beep();
    }
    Some(match record::claim_name(&config::default_save_dir(), stem, ext).and_then(|p| record::move_file(&mut rec, &p).map(|()| p)) {
        Ok(p) => {
            log(&format!("recording saved to {} instead", p.display()));
            format!("Snptu couldn't save the recording where you asked.\n\nIt was saved here instead:\n\n{}", p.display())
        }
        Err(e) => {
            let (kept, note) = match record::keep(&rec) {
                Ok(kept) => (kept, ""),
                Err(_) => (rec, "\n\nCopy it soon: Snptu clears such files out."),
            };
            log(&format!("recording kept at {}: {e}", kept.display()));
            format!("Snptu couldn't save the recording.\n\nIt is at:\n\n{}{note}", kept.display())
        }
    })
}

fn shortcut_letter(key: &Key, physical: &PhysicalKey) -> Option<char> {
    if let Key::Character(c) = key {
        let mut chars = c.chars();
        if let (Some(l), None) = (chars.next(), chars.next()) {
            if l.is_ascii_alphabetic() {
                return Some(l.to_ascii_lowercase());
            }
            if l.is_ascii_graphic() {
                return None;
            }
        }
    }
    match physical {
        PhysicalKey::Code(KeyCode::KeyC) => Some('c'),
        PhysicalKey::Code(KeyCode::KeyS) => Some('s'),
        PhysicalKey::Code(KeyCode::KeyT) => Some('t'),
        PhysicalKey::Code(KeyCode::KeyY) => Some('y'),
        PhysicalKey::Code(KeyCode::KeyZ) => Some('z'),
        _ => None,
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = app_window_attributes()
            .with_title("Snptu")
            .with_decorations(false)
            .with_resizable(false)
            .with_visible(false)
            .with_skip_taskbar(true)
            .with_window_level(WindowLevel::AlwaysOnTop);
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log(&format!("window: {e}"));
                return el.exit();
            }
        };
        set_window_class_icon(&window);
        disable_window_transitions(&window);
        self.window = Some(window);

        let failed = match GlobalHotKeyManager::new() {
            Ok(m) => {
                self.hotkey_mgr = Some(m);
                let proxy = self.proxy.clone();
                GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
                    if e.state == HotKeyState::Pressed {
                        let _ = proxy.send_event(UserEvent::Hotkey(e.id));
                    }
                }));
                self.register_hotkeys()
            }
            Err(e) => {
                log(&format!("hotkey manager: {e}"));
                Vec::new()
            }
        };

        for (id, action) in [
            ("tray.capture", MenuAction::Capture),
            ("tray.record", MenuAction::Record),
            ("tray.settings", MenuAction::Settings),
            ("tray.convert", MenuAction::Convert),
            ("tray.hide_pins", MenuAction::ToggleHidePins),
            ("tray.destroy_pins", MenuAction::DestroyAllPins),
            ("tray.quit", MenuAction::Quit),
        ] {
            self.menu_actions.insert(MenuId::new(id), action);
        }
        let proxy = self.proxy.clone();
        let double_click = std::sync::atomic::AtomicBool::new(false);
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            use std::sync::atomic::Ordering::Relaxed;
            match e {
                TrayIconEvent::Click { button, button_state: MouseButtonState::Up, position, .. } => {
                    if matches!(button, TrayButton::Left) && double_click.swap(false, Relaxed) {
                        return;
                    }
                    let _ = match button {
                        TrayButton::Left => proxy.send_event(UserEvent::Capture),
                        TrayButton::Right => proxy.send_event(UserEvent::TrayMenu(position.x as i32, position.y as i32)),
                        _ => Ok(()),
                    };
                }
                TrayIconEvent::DoubleClick { button: TrayButton::Left, .. } => double_click.store(true, Relaxed),
                TrayIconEvent::Move { .. } => {
                    use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::KillTimer};
                    let hwnd = TRAY_HWND.load(Relaxed);
                    if hwnd != 0 {
                        let _ = unsafe { KillTimer(Some(HWND(hwnd as *mut std::ffi::c_void)), 6007) };
                    }
                }
                _ => {}
            }
        }));
        match TrayIconBuilder::new()
            .with_tooltip("Snptu")
            .with_icon(tray_icon_image())
            .build()
        {
            Ok(t) => {
                TRAY_HWND.store(t.window_handle() as isize, std::sync::atomic::Ordering::Relaxed);
                self.tray_check = Some((Instant::now() + TRAY_CHECKS[0], 0));
                self._tray = Some(t);
            }
            Err(e) => log(&format!("tray: {e}")),
        }

        self.text = Text::load();
        self.ui_bold = Text::load_semibold();
        self.warn_hotkey_conflicts(el, &failed);

        self.convert_from_args(el);

        if let Err(e) = shell_menu::sync(self.config.shell_menu) {
            log(&format!("explorer menu: {e:#}"));
        }
        mem::trim();
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        RECORDING.store(self.recorder.is_some() || !self.finishing.is_empty(), std::sync::atomic::Ordering::Release);
        self.flush_pins(el);
        if std::mem::take(&mut self.release_after_pins) && self.session.is_none() {
            self.release_gpu();
        }
        let mut wake = None::<Instant>;
        if let Some(deadline) = self.gfx_deadline.filter(|_| self.session.is_none()) {
            if Instant::now() >= deadline {
                self.gfx = None;
                self.gfx_deadline = None;
                mem::trim();
                log("gpu released after idle");
            } else {
                wake = Some(deadline);
            }
        }
        if self.quitting && self.writes_pending() > 0 {
            self.quit_if_waiting(el);
            if let Some(at) = self.quit_by.filter(|at| *at > Instant::now()) {
                wake = Some(wake.map_or(at, |w| w.min(at)));
            }
        }
        let due = self.settings.as_mut().map(|s| [s.message_fades_at(), s.marquee_wake(), s.frame_wake()]).unwrap_or_default();
        let convert_due = self.convert.as_mut().and_then(|c| c.frame_wake());
        for at in due.into_iter().flatten().chain(convert_due).chain(self.check_tray()) {
            wake = Some(wake.map_or(at, |w| w.min(at)));
        }
        el.set_control_flow(wake.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }

    fn user_event(&mut self, el: &ActiveEventLoop, ev: UserEvent) {
        let action = if let UserEvent::Menu(id) = &ev { self.menu_actions.get(id).copied() } else { None };
        if self.quitting && asks_for_work(&ev, action) {
            return;
        }
        match ev {
            UserEvent::Capture if self.recorder.is_some() || recbar::is_open() => self.toggle_record(),
            UserEvent::Capture => self.begin_capture(false),
            UserEvent::RecordDone(id, res) => {
                self.finish_recording(id, res);
                self.quit_if_waiting(el);
            }
            UserEvent::RecordSaved => {
                self.saving = self.saving.saturating_sub(1);
                self.quit_if_waiting(el);
            }
            UserEvent::SessionEnding => self.session_ending(),
            UserEvent::CloseApp if !self.quitting => self.quit(el),
            UserEvent::CloseApp => {}
            UserEvent::ShotWritten => {
                self.writing = self.writing.saturating_sub(1);
                self.quit_if_waiting(el);
            }
            UserEvent::RecBar(ev) => self.on_rec_bar(ev),
            UserEvent::Menu(id) => match self.menu_actions.get(&id).copied() {
                Some(MenuAction::Capture) => self.begin_capture(false),
                Some(MenuAction::Record) => self.toggle_record(),
                Some(MenuAction::Settings) => self.open_settings(el),
                Some(MenuAction::Convert) => self.open_convert(el, Vec::new()),
                Some(MenuAction::ConvertPick(pick)) => self.convert_pick(pick),
                Some(MenuAction::Pin(window, PinCmd::Convert)) => self.pin_convert(el, window),
                Some(MenuAction::ToggleHidePins) => self.toggle_pins_visible(),
                Some(MenuAction::DestroyAllPins) => self.destroy_all_pins(),
                Some(MenuAction::Quit) => self.quit(el),
                Some(MenuAction::Pin(window, cmd)) => self.pin_command(window, cmd),
                None => {}
            },
            UserEvent::Hotkey(id) => {
                let action = Action::ALL.iter().copied().find(|a| self.hotkeys[a.index()].is_some_and(|h| h.id() == id));
                match action {
                    Some(Action::Snip) => self.begin_capture(false),
                    Some(Action::SnipCopy) => self.begin_capture(true),
                    Some(Action::Paste) => self.paste_pin(),
                    Some(Action::TogglePins) => self.toggle_pins_visible(),
                    Some(Action::ClickThrough) => self.toggle_pins_click_through(),
                    Some(Action::SnipNested) => self.begin_nested_capture(),
                    Some(Action::Record) => self.toggle_record(),
                    None => {}
                }
            }
            UserEvent::ConvertRequest => self.convert_request(el),
            UserEvent::ConvertProgress => {
                if let Some(w) = &self.convert {
                    w.window.request_redraw();
                }
                self.quit_if_waiting(el);
            }
            UserEvent::TrayMenu(x, y) => self.tray_menu(el, (x, y)),
            UserEvent::Char(c) => self.on_char(c),
            UserEvent::PickColor(t, c, custom, done) => self.on_pick_color(t, c, custom, done),
            UserEvent::GpuWarmed => self.adopt_warm_gpu(),
            UserEvent::PinFile(window, res) => self.pin_file_done(window, res),
            UserEvent::PastePin(image, at) => self.pasted_pin(image, at),
            UserEvent::SettingsFolder(window, hit, res) => self.settings_folder_chosen(window, hit, res),
            UserEvent::ConvertFolder(window, res) => self.convert_folder_chosen(window, res),
            UserEvent::ConvertFiles(window, res) => self.convert_files_chosen(window, res),
            UserEvent::ConvertEstimate(round, res) => self.convert_estimate_done(round, res),
            UserEvent::SaveChosen(id, res) => {
                let Some(p) = self.pending_save.take_if(|p| p.id == id) else { return };
                match res {
                    Ok(Some(mut path)) => {
                        ensure_extension(&mut path, p.params.kind.ext());
                        self.end_capture();
                        self.write_shot(SaveTarget::File(path), p.params, p.w, p.h, p.px);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        log(&format!("save dialog: {e}"));
                        beep();
                    }
                }
            }
        }
    }

    fn window_event(&mut self, _el: &ActiveEventLoop, id: WindowId, ev: WindowEvent) {
        if self.settings.as_ref().is_some_and(|s| s.id() == id) {
            self.settings_event(ev);
            return recbar::set_settings(self.config.rec);
        }
        if self.convert.as_ref().is_some_and(|c| c.id() == id) {
            return self.convert_event(ev);
        }
        if let Some(i) = self.pins.iter().position(|p| p.id() == id) {
            return self.pin_event(i, ev);
        }
        match ev {
            WindowEvent::CloseRequested => self.end_capture(),
            WindowEvent::Resized(size) => {
                if self.session.is_some() {
                    if let Some(g) = &mut self.gfx {
                        g.resize(size.width, size.height);
                    }
                    self.enforce_overlay_geometry();
                }
            }
            WindowEvent::Moved(_) | WindowEvent::ScaleFactorChanged { .. } => {
                if self.session.is_some() {
                    self.enforce_overlay_geometry();
                }
            }
            WindowEvent::RedrawRequested => {
                if self.session.is_some() {
                    self.redraw();
                }
            }
            WindowEvent::Focused(false) => {
                if self.session.is_some() && !foreground_is_ours() {
                    log("overlay lost focus to another application; cancelling the capture");
                    self.end_capture();
                }
            }
            WindowEvent::ModifiersChanged(m) => {
                self.mods = m.state();
                if self.session.as_ref().is_some_and(|s| s.dragging) {
                    self.on_move();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x as f32, position.y as f32);
                self.on_move();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                if let Some(s) = self.session.as_mut()
                    && let (Some(t), true) = (s.tool, dy != 0.0 && s.edit.is_none())
                {
                    let st = &mut s.styles[t.index()];
                    if st.uses_size(t) {
                        st.width = (st.width as i32 + dy.signum() as i32).clamp(1, 12) as u32;
                        self.rerender_ui();
                        self.resize_live_text();
                        self.restyle_selected();
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => match (button, state) {
                (MouseButton::Left, ElementState::Pressed) => self.on_press(),
                (MouseButton::Left, ElementState::Released) => self.on_release(),
                (MouseButton::Right, ElementState::Pressed) => {
                    let has_sel = self
                        .session
                        .as_ref()
                        .is_some_and(|s| matches!(s.sel, Sel::Done(_) | Sel::Dragging { .. }));
                    if has_sel {
                        self.clear_selection();
                    } else {
                        self.end_capture();
                    }
                }
                _ => {}
            },
            WindowEvent::Ime(Ime::Commit(s)) => self.type_text(&s),
            WindowEvent::Ime(Ime::Preedit(s, _)) => self.preedit_text(s),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                self.on_key(&event);
            }
            _ => {}
        }
    }
}

pub(crate) enum SaveTarget {
    Folder { dir: PathBuf, stem: String },
    File(PathBuf),
}

fn write_new(dir: &Path, stem: &str, ext: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind};
    if !dir.is_absolute() {
        return Err(Error::new(ErrorKind::InvalidInput, "save folder must be an absolute path"));
    }
    std::fs::create_dir_all(dir)?;
    let part = convert::write_part(&dir.join(format!("{stem}.{ext}")), bytes, None)?;
    for n in 0..10_000u32 {
        let name = if n == 0 { format!("{stem}.{ext}") } else { format!("{stem}_{n}.{ext}") };
        let path = dir.join(name);
        match convert::rename_new(&part, &path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                return Err(e);
            }
        }
    }
    let _ = std::fs::remove_file(&part);
    Err(Error::other("no free file name"))
}

fn disable_window_transitions(window: &Window) {
    use windows::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute};
    if let Some(hwnd) = hwnd_of(window) {
        let on: i32 = 1;
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_TRANSITIONS_FORCEDISABLED,
                &on as *const i32 as *const std::ffi::c_void,
                size_of::<i32>() as u32,
            );
        }
    }
}

fn place_overlay(window: &Window, frame: &capture::Frame) {
    use windows::Win32::UI::WindowsAndMessaging::{HWND_TOPMOST, SWP_NOACTIVATE, SetWindowPos};
    if let Some(hwnd) = hwnd_of(window) {
        unsafe {
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), frame.x, frame.y, frame.w as i32, frame.h as i32, SWP_NOACTIVATE);
        }
    } else {
        window.set_outer_position(PhysicalPosition::new(frame.x, frame.y));
        let _ = window.request_inner_size(PhysicalSize::new(frame.w, frame.h));
    }
}

fn hwnd_of(window: &Window) -> Option<windows::Win32::Foundation::HWND> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(windows::Win32::Foundation::HWND(h.hwnd.get() as *mut std::ffi::c_void)),
        _ => None,
    }
}

fn app_window_attributes() -> winit::window::WindowAttributes {
    let icon = winit::window::Icon::from_rgba(icon::render_rgba(64), 64, 64).expect("valid icon");
    Window::default_attributes().with_window_icon(Some(icon.clone())).with_taskbar_icon(Some(icon))
}

fn set_window_class_icon(window: &Window) {
    use windows::{
        Win32::{
            Foundation::HINSTANCE,
            System::LibraryLoader::GetModuleHandleW,
            UI::WindowsAndMessaging::{GCLP_HICON, GCLP_HICONSM, LoadIconW, SetClassLongPtrW},
        },
        core::PCWSTR,
    };
    let Some(hwnd) = hwnd_of(window) else { return };
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        let Ok(icon) = LoadIconW(Some(HINSTANCE(module.0)), PCWSTR(std::ptr::without_provenance(1))) else { return };
        SetClassLongPtrW(hwnd, GCLP_HICON, icon.0 as isize);
        SetClassLongPtrW(hwnd, GCLP_HICONSM, icon.0 as isize);
    }
}

fn init_taskbar() {
    if !shell_pkg::has_identity() {
        unsafe {
            let _ = windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(windows::core::w!("Snptu.Desktop"));
        }
    }
}

fn tray_icon_image() -> Icon {
    Icon::from_rgba(icon::render_rgba(32), 32, 32).expect("valid icon")
}

fn log(msg: &str) {
    #[cfg(debug_assertions)]
    {
        use windows::{Win32::System::Diagnostics::Debug::OutputDebugStringW, core::PCWSTR};
        let wide = wide(&format!("Snptu: {msg}
"));
        unsafe { OutputDebugStringW(PCWSTR(wide.as_ptr())) };
    }
    #[cfg(not(debug_assertions))]
    let _ = msg;
}

pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn beep() {
    use windows::Win32::{
        Foundation::POINT,
        System::Diagnostics::Debug::MessageBeep,
        UI::WindowsAndMessaging::{GetCursorPos, MB_ICONHAND},
    };
    let mut at = POINT::default();
    if unsafe { GetCursorPos(&mut at) }.is_err() {
        return;
    }
    unsafe {
        let _ = MessageBeep(MB_ICONHAND);
    }
}

fn initial_styles(cfg: &config::Config) -> [Style; 9] {
    let mut styles = [Style::default(); 9];
    if cfg.remember_colors {
        for (st, v) in styles.iter_mut().zip(&cfg.colors) {
            if let Some((color, custom)) = config::parse_color(v) {
                st.color = color;
                st.custom = custom;
            }
        }
    }
    styles
}

pub(crate) fn is_foreground(window: &Window) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    hwnd_of(window).is_some_and(|h| unsafe { GetForegroundWindow() } == h)
}

fn foreground_is_ours() -> bool {
    use windows::Win32::{
        System::Threading::GetCurrentProcessId,
        UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
    };
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        pid == GetCurrentProcessId()
    }
}

fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    use image::{
        ExtendedColorType, ImageEncoder,
        codecs::png::{CompressionType, FilterType, PngEncoder},
    };
    let mut rgb = Vec::with_capacity(rgba.len() / 4 * 3);
    for p in rgba.as_chunks::<4>().0 {
        rgb.extend_from_slice(&p[..3]);
    }
    let mut out = Vec::with_capacity(rgb.len() / 2);
    PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Sub)
        .write_image(&rgb, w, h, ExtendedColorType::Rgb8)?;
    Ok(out)
}

fn ensure_extension(path: &mut std::path::PathBuf, ext: &str) {
    let wanted = convert::Kind::from_extension(ext);
    let right = |e: &std::ffi::OsStr| {
        e.eq_ignore_ascii_case(ext) || (wanted.is_some() && e.to_str().and_then(convert::Kind::from_extension) == wanted)
    };
    if !path.extension().is_some_and(right) {
        *path = convert::with_added_extension(path, ext);
    }
}

fn write_in_background(mut target: SaveTarget, params: convert::Params, w: u32, h: u32, rgba: Vec<u8>, done: ShotDone) {
    std::thread::spawn(move || {
        par::lower_priority();
        let mut ext = params.kind.ext();
        let bytes = match convert::encode(&params, w, h, &rgba) {
            Ok(b) => b,
            Err(e) => {
                log(&format!("{} encode: {e:#}", ext));
                beep();
                match encode_png(w, h, &rgba) {
                    Ok(b) => {
                        if let SaveTarget::File(p) = &mut target {
                            *p = convert::with_added_extension(&p.with_extension(""), "png");
                        }
                        ext = "png";
                        b
                    }
                    Err(e) => return log(&format!("png encode: {e}")),
                }
            }
        };
        drop(rgba);
        let (stem, wanted, result) = match &target {
            SaveTarget::Folder { dir, stem } => (stem.clone(), dir.display().to_string(), write_new(dir, stem, ext, &bytes)),
            SaveTarget::File(path) => {
                let res = convert::write_atomic(path, &bytes).map(|()| path.clone());
                let stem = path.file_stem().map_or_else(|| "Snptu".to_string(), |s| s.to_string_lossy().into_owned());
                (stem, path.display().to_string(), res)
            }
        };
        let Err(e) = result else { return };
        log(&format!("save to {wanted}: {e}"));
        beep();
        let fallback = config::default_save_dir();
        let saved = if matches!(&target, SaveTarget::Folder { dir, .. } if *dir == fallback) {
            None
        } else {
            match write_new(&fallback, &stem, ext, &bytes) {
                Ok(p) => {
                    log(&format!("saved to {} instead", p.display()));
                    Some(p)
                }
                Err(e) => {
                    log(&format!("fallback save to {} failed too: {e}", fallback.display()));
                    None
                }
            }
        };
        drop(done);
        convert::message_box(&save_failure_text(&wanted, &e, saved.as_deref()));
    });
}

fn asks_for_work(ev: &UserEvent, action: Option<MenuAction>) -> bool {
    match ev {
        UserEvent::Menu(_) => !matches!(action, Some(MenuAction::Quit)),
        UserEvent::Capture | UserEvent::Hotkey(_) | UserEvent::TrayMenu(..) | UserEvent::ConvertRequest => true,
        UserEvent::RecBar(e) => matches!(e, recbar::Ev::Begin | recbar::Ev::Reselect),
        _ => false,
    }
}

fn session_end_event(closing_app: bool) -> UserEvent {
    if closing_app { UserEvent::CloseApp } else { UserEvent::SessionEnding }
}

fn quit_ready(recordings_done: bool, writing: u32, give_up: Option<Instant>, now: Instant) -> bool {
    recordings_done && (writing == 0 || give_up.is_some_and(|at| now >= at))
}

fn save_failure_text(wanted: &str, error: &std::io::Error, saved_to: Option<&Path>) -> String {
    let outcome = match saved_to {
        Some(p) => format!("It was saved here instead:\n\n{}", p.display()),
        None => "The screenshot was not saved.".to_string(),
    };
    format!("Snptu couldn't save the screenshot to {}.\n\n{error}\n\n{outcome}", short_path(wanted, 64))
}

fn short_path(path: &str, max: usize) -> String {
    if path.chars().count() <= max {
        return path.to_string();
    }
    let parts: Vec<&str> = path.split(['\\', '/']).filter(|p| !p.is_empty()).collect();
    let Some((head, rest)) = parts.split_first() else { return path.to_string() };
    let mut tail: Vec<&str> = Vec::new();
    let mut len = head.chars().count() + 3;
    for p in rest.iter().rev() {
        len += p.chars().count() + 1;
        if len > max && !tail.is_empty() {
            break;
        }
        tail.push(p);
    }
    tail.reverse();
    let lead = if path.starts_with("\\\\") { "\\\\" } else { "" };
    format!("{lead}{head}\\…\\{}", tail.join("\\"))
}

fn forward_to_running() {
    use windows::{
        Win32::{
            Foundation::CloseHandle,
            System::Threading::{EVENT_MODIFY_STATE, OpenEventW, SetEvent},
        },
        core::w,
    };
    if !forwards_from(&desktop_name()) {
        return;
    }
    let args: Vec<String> = args_lossy().into_iter().skip(1).collect();
    let first = args.first().map(String::as_str);
    let command = first.filter(|a| matches!(*a, "--settings" | "--record" | "--capture"));
    if command.is_none() && (first != Some("--convert-ui") || args.len() < 2) {
        return;
    }
    let absolute = |a: &String| std::path::absolute(a).map_or_else(|_| a.clone(), |p| p.to_string_lossy().into_owned());
    let body = command.map_or_else(|| args[1..].iter().map(absolute).collect::<Vec<_>>().join("\n"), str::to_string);
    if app_convert::queue_request(&app_convert::requests_dir(), &body).is_err() {
        return;
    }
    unsafe {
        if let Ok(ev) = OpenEventW(EVENT_MODIFY_STATE, false, w!("Local\\snptu-show-settings")) {
            let _ = SetEvent(ev);
            let _ = CloseHandle(ev);
        }
    }
}

fn forwards_from(desktop: &str) -> bool {
    desktop.is_empty() || desktop.eq_ignore_ascii_case("Default")
}

fn desktop_name() -> String {
    use std::ffi::c_void;
    #[link(name = "user32", kind = "raw-dylib")]
    unsafe extern "system" {
        fn GetThreadDesktop(thread: u32) -> *mut c_void;
        fn GetUserObjectInformationW(object: *mut c_void, index: i32, info: *mut c_void, len: u32, needed: *mut u32) -> i32;
    }
    const UOI_NAME: i32 = 2;
    let mut name = [0u16; 256];
    let mut needed = 0u32;
    unsafe {
        let desktop = GetThreadDesktop(windows::Win32::System::Threading::GetCurrentThreadId());
        if desktop.is_null() || GetUserObjectInformationW(desktop, UOI_NAME, name.as_mut_ptr().cast(), std::mem::size_of_val(&name) as u32, &mut needed) == 0 {
            return String::new();
        }
    }
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    String::from_utf16_lossy(&name[..len])
}

fn acquire_single_instance() -> Option<isize> {
    use windows::Win32::{
        Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError},
        System::Threading::{CreateEventW, CreateMutexW},
    };
    use windows::core::w;
    unsafe {
        let mut waits = 0;
        let mutex = loop {
            let Ok(mutex) = CreateMutexW(None, false, w!("Local\\snptu-single-instance")) else {
                log("single-instance mutex unavailable; running without the guard");
                return Some(0);
            };
            if GetLastError() != ERROR_ALREADY_EXISTS {
                break mutex;
            }
            let _ = CloseHandle(mutex);
            if waits < 50 && wait_for_quitting(QUITTING, QUIT_MARK_WAIT_MS) {
                waits += 1;
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            forward_to_running();
            return None;
        };
        let _ = mutex;
        let ev = match CreateEventW(None, false, false, w!("Local\\snptu-show-settings")) {
            Ok(h) if GetLastError() == ERROR_ALREADY_EXISTS => {
                let _ = CloseHandle(h);
                0
            }
            Ok(h) => h.0 as isize,
            Err(_) => 0,
        };
        Some(ev)
    }
}

const QUITTING: windows::core::PCWSTR = windows::core::w!("Local\\snptu-quitting");

const QUIT_MARK_WAIT_MS: u32 = 30_000;

fn mark_quitting(name: windows::core::PCWSTR) -> bool {
    use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
    unsafe {
        let Ok(mark) = windows::Win32::System::Threading::CreateMutexW(None, true, name) else { return false };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(mark);
            return false;
        }
    }
    true
}

fn wait_for_quitting(name: windows::core::PCWSTR, limit_ms: u32) -> bool {
    use windows::Win32::{
        Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0},
        System::Threading::{MUTEX_MODIFY_STATE, OpenMutexW, ReleaseMutex, SYNCHRONIZATION_SYNCHRONIZE, WaitForSingleObject},
    };
    unsafe {
        let Ok(mark) = OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE | MUTEX_MODIFY_STATE, false, name) else { return false };
        let woke = WaitForSingleObject(mark, limit_ms);
        let left = woke == WAIT_OBJECT_0 || woke == WAIT_ABANDONED;
        if left {
            let _ = ReleaseMutex(mark);
        }
        let _ = CloseHandle(mark);
        left
    }
}

fn watch_for_relaunch(event: isize, proxy: EventLoopProxy<UserEvent>) {
    use windows::Win32::{
        Foundation::{HANDLE, WAIT_OBJECT_0},
        System::Threading::{INFINITE, WaitForSingleObject},
    };
    if event == 0 {
        return;
    }
    std::thread::spawn(move || {
        let ev = HANDLE(event as *mut std::ffi::c_void);
        while unsafe { WaitForSingleObject(ev, INFINITE) } == WAIT_OBJECT_0 {
            if proxy.send_event(UserEvent::ConvertRequest).is_err() {
                break;
            }
        }
    });
}

pub(crate) fn args_lossy() -> Vec<String> {
    std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect()
}

fn harden_dll_search() {
    use windows::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories};
    unsafe {
        let _ = SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32);
    }
}

fn main() -> Result<()> {
    harden_dll_search();
    mem::init();
    std::panic::set_hook(Box::new(|info| log(&format!("panic: {info}"))));
    if let Some(code) = convert::run_decode_helper(&std::env::args_os().collect::<Vec<_>>()) {
        std::process::exit(code);
    }
    init_taskbar();
    let args = args_lossy();
    if shell_pkg::is_server_launch(&args) {
        shell_pkg::run_server();
    }
    if let Some(code) = convert::run_cli(&args) {
        std::process::exit(code);
    }
    let Some(relaunch_event) = acquire_single_instance() else { return Ok(()) };
    std::thread::spawn(record::clean_stale);
    Text::preload();
    let proxy_slot: Rc<RefCell<Option<EventLoopProxy<UserEvent>>>> = Rc::default();
    let hook_slot = proxy_slot.clone();
    let mut high_surrogate = 0u16;
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    builder.with_msg_hook(move |msg| {
        use windows::Win32::UI::WindowsAndMessaging::{MSG, WM_CHAR};
        let msg = unsafe { &*(msg as *const MSG) };
        if msg.message == WM_CHAR {
            let unit = msg.wParam.0 as u16;
            let ch = match unit {
                0xD800..=0xDBFF => {
                    high_surrogate = unit;
                    None
                }
                0xDC00..=0xDFFF => {
                    let hi = std::mem::take(&mut high_surrogate);
                    char::decode_utf16([hi, unit]).next().and_then(|r| r.ok())
                }
                _ => {
                    high_surrogate = 0;
                    char::from_u32(unit as u32)
                }
            };
            if let (Some(c), Some(p)) = (ch, hook_slot.borrow().as_ref())
                && !c.is_control()
            {
                let _ = p.send_event(UserEvent::Char(c));
            }
        }
        false
    });
    let event_loop = builder.build()?;
    *proxy_slot.borrow_mut() = Some(event_loop.create_proxy());
    watch_for_relaunch(relaunch_event, event_loop.create_proxy());
    let ending = event_loop.create_proxy();
    let end = move |closing_app: bool| {
        let _ = ending.send_event(session_end_event(closing_app));
    };
    session::watch(recordings_unsaved, end, save_waiting);
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App {
        proxy: event_loop.create_proxy(),
        window: None,
        gfx: None,
        text: None,
        ui_bold: None,
        loupe_info: loupe::Info::new(),
        shaper: None,
        shaper_job: None,
        redraw_retries: 0,
        gfx_deadline: None,
        warm: None,
        suspended: Vec::new(),
        pending_pins: Vec::new(),
        release_after_pins: false,
        pins: Vec::new(),
        pins_click_through: false,
        menu_actions: HashMap::new(),
        config: Config::load(),
        autostart: config::autostart_enabled(),
        settings: None,
        convert: None,
        hotkeys: [None; config::ACTION_COUNT],
        recorder: None,
        pending_save: None,
        save_seq: 0,
        rec_area: [0; 4],
        rec_seq: 0,
        rec_id: 0,
        finishing: HashMap::new(),
        saving: 0,
        quitting: false,
        writing: 0,
        quit_by: None,
        converting: Vec::new(),
        hotkey_mgr: None,
        session: None,
        cursor: (0.0, 0.0),
        mods: ModifiersState::empty(),
        _tray: None,
        tray_check: None,
    };
    event_loop.run_app(&mut app)?;
    menu::dismiss();
    Ok(())
}

fn toolbar_layout(scale: f32, tool: Option<Tool>, r: [f32; 4], mon: [f32; 4]) -> (Toolbar, (f32, f32)) {
    let [mx0, my0, mx1, my1] = mon;
    let gap = 8.0 * scale;
    let mut tb = Toolbar::new(scale, tool, false);
    let (total_w, total_h) = (tb.w as f32 - 2.0 * tb.margin, tb.h as f32 - 2.0 * tb.margin);
    let x = (r[2] - tb.pill[2]).min(mx1 - total_w).max(mx0);
    let mut y = r[3] + gap;
    if y + total_h > my1 {
        tb = Toolbar::new(scale, tool, true);
        y = r[1] - gap - total_h;
        if y < my0 {
            y = (r[3].min(my1) - gap - total_h).max(my0);
        }
    }
    let origin = (x - tb.margin, y - tb.margin);
    (tb, origin)
}
