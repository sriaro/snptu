
use crate::{
    App, MenuAction, UserEvent,
    convert::{self, Kind, Progress, ResizeFilter, Source},
    convert_ui::{ConvertWindow, Effect, MAX_FILES, Wd, filter_label},
    dialogs, hwnd_of, log,
    menu::{self, Entry},
    text::Text,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};
use tray_icon::menu::MenuId;
use winit::{
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::ActiveEventLoop,
    keyboard::{Key, NamedKey},
    window::{CursorIcon, WindowId},
};

#[derive(Clone, Copy)]
pub enum ConvertPick {
    Format(Kind),
    Filter(ResizeFilter),
}

pub(crate) fn windows_dir() -> PathBuf {
    use windows::Win32::System::SystemInformation::GetWindowsDirectoryW;
    let mut buf = [0u16; 260];
    let n = unsafe { GetWindowsDirectoryW(Some(&mut buf)) } as usize;
    if n == 0 || n >= buf.len() {
        return PathBuf::from(r"C:\Windows");
    }
    PathBuf::from(String::from_utf16_lossy(&buf[..n]))
}

fn is_image(p: &std::path::Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(crate::convert::can_read)
}

fn expand(paths: Vec<PathBuf>, limit: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let max_scan = limit.saturating_mul(4).max(1024);
    for p in paths {
        if out.len() > limit {
            break;
        }
        if p.is_dir() {
            let file = |e: &std::fs::DirEntry, q: &std::path::Path| e.file_type().is_ok_and(|t| if t.is_symlink() { q.is_file() } else { t.is_file() });
            let mut inner = Vec::new();
            let mut scanned = 0;
            for e in std::fs::read_dir(&p).into_iter().flatten().flatten() {
                scanned += 1;
                if scanned > max_scan || out.len() + inner.len() > limit {
                    break;
                }
                let q = e.path();
                if is_image(&q) && file(&e, &q) {
                    inner.push(q);
                }
            }
            inner.sort();
            out.extend(inner);
        } else {
            out.push(p);
        }
    }
    out
}

fn keep_dropped(dropped: &mut Vec<PathBuf>, path: PathBuf) {
    if dropped.len() <= MAX_FILES {
        dropped.push(path);
    }
}

fn rehover(w: &mut ConvertWindow, text: &Text) {
    let hit = w.hit(text, w.cursor.0, w.cursor.1).map(|(id, _)| id);
    if hit != w.hover {
        w.hover = hit;
        let icon = match hit {
            Some(Wd::FPercent | Wd::FW | Wd::FH | Wd::FTemplate) => CursorIcon::Text,
            Some(_) => CursorIcon::Pointer,
            None => CursorIcon::Default,
        };
        w.window.set_cursor(icon);
    }
}

fn menu_key(key: &Key) -> Option<u16> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE, VK_UP};
    match key {
        Key::Named(NamedKey::ArrowDown) => Some(VK_DOWN.0),
        Key::Named(NamedKey::ArrowUp) => Some(VK_UP.0),
        Key::Named(NamedKey::ArrowLeft) => Some(VK_LEFT.0),
        Key::Named(NamedKey::ArrowRight) => Some(VK_RIGHT.0),
        Key::Named(NamedKey::Enter) => Some(VK_RETURN.0),
        Key::Named(NamedKey::Space) => Some(VK_SPACE.0),
        Key::Named(NamedKey::Escape) => Some(VK_ESCAPE.0),
        _ => None,
    }
}

const MAX_REQUEST_BYTES: u64 = 4 << 20;

pub(crate) fn requests_dir() -> PathBuf {
    std::env::temp_dir().join("snptu-requests")
}

pub(crate) fn queue_request(dir: &Path, body: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    convert::write_atomic(&dir.join(format!("{}-{nanos}.txt", std::process::id())), body.as_bytes())
}

