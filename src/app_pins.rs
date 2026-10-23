
use crate::{
    App, MenuAction, Sel, beep, config, dialogs, hwnd_of, log,
    pin::{PendingPin, Pin, PinCmd, Place, size_ok, valid_image},
};
use crate::{
    convert::{self, Format},
    menu::{self, Entry},
};
use std::collections::HashMap;
use tray_icon::menu::MenuId;
use winit::{
    dpi::PhysicalSize,
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::ActiveEventLoop,
    keyboard::{Key, NamedKey},
    window::WindowId,
};

fn entry(actions: &mut HashMap<MenuId, MenuAction>, label: &str, win: WindowId, cmd: PinCmd) -> Entry {
    let id = format!("pin{}", actions.len());
    actions.insert(MenuId::new(&id), MenuAction::Pin(win, cmd));
    Entry::item(id, label)
}

const CF_DIBV5: u32 = 17;
const MAX_CLIPBOARD_BYTES: usize = 4 * 64 * 1024 * 1024 + 4096;
const MAX_DECODED_BYTES: u64 = 512 * 1024 * 1024;

fn clipboard_picture() -> Option<(bool, Vec<u8>)> {
    use windows::{
        Win32::{
            Foundation::HGLOBAL,
            System::{
                DataExchange::{
                    CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
                },
                Memory::{GlobalLock, GlobalSize, GlobalUnlock},
            },
        },
        core::w,
    };
    unsafe {
        let open = (0..6).any(|i| {
            if i > 0 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            OpenClipboard(None).is_ok()
        });
        if !open {
            return None;
        }
        let png = RegisterClipboardFormatW(w!("PNG"));
        let found = if png != 0 && IsClipboardFormatAvailable(png).is_ok() {
            Some((true, png))
        } else {
            IsClipboardFormatAvailable(CF_DIBV5).is_ok().then_some((false, CF_DIBV5))
        };
        let bytes = found.and_then(|(is_png, format)| {
            let mem = HGLOBAL(GetClipboardData(format).ok()?.0);
            let len = GlobalSize(mem);
            let data = GlobalLock(mem) as *const u8;
            if data.is_null() {
                return None;
            }
            let bytes = (len > 0 && len <= MAX_CLIPBOARD_BYTES).then(|| std::slice::from_raw_parts(data, len).to_vec());
            let _ = GlobalUnlock(mem);
            bytes.map(|b| (is_png, b))
        });
        let _ = CloseClipboard();
        bytes
    }
}

fn rgba_of(decoder: impl image::ImageDecoder) -> Option<(Vec<u8>, u32, u32)> {
    let (w, h) = decoder.dimensions();
    if !size_ok(w, h) || decoder.total_bytes() > MAX_DECODED_BYTES {
        return None;
    }
    Some((image::DynamicImage::from_decoder(decoder).ok()?.into_rgba8().into_raw(), w, h))
}

fn rgba_of_alpha_dib(dib: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    const HEADER: usize = 124;
    if dib.len() < HEADER {
        return None;
    }
    let word = |at: usize| u32::from_le_bytes(dib[at..at + 4].try_into().unwrap());
    let standard = matches!([word(40), word(44), word(48)], [0, 0, 0] | [0x00ff_0000, 0x0000_ff00, 0x0000_00ff]);
    if word(0) != HEADER as u32
        || dib[14..16] != [32, 0]
        || word(16) != 0
        || word(32) != 0
        || word(52) != 0xff00_0000
        || !standard
    {
        return None;
    }
    let (w, height) = (word(4) as i32, word(8) as i32);
    let (w, h) = (u32::try_from(w).ok()?, height.unsigned_abs());
    if !size_ok(w, h) {
        return None;
    }
    let (row, rows) = (w as usize * 4, h as usize);
    let pixels = dib[HEADER..].get(..row * rows)?;
    let mut rgba = vec![0u8; pixels.len()];
    for (y, src) in pixels.chunks_exact(row).enumerate() {
        let y = if height < 0 { y } else { rows - 1 - y };
        for (to, from) in rgba[y * row..][..row].as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
            to.copy_from_slice(&[from[2], from[1], from[0], from[3]]);
        }
    }
    Some((rgba, w, h))
}

