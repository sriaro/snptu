
use crate::{App, config, config::Action, menu, menu::Entry};
use winit::event_loop::ActiveEventLoop;

impl App {
    pub(crate) fn tray_menu(&mut self, el: &ActiveEventLoop, at: (i32, i32)) {
        let Some(text) = self.text.clone() else { return };
        let scale = el
            .available_monitors()
            .find(|m| {
                let (p, s) = (m.position(), m.size());
                at.0 >= p.x && at.0 < p.x + s.width as i32 && at.1 >= p.y && at.1 < p.y + s.height as i32
            })
            .map_or(1.0, |m| m.scale_factor() as f32);
        let key = &self.config.hotkeys[Action::Snip.index()];
        let capture = Entry::item("tray.capture", "Capture");
        let capture = if key.is_empty() { capture } else { capture.hint(config::pretty_hotkey(key)) };
        let rec_key = &self.config.hotkeys[Action::Record.index()];
        let record = Entry::item("tray.record", if self.recorder.is_some() { "Stop recording" } else { "Record screen\u{2026}" });
        let record = if rec_key.is_empty() { record } else { record.hint(config::pretty_hotkey(rec_key)) };
        let entries = vec![
            capture,
            record,
            Entry::item("tray.convert", "Convert images\u{2026}"),
            Entry::Sep,
            Entry::item("tray.hide_pins", "Hide / show pins"),
            Entry::item("tray.destroy_pins", "Destroy all pins"),
            Entry::Sep,
            Entry::item("tray.settings", "Settings\u{2026}"),
            Entry::Sep,
            Entry::item("tray.quit", "Quit"),
        ];
        let proxy = self.proxy.clone();
        menu::show(at, entries, scale, self.config.theme.is_dark(), text, move |id| {
            let _ = proxy.send_event(crate::UserEvent::Menu(tray_icon::menu::MenuId::new(id)));
        });
    }
}