fn read_request(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.take(MAX_REQUEST_BYTES + 1).read_to_end(&mut buf).ok()?;
    if buf.len() as u64 > MAX_REQUEST_BYTES {
        return None;
    }
    String::from_utf8(buf).ok()
}

fn take_requests(dir: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    if !std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir() && !m.is_symlink()) {
        return lines;
    }
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = e.path();
        if !path.extension().is_some_and(|x| x == "txt") || !e.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let stale = e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|age| age > std::time::Duration::from_secs(120)));
        if !stale && let Some(text) = read_request(&path) {
            lines.extend(text.lines().filter(|l| !l.trim().is_empty()).map(str::to_string));
        }
        let _ = std::fs::remove_file(&path);
    }
    lines
}

pub(crate) fn cancel_all(batches: &[Arc<Progress>]) {
    for p in batches {
        p.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

pub(crate) fn running(batches: &[Arc<Progress>]) -> u32 {
    batches.iter().filter(|p| !p.ended.load(std::sync::atomic::Ordering::Relaxed)).count() as u32
}

fn register(batches: &mut Vec<Arc<Progress>>, progress: Arc<Progress>) {
    batches.retain(|p| !p.ended.load(std::sync::atomic::Ordering::Relaxed));
    batches.push(progress);
}

fn end_batch(progress: &Progress, notify: &dyn Fn()) {
    progress.ended.store(true, std::sync::atomic::Ordering::Relaxed);
    notify();
}

fn fail_the_rest(progress: &Progress) {
    let mut outcomes = progress.outcomes.lock().unwrap_or_else(|e| e.into_inner());
    for o in outcomes.iter_mut().filter(|o| o.is_none()) {
        *o = Some(convert::Outcome::Failed("internal error".to_string()));
        progress.finished.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl App {
    pub(crate) fn open_convert(&mut self, el: &ActiveEventLoop, files: Vec<PathBuf>) {
        self.open_convert_with(el, files, |_| {});
    }

    fn open_convert_with(&mut self, el: &ActiveEventLoop, mut files: Vec<PathBuf>, fill: impl FnOnce(&mut ConvertWindow)) {
        if self.session.is_some() {
            self.end_capture();
        }
        let mut fill = Some(fill);
        if self.convert.is_none() {
            crate::settings::refresh_motion();
            match ConvertWindow::new(el, self.config.theme.is_dark()) {
                Ok(mut w) => {
                    w.apply_prefs(&self.config.convert);
                    w.add_files(expand(std::mem::take(&mut files), MAX_FILES));
                    if let Some(f) = fill.take() {
                        f(&mut w);
                    }
                    self.convert = Some(w);
                    self.draw_convert();
                    if let Some(w) = &self.convert {
                        crate::settings::cloak(&w.window, true);
                        w.show();
                    }
                    self.draw_convert();
                    if let Some(w) = self.convert.as_mut() {
                        w.begin_opening();
                    }
                }
                Err(e) => return log(&format!("convert window: {e:#}")),
            }
        }
        if let Some(w) = self.convert.as_mut() {
            w.add_files(expand(files, MAX_FILES));
            if let Some(f) = fill.take() {
                f(w);
            }
            w.window.request_redraw();
            w.window.focus_window();
        }
        self.pump_estimate();
    }

    pub(crate) fn open_convert_pixels(&mut self, el: &ActiveEventLoop, rgba: Vec<u8>, w: u32, h: u32) {
        let stamp = crate::config::now_stamp();
        let name = crate::config::expand_name(&self.config.file_name, &stamp);
        let dir = self.config.save_dir.clone();
        self.open_convert_with(el, Vec::new(), move |win| win.add_pixels(Arc::new(rgba), w, h, name, Some(dir)));
    }

    pub(crate) fn convert_request(&mut self, el: &ActiveEventLoop) {
        let mut files: Vec<PathBuf> = Vec::new();
        let mut settings = false;
        let (mut record, mut capture) = (false, false);
        for line in take_requests(&requests_dir()) {
            if line == "--settings" {
                settings = true;
            } else if line == "--record" {
                record = true;
            } else if line == "--capture" {
                capture = true;
            } else {
                files.push(PathBuf::from(line));
            }
        }
        files.truncate(MAX_FILES);
        if settings {
            self.open_settings(el);
        }
        if !files.is_empty() {
            self.open_convert(el, files);
        }
        if record || capture {
            self.capture_request(record);
        }
    }

    pub(crate) fn convert_from_args(&mut self, el: &ActiveEventLoop) {
        let args: Vec<String> = crate::args_lossy().into_iter().skip(1).collect();
        match args.first().map(String::as_str) {
            Some("--record") => return self.capture_request(true),
            Some("--capture") => return self.capture_request(false),
            Some("--convert-ui") if args.len() > 1 => self.open_convert(el, args[1..].iter().map(PathBuf::from).collect()),
            Some("--settings") => self.open_settings(el),
            _ => {}
        }
        self.convert_request(el);
    }

    pub(crate) fn draw_convert(&mut self) {
        let dark = self.config.theme.is_dark();
        let text = self.text.clone();
        if let Some(w) = self.convert.as_mut() {
            w.draw(text.as_deref(), dark);
        }
    }

    pub(crate) fn close_convert(&mut self) {
        menu::dismiss();
        if let Some(w) = &self.convert
            && let Some(p) = &w.progress
        {
            p.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.remember_convert_options();
        if let Some(w) = self.convert.as_mut() {
            w.finish_opening();
            let window = w.window.clone();
            crate::settings::dismiss(&window, |t| w.present_closing(t));
        }
        self.convert = None;
        crate::mem::trim();
    }

    pub(crate) fn remember_convert_options(&mut self) {
        let Some(w) = &self.convert else { return };
        let prefs = w.prefs();
        if prefs != self.config.convert {
            self.config.convert = prefs;
            if let Err(e) = self.config.save() {
                log(&format!("config save: {e:#}"));
            }
        }
    }

    pub(crate) fn convert_char(&mut self, c: char) -> bool {
        let Some(w) = self.convert.as_mut() else { return false };
        if !crate::is_foreground(&w.window) {
            return false;
        }
        let Some(e) = w.editor_mut() else { return false };
        e.insert(c);
        w.window.request_redraw();
        true
    }

    pub(crate) fn convert_event(&mut self, ev: WindowEvent) {
        let redraw = matches!(ev, WindowEvent::RedrawRequested | WindowEvent::Resized(_));
        self.convert_event_inner(ev);
        if !redraw {
            self.pump_estimate();
        }
    }

    fn convert_event_inner(&mut self, ev: WindowEvent) {
        let Some(text) = self.text.clone() else { return };
        if matches!(&ev, WindowEvent::MouseInput { state: ElementState::Pressed, .. } | WindowEvent::KeyboardInput { .. } | WindowEvent::MouseWheel { .. } | WindowEvent::DroppedFile(_))
            && self.convert.as_ref().is_some_and(|w| w.opening.is_some())
        {
            if let Some(w) = self.convert.as_mut() {
                w.finish_opening();
            }
            self.draw_convert();
        }
        match ev {
            WindowEvent::CloseRequested => self.close_convert(),
            WindowEvent::RedrawRequested if self.convert.as_mut().is_some_and(|w| !w.frames.allow()) => {}
            WindowEvent::RedrawRequested | WindowEvent::Resized(_) => {
                let dropped = self.convert.as_mut().map(|w| std::mem::take(&mut w.dropped)).unwrap_or_default();
                let added = !dropped.is_empty();
                if let Some(w) = self.convert.as_mut().filter(|_| added) {
                    w.add_files(expand(dropped, MAX_FILES));
                }
                self.draw_convert();
                if added {
                    self.pump_estimate();
                }
            }
            WindowEvent::ModifiersChanged(m) => {
                if let Some(w) = self.convert.as_mut() {
                    w.mods = m.state();
                }
            }
            WindowEvent::DroppedFile(path) => {
                if let Some(w) = self.convert.as_mut() {
                    keep_dropped(&mut w.dropped, path);
                    w.window.request_redraw();
                }
            }
            WindowEvent::ThemeChanged(_) => {
                if let Some(w) = &self.convert {
                    w.window.request_redraw();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if let Some(w) = self.convert.as_mut()
                    && w.over_list(w.cursor.0, w.cursor.1)
                {
                    let dy = match delta {
                        MouseScrollDelta::LineDelta(_, y) => -y * 42.0,
                        MouseScrollDelta::PixelDelta(p) => -(p.y as f32) / w.window.scale_factor() as f32,
                    };
                    w.scroll_by(dy);
                    rehover(w, &text);
                    w.window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let Some(w) = self.convert.as_mut() else { return };
                w.cursor = (position.x as f32, position.y as f32);
                if let Some(drag) = w.drag {
                    let rect = w.layout(&text).items.iter().find(|p| p.id == drag).map(|p| p.rect);
                    if let Some(r) = rect {
                        w.slider_from_x(drag, r, w.cursor.0);
                    }
                    w.window.request_redraw();
                    return;
                }
                rehover(w, &text);
                w.window.request_redraw();
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(w) = self.convert.as_mut()
                    && w.hover.take().is_some()
                {
                    w.window.request_redraw();
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                if let Some(w) = self.convert.as_mut() {
                    w.presses.release(Instant::now());
                    if w.drag.take().is_some() {
                        rehover(w, &text);
                    }
                    w.window.request_redraw();
                }
            }
            WindowEvent::Focused(false) => {
                if menu::is_open() {
                    menu::close();
                }
                if let Some(w) = self.convert.as_mut() {
                    w.presses.release(Instant::now());
                    w.window.request_redraw();
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let effect = {
                    let Some(w) = self.convert.as_mut() else { return };
                    if w.in_title_bar(w.cursor.1) {
                        let _ = w.window.drag_window();
                        self.revive_frames();
                        return;
                    }
                    let hit = w.hit(&text, w.cursor.0, w.cursor.1);
                    let effect = match hit {
                        Some((wd, rect)) => {
                            if !matches!(wd, Wd::FPercent | Wd::FW | Wd::FH | Wd::FTemplate) {
                                w.presses.press(wd, Instant::now());
                            }
                            w.click(wd, rect)
                        }
                        None => {
                            w.focus = None;
                            None
                        }
                    };
                    if matches!(effect, Some(Effect::FormatMenu(_) | Effect::FilterMenu(_) | Effect::AddFiles | Effect::ChooseFolder)) {
                        w.presses.release(Instant::now());
                    }
                    rehover(w, &text);
                    effect
                };
                if let Some(w) = &self.convert {
                    w.window.request_redraw();
                }
                if let Some(effect) = effect {
                    self.convert_effect(effect);
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if menu::is_open() {
                    if let Some(vk) = menu_key(&event.logical_key) {
                        menu::key(vk);
                    }
                    return;
                }
                let ctrl = self.convert.as_ref().is_some_and(|w| w.mods.control_key());
                let Some(w) = self.convert.as_mut() else { return };
                if w.focus.is_some() {
                    let paste = |w: &mut ConvertWindow| {
                        if let Ok(t) = arboard::Clipboard::new().and_then(|mut c| c.get_text())
                            && let Some(e) = w.editor_mut()
                        {
                            e.insert_str(t.lines().next().unwrap_or(""));
                        }
                    };
                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) | Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Tab) => w.focus = None,
                        Key::Character(c) if ctrl && c.eq_ignore_ascii_case("v") => paste(w),
                        key => {
                            if let Some(e) = w.editor_mut() {
                                match key {
                                    Key::Named(NamedKey::Backspace) => e.backspace(),
                                    Key::Named(NamedKey::Delete) => e.delete(),
                                    Key::Named(NamedKey::ArrowLeft) => e.left(),
                                    Key::Named(NamedKey::ArrowRight) => e.right(),
                                    Key::Named(NamedKey::Home) => e.home(),
                                    Key::Named(NamedKey::End) => e.end(),
                                    Key::Character(c) if ctrl && c.eq_ignore_ascii_case("a") => e.all = true,
                                    _ => {}
                                }
                            }
                        }
                    }
                } else if matches!(&event.logical_key, Key::Named(NamedKey::Escape)) {
                    self.close_convert();
                    return;
                }
                if let Some(w) = &self.convert {
                    w.window.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn convert_effect(&mut self, effect: Effect) {
        match effect {
            Effect::Close => self.close_convert(),
            Effect::AddFiles => {
                let Some(w) = &self.convert else { return };
                let (window, proxy) = (w.id(), self.proxy.clone());
                dialogs::in_background(window.into(), hwnd_of(&w.window), move |owner| {
                    let res = dialogs::open_images(owner).map_err(|e| format!("{e:#}"));
                    let _ = proxy.send_event(UserEvent::ConvertFiles(window, res));
                });
            }
            Effect::ChooseFolder => {
                let Some(w) = &self.convert else { return };
                let (window, proxy) = (w.id(), self.proxy.clone());
                let start = w.out_dir.clone().unwrap_or_else(|| self.config.save_dir.clone());
                dialogs::in_background(window.into(), hwnd_of(&w.window), move |owner| {
                    let res = dialogs::pick_folder(owner, &start).map_err(|e| format!("{e:#}"));
                    let _ = proxy.send_event(UserEvent::ConvertFolder(window, res));
                });
            }
            Effect::FormatMenu(rect) => {
                let w_kinds: &[Kind] = if self.convert.as_ref().is_some_and(|w| w.animation) { &Kind::ANIMATIONS } else { &Kind::IMAGES };
                let entries: Vec<(String, ConvertPick)> = w_kinds.iter().map(|k| (k.label().to_string(), ConvertPick::Format(*k))).collect();
                self.convert_dropdown(rect, entries);
            }
            Effect::FilterMenu(rect) => {
                let entries: Vec<(String, ConvertPick)> = [ResizeFilter::Lanczos, ResizeFilter::CatmullRom, ResizeFilter::Bilinear, ResizeFilter::Nearest]
                    .iter()
                    .map(|f| (filter_label(*f).to_string(), ConvertPick::Filter(*f)))
                    .collect();
                self.convert_dropdown(rect, entries);
            }
            Effect::Cancel => {
                if let Some(w) = self.convert.as_ref() {
                    if let Some(p) = &w.progress {
                        p.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    w.window.request_redraw();
                }
            }
            Effect::OpenFolder => {
                let first = self.convert.as_ref().and_then(|w| {
                    w.files.iter().find_map(|f| match &f.status {
                        crate::convert_ui::Status::Done(p) | crate::convert_ui::Status::Kept(p, _) => Some(p.clone()),
                        _ => None,
                    })
                });
                if let Some(p) = first {
                    use std::os::windows::process::CommandExt;
                    let _ = std::process::Command::new(windows_dir().join("explorer.exe"))
                        .raw_arg(format!("/select,\"{}\"", p.display()))
                        .spawn();
                }
            }
            Effect::Convert => {
                self.remember_convert_options();
                self.start_convert()
            }
        }
    }

    pub(crate) fn convert_files_chosen(&mut self, window: WindowId, res: Result<Option<Vec<PathBuf>>, String>) {
        let Some(w) = self.convert.as_mut().filter(|w| w.id() == window) else { return };
        match res {
            Ok(Some(files)) => {
                w.add_files(files);
                w.window.request_redraw();
                self.pump_estimate();
            }
            Ok(None) => {}
            Err(e) => log(&format!("open images: {e}")),
        }
    }

    pub(crate) fn convert_folder_chosen(&mut self, window: WindowId, res: Result<Option<PathBuf>, String>) {
        let Some(w) = self.convert.as_mut().filter(|w| w.id() == window) else { return };
        if let Ok(Some(dir)) = res {
            w.out_dir = Some(dir);
            w.window.request_redraw();
        }
    }

    fn convert_dropdown(&mut self, rect: [f32; 4], entries: Vec<(String, ConvertPick)>) {
        let Some(text) = self.text.clone() else { return };
        let Some(w) = self.convert.as_ref() else { return };
        let origin = w.window.inner_position().map_or((0, 0), |p| (p.x, p.y));
        let scale = w.window.scale_factor() as f32;
        let at = (origin.0 + rect[0] as i32, origin.1 + (rect[1] + rect[3]) as i32 + (4.0 * scale) as i32);
        self.menu_actions.retain(|_, a| !matches!(a, MenuAction::ConvertPick(_)));
        let list: Vec<Entry> = entries
            .into_iter()
            .enumerate()
            .map(|(i, (label, pick))| {
                let id = format!("convert{i}");
                self.menu_actions.insert(MenuId::new(&id), MenuAction::ConvertPick(pick));
                Entry::item(id, label)
            })
            .collect();
        let proxy = self.proxy.clone();
        menu::show_passive(at, list, scale, self.config.theme.is_dark(), text, move |id| {
            let _ = proxy.send_event(UserEvent::Menu(MenuId::new(id)));
        });
    }

    pub(crate) fn convert_pick(&mut self, pick: ConvertPick) {
        if let Some(w) = self.convert.as_mut() {
            match pick {
                ConvertPick::Format(k) => w.set_kind(k),
                ConvertPick::Filter(f) => w.filter = f,
            }
            w.window.request_redraw();
        }
    }

    pub(crate) fn pump_estimate(&mut self) {
        let Some(w) = self.convert.as_mut() else { return };
        let Some((round, src, params)) = w.estimate_wanted() else { return };
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            crate::par::lower_priority();
            let res = convert::estimate(&src, &params).map_err(|e| format!("{e:#}"));
            let _ = proxy.send_event(UserEvent::ConvertEstimate(round, res));
        });
    }

    pub(crate) fn convert_estimate_done(&mut self, round: u64, res: Result<convert::Estimate, String>) {
        if let Some(w) = self.convert.as_mut() {
            w.set_estimate(round, res);
        }
        self.pump_estimate();
    }

    fn start_convert(&mut self) {
        let proxy = self.proxy.clone();
        let Some(w) = self.convert.as_mut() else { return };
        if w.running() || w.files.is_empty() {
            return;
        }
        if running(&self.converting) > 0 {
            w.status = Some(("The previous conversion is still finishing".to_string(), false));
            w.window.request_redraw();
            return;
        }
        let (params, out) = match w.build() {
            Ok(v) => v,
            Err(msg) => {
                w.status = Some((msg, true));
                w.window.request_redraw();
                return;
            }
        };
        w.status = None;
        for (i, f) in w.files.iter_mut().enumerate() {
            f.status = crate::convert_ui::Status::Waiting;
            f.slot = Some(i);
        }
        let items: Vec<Source> = w.files.iter().map(|f| f.src.clone()).collect();
        let progress = Progress::new(items.len());
        w.progress = Some(progress.clone());
        let threads = w.threads;
        w.window.request_redraw();
        register(&mut self.converting, progress.clone());
        std::thread::spawn(move || {
            let notify = || {
                let _ = proxy.send_event(UserEvent::ConvertProgress);
            };
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| convert::run_batch(items, params, out, threads, progress.clone(), notify))).is_err() {
                fail_the_rest(&progress);
            }
            end_batch(&progress, &notify);
        });
    }
}