fn decode_clipboard(is_png: bool, data: Vec<u8>) -> Option<(Vec<u8>, u32, u32)> {
    use image::{
        Limits,
        codecs::{bmp::BmpDecoder, png::PngDecoder},
    };
    use std::io::Cursor;
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if is_png {
            let mut limits = Limits::default();
            limits.max_alloc = Some(MAX_DECODED_BYTES);
            rgba_of(PngDecoder::with_limits(Cursor::new(&data), limits).ok()?)
        } else {
            rgba_of_alpha_dib(&data).or_else(|| rgba_of(BmpDecoder::new_without_file_header(Cursor::new(&data)).ok()?))
        }
    }))
    .ok()
    .flatten()
}

fn clipboard_image() -> Option<(Vec<u8>, u32, u32)> {
    let (is_png, data) = clipboard_picture()?;
    let (rgba, w, h) = decode_clipboard(is_png, data)?;
    if !valid_image(&rgba, w, h) {
        log(&format!("clipboard image refused: {w} x {h}, {} bytes", rgba.len()));
        return None;
    }
    Some((rgba, w, h))
}

impl App {
    pub(crate) fn pin_selection(&mut self) {
        self.finish_text();
        let Some((w, h, rgba)) = self.selection_pixels() else { return };
        if !size_ok(w, h) {
            log(&format!("pin selection: too large to pin ({w} x {h})"));
            beep();
            return;
        }
        let at = match self.session.as_ref() {
            Some(s) => match s.sel {
                Sel::Done(r) => (s.frame.x + r[0] as i32, s.frame.y + r[1] as i32),
                _ => (0, 0),
            },
            None => (0, 0),
        };
        self.pending_pins.push(PendingPin { rgba, w, h, place: Place::At(at.0, at.1) });
        self.end_capture();
    }

    pub(crate) fn paste_pin(&mut self) {
        let at = {
            use windows::Win32::{Foundation::POINT, UI::WindowsAndMessaging::GetCursorPos};
            let mut p = POINT::default();
            let _ = unsafe { GetCursorPos(&mut p) };
            (p.x, p.y)
        };
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let _ = proxy.send_event(crate::UserEvent::PastePin(clipboard_image(), at));
        });
    }

    pub(crate) fn pasted_pin(&mut self, image: Option<(Vec<u8>, u32, u32)>, at: (i32, i32)) {
        let Some((rgba, w, h)) = image else {
            log("paste as pin: no image on the clipboard");
            beep();
            return;
        };
        self.pending_pins.push(PendingPin { rgba, w, h, place: Place::Around(at.0, at.1) });
    }

    pub(crate) fn flush_pins(&mut self, el: &ActiveEventLoop) {
        if self.pending_pins.is_empty() {
            return;
        }
        for p in std::mem::take(&mut self.pending_pins) {
            match Pin::new(el, p) {
                Ok(mut pin) => {
                    if self.pins_click_through {
                        pin.set_click_through(true);
                    }
                    self.pins.push(pin);
                }
                Err(e) => {
                    log(&format!("pin window: {e:#}"));
                    beep();
                }
            }
        }
    }

    pub(crate) fn toggle_pins_visible(&mut self) {
        let any_visible = self.pins.iter().any(|p| !p.hidden);
        for p in &mut self.pins {
            p.set_hidden(any_visible);
        }
    }

    pub(crate) fn toggle_pins_click_through(&mut self) {
        self.pins_click_through = !self.pins_click_through;
        let on = self.pins_click_through;
        for p in &mut self.pins {
            p.set_click_through(on);
        }
    }

    pub(crate) fn destroy_all_pins(&mut self) {
        self.pins.clear();
        crate::mem::trim();
    }

    fn destroy_pin(&mut self, index: usize) {
        self.pins.remove(index);
        crate::mem::trim();
    }

    fn close_pin(&mut self, index: usize) {
        self.pins[index].set_hidden(true);
    }


    fn pin_context_menu(&mut self, index: usize) {
        let pin = &self.pins[index];
        let win = pin.id();
        let Some(text) = self.text.clone() else { return };
        let scale = pin.window.scale_factor() as f32;
        let at = {
            use windows::Win32::{Foundation::POINT, UI::WindowsAndMessaging::GetCursorPos};
            let mut p = POINT::default();
            let _ = unsafe { GetCursorPos(&mut p) };
            (p.x, p.y)
        };
        self.menu_actions.retain(|_, a| !matches!(a, MenuAction::Pin(..)));
        let actions = &mut self.menu_actions;
        let mut e = |label: &str, cmd: PinCmd| entry(actions, label, win, cmd);

        let zoom = [25, 50, 75, 100, 150, 200, 300].map(|p| e(&format!("{p}%"), PinCmd::Zoom(p))).to_vec();
        let opacity = [100, 80, 60, 40].map(|p| e(&format!("{p}%"), PinCmd::Opacity(p))).to_vec();
        let processing = vec![
            e("Grayscale", PinCmd::Gray),
            e("Invert colors", PinCmd::Invert),
            e("Flip horizontally", PinCmd::FlipH),
            e("Flip vertically", PinCmd::FlipV),
            e("Rotate clockwise", PinCmd::RotateCw),
            e("Rotate counter-clockwise", PinCmd::RotateCcw),
        ];
        let entries = vec![
            e("Copy image", PinCmd::Copy),
            e("Save image as\u{2026}", PinCmd::SaveAs),
            Entry::Sep,
            e("Shadow", PinCmd::Shadow).checked(pin.shadow),
            e("Always on top", PinCmd::OnTop).checked(pin.on_top),
            Entry::Sep,
            Entry::sub("Zoom", zoom),
            Entry::sub("Opacity", opacity),
            Entry::sub("Image processing", processing),
            e("Convert\u{2026}", PinCmd::Convert),
            Entry::Sep,
            e("Paste", PinCmd::Paste),
            e("Replace by file\u{2026}", PinCmd::ReplaceFile),
            Entry::Sep,
            e("Close", PinCmd::Close),
            e("Destroy", PinCmd::Destroy),
            Entry::Sep,
            Entry::sub(pin.size_label(), vec![e("Reset to 100%", PinCmd::Zoom(100))]),
        ];
        let proxy = self.proxy.clone();
        menu::show(at, entries, scale, self.config.theme.is_dark(), text, move |id| {
            let _ = proxy.send_event(crate::UserEvent::Menu(MenuId::new(id)));
        });
    }

    pub(crate) fn pin_convert(&mut self, el: &ActiveEventLoop, window: WindowId) {
        let Some(i) = self.pins.iter().position(|p| p.id() == window) else { return };
        let (w, h, px) = self.pins[i].image();
        let rgba = px.to_vec();
        self.open_convert_pixels(el, rgba, w, h);
    }

    pub(crate) fn pin_command(&mut self, window: WindowId, cmd: PinCmd) {
        let Some(i) = self.pins.iter().position(|p| p.id() == window) else { return };
        match cmd {
            PinCmd::Copy => self.copy_pin(i),
            PinCmd::SaveAs => self.save_pin(i),
            PinCmd::Convert => {}
            PinCmd::Shadow => {
                let on = !self.pins[i].shadow;
                self.pins[i].set_shadow(on);
            }
            PinCmd::OnTop => {
                let on = !self.pins[i].on_top;
                self.pins[i].set_on_top(on);
            }
            PinCmd::Zoom(p) => self.pins[i].set_zoom_percent(p),
            PinCmd::Opacity(p) => self.pins[i].set_opacity(p),
            PinCmd::Gray => self.pins[i].gray(),
            PinCmd::Invert => self.pins[i].invert(),
            PinCmd::FlipH => self.pins[i].flip_h(),
            PinCmd::FlipV => self.pins[i].flip_v(),
            PinCmd::RotateCw => self.pins[i].rotate(true),
            PinCmd::RotateCcw => self.pins[i].rotate(false),
            PinCmd::Paste => self.replace_pin_in_background(i, |_| clipboard_image().map(Some).ok_or_else(|| "paste into pin: no usable image on the clipboard".to_string())),
            PinCmd::ReplaceFile => self.replace_pin_in_background(i, |owner| match dialogs::open_image(owner) {
                Ok(Some(path)) => convert::decode_srgb(&path).map(Some).map_err(|e| format!("open {}: {e:#}", path.display())),
                Ok(None) => Ok(None),
                Err(e) => Err(format!("open image dialog: {e:#}")),
            }),
            PinCmd::Close => self.close_pin(i),
            PinCmd::Destroy => self.destroy_pin(i),
        }
    }

    fn copy_pin(&self, index: usize) {
        let (w, h, px) = self.pins[index].image();
        let res = arboard::Clipboard::new().and_then(|mut c| {
            c.set_image(arboard::ImageData { width: w as usize, height: h as usize, bytes: std::borrow::Cow::Borrowed(px) })
        });
        if let Err(e) = res {
            log(&format!("copy pin: {e}"));
            beep();
        }
    }

    const SAVE_TYPES: [Format; 4] = [
        Format::Png,
        Format::Jpeg(92),
        Format::WebpLossless,
        Format::Ico,
    ];

    fn save_pin(&mut self, index: usize) {
        let stem = config::expand_name(&self.config.file_name, &config::now_stamp());
        let owner = hwnd_of(&self.pins[index].window).map(|h| h.0 as isize);
        let (w, h, px) = self.pins[index].image();
        let px = px.to_vec();
        std::thread::spawn(move || {
            let owner = owner.map(|v| windows::Win32::Foundation::HWND(v as *mut std::ffi::c_void));
            let types = Self::SAVE_TYPES;
            let listed: Vec<(&str, &str)> = types.iter().map(|f| (f.label(), f.ext())).collect();
            let (mut path, picked) = match dialogs::save_formats(owner, &stem, &listed) {
                Ok(Some(r)) => r,
                Ok(None) => return,
                Err(e) => return log(&format!("save pin dialog: {e:#}")),
            };
            let typed = path.extension().and_then(|e| e.to_str()).and_then(Format::from_extension);
            let format = match typed {
                Some(f) => if f.ext() == types[picked].ext() { types[picked] } else { f },
                None => {
                    if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case(types[picked].ext())) {
                        path = convert::with_added_extension(&path, types[picked].ext());
                    }
                    types[picked]
                }
            };
            convert::convert_in_background(path, format, w, h, px);
        });
    }

    fn replace_pin_in_background(
        &mut self,
        index: usize,
        pick: impl FnOnce(Option<windows::Win32::Foundation::HWND>) -> Result<Option<(Vec<u8>, u32, u32)>, String> + Send + 'static,
    ) {
        let pin = &mut self.pins[index];
        if std::mem::replace(&mut pin.replacing, true) {
            return;
        }
        let (win, owner) = (pin.id(), hwnd_of(&pin.window).map(|h| h.0 as isize));
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let owner = owner.map(|v| windows::Win32::Foundation::HWND(v as *mut std::ffi::c_void));
            let _ = proxy.send_event(crate::UserEvent::PinFile(win, pick(owner)));
        });
    }

    pub(crate) fn pin_file_done(&mut self, window: WindowId, res: Result<Option<(Vec<u8>, u32, u32)>, String>) {
        let Some(pin) = self.pins.iter_mut().find(|p| p.id() == window) else { return };
        pin.replacing = false;
        match res {
            Ok(Some((rgba, w, h))) => {
                if !pin.set_image(rgba, w, h) {
                    log(&format!("open image: too large to pin ({w} x {h})"));
                    beep();
                }
            }
            Ok(None) => {}
            Err(e) => {
                log(&e);
                beep();
            }
        }
    }


    pub(crate) fn pin_event(&mut self, index: usize, ev: WindowEvent) {
        match ev {
            WindowEvent::CloseRequested => self.close_pin(index),
            WindowEvent::RedrawRequested => self.pins[index].draw(),
            WindowEvent::Resized(_) => self.pins[index].resized(),
            WindowEvent::ScaleFactorChanged { mut inner_size_writer, .. } => {
                let (w, h) = self.pins[index].zoomed_size();
                let _ = inner_size_writer.request_inner_size(PhysicalSize::new(w, h));
            }
            WindowEvent::CursorMoved { position, .. } => {
                let pin = &mut self.pins[index];
                pin.cursor = (position.x as f32, position.y as f32);
                if pin.grabbing() {
                    pin.drag_resize();
                } else {
                    pin.update_cursor();
                }
            }
            WindowEvent::CursorEntered { .. } => self.pins[index].hovered = true,
            WindowEvent::CursorLeft { .. } => self.pins[index].hovered = false,
            WindowEvent::MouseWheel { delta, .. } => {
                let steps = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
                };
                self.pins[index].zoom_by(steps);
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let pin = &mut self.pins[index];
                match pin.edge_at(pin.cursor) {
                    Some(dir) => pin.begin_grab(dir),
                    None => {
                        let _ = pin.window.drag_window();
                        self.revive_frames();
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                self.pins[index].end_grab();
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Right, .. } => {
                self.pin_context_menu(index)
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => match &event.logical_key {
                Key::Named(NamedKey::Escape) if self.pins[index].hovered => self.close_pin(index),
                Key::Named(NamedKey::Delete) if self.pins[index].hovered => self.destroy_pin(index),
                Key::Character(c) if self.mods.control_key() && c.eq_ignore_ascii_case("c") => self.copy_pin(index),
                _ => {}
            },
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            _ => {}
        }
    }
}
