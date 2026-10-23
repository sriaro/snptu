
use crate::{
    App, UserEvent, beep, config,
    config::Action,
    dialogs, hwnd_of, log,
    settings::{Hit, SettingsWindow, View},
    convert::Kind,
    theme::ThemeMode,
};
use anyhow::{Result, anyhow};
use global_hotkey::hotkey::HotKey;
use std::{path::{Path, PathBuf}, str::FromStr};
use winit::{
    event::{ElementState, KeyEvent, MouseButton, WindowEvent},
    event_loop::ActiveEventLoop,
    keyboard::{Key, KeyCode, NamedKey, PhysicalKey},
    window::{CursorIcon, WindowId},
};

fn output_descriptions(cfg: &config::Config) -> [String; 3] {
    let format = match cfg.format {
        Kind::Png => "Lossless and universal",
        Kind::Jpeg => "Small files, no transparency",
        _ => "Small files, keeps transparency",
    };
    let compression = match (cfg.format, cfg.effective_compression()) {
        (Kind::Png, _) => "PNG is always lossless",
        (_, config::LOSSLESS) => "Not used while lossless",
        _ => ["Smallest files", "Best quality for the size", "Sharper, larger files", "Closest to the original"][config::quality_step(cfg.quality)],
    };
    let lossless = match cfg.format {
        Kind::Png => "Always on for PNG",
        Kind::Webp if cfg.effective_compression() == config::LOSSLESS => "Identical pixels, larger file",
        Kind::Webp => "Smaller file, a little loss",
        _ => "JPEG is always lossy",
    };
    [format.to_string(), compression.to_string(), lossless.to_string()]
}

fn shell_menu_message(mode: u8, needs_developer_mode: bool, package_active: bool) -> &'static str {
    match mode {
        config::SHELL_OFF => "Removed from the Explorer menu",
        config::SHELL_MODERN if needs_developer_mode => "Added under Show more options. New menu needs Developer Mode",
        config::SHELL_MODERN if package_active => "Added Snptu to the new right-click menu",
        config::SHELL_MODERN => "Added Snptu to the right-click menu",
        _ => "Added Snptu to the classic menu (Show more options)",
    }
}

fn hotkey_taken(spec: &str, action: Action) -> String {
    format!("{} ({}) is used by another program", config::pretty_hotkey(spec), action.title())
}

fn name_to_save<'a>(draft: &'a str, saved: &str) -> Option<&'a str> {
    let name = draft.trim();
    (!name.is_empty() && name != saved).then_some(name)
}

fn config_reset_message(custom: bool, dir: Option<&Path>, exe_dir: Option<&Path>) -> (String, bool) {
    match dir {
        _ if custom => ("Couldn't move the settings back to the default folder".to_string(), true),
        Some(d) if exe_dir == Some(d) => ("Settings are back beside Snptu.exe".to_string(), false),
        Some(d) => (format!("Settings are now in {}", d.display()), false),
        None => ("Settings are back in the default folder".to_string(), false),
    }
}

fn config_reset_now() -> (String, bool) {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from));
    config_reset_message(config::config_dir_is_custom(), config::config_dir().as_deref(), exe_dir.as_deref())
}

fn hotkeys_to_reset(current: &[String; config::ACTION_COUNT], defaults: &[String; config::ACTION_COUNT]) -> Vec<Action> {
    Action::ALL
        .into_iter()
        .filter(|a| {
            let (now, default) = (&current[a.index()], &defaults[a.index()]);
            now != default && !config::same_hotkey(now, default)
        })
        .collect()
}

impl App {

    pub(crate) fn register_hotkeys(&mut self) -> Vec<String> {
        let mut failed = Vec::new();
        for action in Action::ALL {
            let spec = self.config.hotkeys[action.index()].clone();
            if spec.is_empty() {
                continue;
            }
            if let Err(e) = self.register_one(action, &spec) {
                log(&format!("hotkey {} ({spec}): {e}", action.key()));
                failed.push(hotkey_taken(&spec, action));
            }
        }
        failed
    }

    pub(crate) fn warn_hotkey_conflicts(&mut self, el: &ActiveEventLoop, failed: &[String]) {
        if failed.is_empty() {
            return;
        }
        self.open_settings_on(el, 3);
        if let Some(s) = self.settings.as_mut() {
            s.set_tab(3);
        }
        self.settings_message(&failed.join(". "), true);
    }

    pub(crate) fn sync_nested_hotkey(&mut self) -> Result<()> {
        let action = Action::SnipNested;
        let spec = self.config.hotkeys[action.index()].clone();
        if self.config.nested_capture && !spec.is_empty() {
            if self.hotkeys[action.index()].is_none()
                && let Err(e) = self.register_one(action, &spec)
            {
                log(&format!("hotkey {} ({spec}): {e}", action.key()));
                return Err(anyhow!("{} is used by another program", config::pretty_hotkey(&spec)));
            }
        } else {
            self.unregister_one(action);
        }
        Ok(())
    }

    fn register_one(&mut self, action: Action, spec: &str) -> Result<()> {
        if action == Action::SnipNested && !self.config.nested_capture {
            return Ok(());
        }
        let hk = HotKey::from_str(spec).map_err(|_| anyhow!("That key can't be used"))?;
        let mgr = self.hotkey_mgr.as_ref().ok_or_else(|| anyhow!("Hotkeys are unavailable"))?;
        mgr.register(hk).map_err(|_| anyhow!("Another program already uses that shortcut"))?;
        self.hotkeys[action.index()] = Some(hk);
        Ok(())
    }

    fn unregister_one(&mut self, action: Action) -> Option<HotKey> {
        let hk = self.hotkeys[action.index()].take()?;
        if let Some(m) = &self.hotkey_mgr {
            let _ = m.unregister(hk);
        }
        Some(hk)
    }

    pub(crate) fn set_hotkey(&mut self, action: Action, spec: Option<&str>) -> Result<()> {
        let i = action.index();
        match spec.filter(|s| !s.is_empty()) {
            None => {
                self.unregister_one(action);
                self.config.hotkeys[i].clear();
            }
            Some(spec) => {
                config::validate_hotkey(spec).map_err(|m| anyhow!(m))?;
                if let Some(other) = Action::ALL
                    .iter()
                    .find(|a| **a != action && config::same_hotkey(&self.config.hotkeys[a.index()], spec))
                {
                    return Err(anyhow!("Already used by \"{}\"", other.title()));
                }
                let old = self.unregister_one(action);
                if let Err(e) = self.register_one(action, spec) {
                    if let Some(o) = old {
                        if self.hotkey_mgr.as_ref().is_some_and(|m| m.register(o).is_ok()) {
                            self.hotkeys[i] = Some(o);
                        } else {
                            let old_spec = &self.config.hotkeys[i];
                            log(&format!("hotkey {} ({old_spec}): taken back by another program", action.key()));
                            return Err(anyhow!("{e}. {}", hotkey_taken(old_spec, action)));
                        }
                    }
                    return Err(e);
                }
                self.config.hotkeys[i] = spec.to_string();
            }
        }
        Ok(())
    }

    fn suspend_hotkeys(&mut self) {
        if !self.suspended.is_empty() {
            return;
        }
        for action in Action::ALL {
            if let Some(hk) = self.unregister_one(action) {
                self.suspended.push((action, hk));
            }
        }
    }

    fn resume_hotkeys(&mut self, except: Option<Action>) -> Vec<String> {
        let mut failed = Vec::new();
        for (action, hk) in std::mem::take(&mut self.suspended) {
            if self.hotkey_mgr.as_ref().is_some_and(|m| m.register(hk).is_ok()) {
                self.hotkeys[action.index()] = Some(hk);
            } else if except != Some(action) {
                let spec = &self.config.hotkeys[action.index()];
                log(&format!("hotkey {} ({spec}): taken while Settings listened", action.key()));
                failed.push(hotkey_taken(spec, action));
            }
        }
        failed
    }

    fn tell_lost_hotkeys(&mut self, lost: &[String]) {
        if !lost.is_empty() {
            self.settings_message(&lost.join(". "), true);
        }
    }

    pub(crate) fn unregister_hotkeys(&mut self) {
        for action in Action::ALL {
            self.unregister_one(action);
        }
        self.suspended.clear();
    }


    pub(crate) fn open_settings(&mut self, el: &ActiveEventLoop) {
        self.open_settings_on(el, 0);
    }

    pub(crate) fn open_settings_on(&mut self, el: &ActiveEventLoop, tab: usize) {
        if self.session.is_some() {
            self.end_capture();
        }
        if let Some(s) = &self.settings {
            s.window.focus_window();
            return;
        }
        self.autostart = config::autostart_enabled();
        crate::settings::refresh_motion();
        match SettingsWindow::new(el, self.config.theme.is_dark(), tab) {
            Ok(s) => {
                self.settings = Some(s);
                self.draw_settings();
                if let Some(s) = &self.settings {
                    crate::settings::cloak(&s.window, true);
                    s.show();
                }
                self.draw_settings();
                if let Some(s) = self.settings.as_mut() {
                    s.begin_opening();
                    s.window.focus_window();
                }
            }
            Err(e) => log(&format!("settings window: {e:#}")),
        }
    }

    pub(crate) fn revive_frames(&mut self) {
        if let Some(s) = self.settings.as_mut() {
            s.frame_revive();
        }
        if let Some(c) = self.convert.as_mut() {
            c.frame_revive();
        }
    }

    fn draw_settings(&mut self) {
        let editing_name = self.settings.as_ref().is_some_and(|s| s.editing_name);
        let template = match self.settings.as_ref() {
            Some(s) if editing_name => s.draft.clone(),
            _ => self.config.file_name.clone(),
        };
        let preview = format!("{}.{}", config::expand_name(&template, &config::now_stamp()), self.config.format.ext());
        let view = View {
            hotkeys: std::array::from_fn(|i| {
                let spec = &self.config.hotkeys[i];
                if spec.is_empty() { String::new() } else { config::pretty_hotkey(spec) }
            }),
            autostart: self.autostart,
            auto_save: self.config.auto_save,
            nested: self.config.nested_capture,
            shell_menu: self.config.shell_menu,
            remember_colors: self.config.remember_colors,
            save_dir: self.config.save_dir.display().to_string(),
            file_name: template,
            file_preview: preview,
            theme: self.config.theme,
            format: config::OUTPUT_KINDS.iter().position(|k| *k == self.config.format).unwrap_or(0),
            compression: config::quality_step(self.config.quality),
            comp_enabled: self.config.format.has_quality() && self.config.effective_compression() != config::LOSSLESS,
            lossless: self.config.effective_compression() == config::LOSSLESS,
            lossless_enabled: self.config.format.has_lossless_switch(),
            output_desc: output_descriptions(&self.config),
            config_dir: config::config_dir().map_or_else(String::new, |d| d.display().to_string()),
            config_custom: config::config_dir_is_custom(),
            save_dir_custom: self.config.save_dir != config::default_save_dir(),
            dark: self.config.theme.is_dark(),
            rec: self.config.rec,
        };
        if let Some(s) = self.settings.as_mut() {
            s.draw(&view, self.text.as_deref());
        }
    }

    pub(crate) fn settings_message(&mut self, text: &str, is_error: bool) {
        if let Some(s) = self.settings.as_mut() {
            s.say(text, is_error);
        }
    }

    fn settings_clear_message(&mut self) {
        if let Some(s) = self.settings.as_mut() {
            s.message = None;
            s.window.request_redraw();
        }
    }

    pub(crate) fn save_config(&mut self) -> bool {
        let Err(e) = self.config.save() else { return true };
        log(&format!("config save: {e:#}"));
        if self.settings.is_some() {
            self.settings_message(&format!("Couldn't save settings: {e}"), true);
        } else {
            beep();
        }
        false
    }

    fn theme_changed(&self) {
        if let Some(c) = &self.convert {
            c.window.request_redraw();
        }
    }

    fn stop_listening(&mut self) {
        if let Some(s) = self.settings.as_mut()
            && s.listening.take().is_some()
        {
            s.window.request_redraw();
        }
        let lost = self.resume_hotkeys(None);
        self.tell_lost_hotkeys(&lost);
    }

    fn commit_name(&mut self) {
        let Some(s) = self.settings.as_mut() else { return };
        if !s.editing_name {
            return;
        }
        s.editing_name = false;
        let draft = std::mem::take(&mut s.draft);
        s.window.request_redraw();
        if draft.trim().is_empty() {
            self.settings_message("File name unchanged", false);
        } else if let Some(name) = name_to_save(&draft, &self.config.file_name) {
            self.config.file_name = name.to_string();
            if self.save_config() {
                self.settings_message("File name saved", false);
            }
        }
    }

    fn cancel_name(&mut self) {
        if let Some(s) = self.settings.as_mut()
            && s.editing_name
        {
            s.editing_name = false;
            s.draft.clear();
            s.window.request_redraw();
        }
    }

    pub(crate) fn settings_char(&mut self, c: char) -> bool {
        let Some(s) = self.settings.as_mut() else { return false };
        if !s.editing_name || !crate::is_foreground(&s.window) {
            return false;
        }
        if !c.is_control() && s.draft.chars().count() < 120 {
            s.draft.push(c);
            s.window.request_redraw();
        }
        true
    }

    fn capture_shortcut(&mut self, action: Action, ev: &KeyEvent) {
        let PhysicalKey::Code(code) = ev.physical_key else { return };
        if matches!(
            code,
            KeyCode::ControlLeft
                | KeyCode::ControlRight
                | KeyCode::AltLeft
                | KeyCode::AltRight
                | KeyCode::ShiftLeft
                | KeyCode::ShiftRight
                | KeyCode::SuperLeft
                | KeyCode::SuperRight
        ) {
            return;
        }
        let Some(s) = self.settings.as_mut() else { return };
        if code == KeyCode::Escape {
            s.message = None;
            self.stop_listening();
            return;
        }
        if matches!(code, KeyCode::Backspace | KeyCode::Delete) && !s.mods.control_key() && !s.mods.alt_key() {
            s.listening = None;
            let lost = self.resume_hotkeys(Some(action));
            match self.set_hotkey(action, None) {
                Ok(()) => {
                    if self.save_config() {
                        self.settings_message("Shortcut cleared", false);
                    }
                }
                Err(e) => self.settings_message(&e.to_string(), true),
            }
            self.tell_lost_hotkeys(&lost);
            return;
        }
        let mut parts = Vec::new();
        if s.mods.control_key() {
            parts.push("CONTROL".to_string());
        }
        if s.mods.alt_key() {
            parts.push("ALT".to_string());
        }
        if s.mods.shift_key() {
            parts.push("SHIFT".to_string());
        }
        if s.mods.super_key() {
            parts.push("SUPER".to_string());
        }
        parts.push(format!("{code:?}"));
        let spec = parts.join("+");
        if let Err(msg) = config::validate_hotkey(&spec) {
            s.message = Some((msg.to_string(), true));
            s.window.request_redraw();
            return;
        }
        s.listening = None;
        let lost = self.resume_hotkeys(Some(action));
        let switched_on = action == Action::SnipNested && !self.config.nested_capture;
        if switched_on {
            self.config.nested_capture = true;
        }
        match self.set_hotkey(action, Some(&spec)) {
            Ok(()) => {
                if self.save_config() {
                    self.settings_message("Shortcut updated", false);
                }
            }
            Err(e) => {
                if switched_on {
                    self.config.nested_capture = false;
                }
                self.settings_message(&e.to_string(), true);
            }
        }
        self.tell_lost_hotkeys(&lost);
    }

    fn settings_click(&mut self) {
        let Some(s) = self.settings.as_mut() else { return };
        let hover = s.hover;
        if s.listening.is_some() && !matches!(hover, Some(Hit::Hotkey(_))) {
            s.message = None;
            self.stop_listening();
        }
        if self.settings.as_ref().is_some_and(|s| s.editing_name) && hover != Some(Hit::FileName) {
            self.commit_name();
        }
        let Some(s) = self.settings.as_mut() else { return };
        match hover {
            Some(Hit::Close) => self.close_settings(),
            Some(Hit::Tab(i)) => {
                s.set_tab(i);
                s.message = None;
                s.window.request_redraw();
            }
            Some(Hit::Seg(0, i)) => {
                self.config.theme = ThemeMode::ALL[i.min(2)];
                s.window.request_redraw();
                self.save_config();
                self.theme_changed();
            }
            Some(Hit::Seg(1, i)) => {
                self.config.format = config::OUTPUT_KINDS[i.min(config::OUTPUT_KINDS.len() - 1)];
                s.window.request_redraw();
                self.save_config();
            }
            Some(Hit::Seg(g @ 3..=10, i)) => {
                use crate::recfmt::{COUNTDOWN, FPS, MAX_SECS, SCALE};
                let r = &mut self.config.rec;
                match g {
                    3 => {
                        r.webp = i == 1;
                        r.mp4 = i == 2;
                    }
                    4 => r.fps = FPS[i.min(FPS.len() - 1)],
                    5 => r.scale = SCALE[i.min(SCALE.len() - 1)],
                    6 => r.quality = i.min(2) as u8,
                    7 if !r.mp4 => r.filter = i.min(3) as u8,
                    7 => {}
                    8 => r.pointer = i.min(2) as u8,
                    9 => r.max_secs = MAX_SECS[i.min(MAX_SECS.len() - 1)],
                    _ => r.countdown = COUNTDOWN[i.min(COUNTDOWN.len() - 1)],
                }
                s.window.request_redraw();
                self.save_config();
            }
            Some(Hit::RecDither) if !(self.config.rec.webp || self.config.rec.mp4) => {
                self.config.rec.dither = !self.config.rec.dither;
                s.window.request_redraw();
                self.save_config();
            }
            Some(Hit::Seg(11, i)) => {
                self.config.shell_menu = [config::SHELL_OFF, config::SHELL_MODERN, config::SHELL_CLASSIC][i.min(2)];
                let saved = self.save_config();
                match crate::shell_menu::sync(self.config.shell_menu) {
                    Ok(()) if saved => self.settings_message(
                        shell_menu_message(self.config.shell_menu, crate::shell_pkg::needs_developer_mode(), crate::shell_pkg::active()),
                        false,
                    ),
                    Ok(()) => {}
                    Err(e) => self.settings_message(&format!("Couldn't change the Explorer menu: {e}"), true),
                }
            }
            Some(Hit::Seg(..) | Hit::RecDither) => {}
            Some(Hit::Quality) => {
                if self.config.format.has_quality() && self.config.effective_compression() != config::LOSSLESS && s.on_quality_track(s.cursor.0) {
                    s.drag_quality = true;
                    self.config.quality = s.quality_at(s.cursor.0);
                    s.window.request_redraw();
                }
            }
            Some(Hit::Lossless) => {
                s.window.request_redraw();
                if self.config.format.has_lossless_switch() {
                    self.config.compression = if self.config.compression == config::LOSSLESS { config::CUSTOM } else { config::LOSSLESS };
                    self.save_config();
                }
            }
            Some(Hit::Hotkey(action)) => {
                s.listening = Some(action);
                s.message = Some(("Press the shortcut, Backspace clears, Esc cancels".into(), false));
                s.window.request_redraw();
                self.suspend_hotkeys();
            }
            Some(Hit::FileName) => {
                if !s.editing_name {
                    s.editing_name = true;
                    s.draft = self.config.file_name.clone();
                    s.message = Some(("Put date tokens between $ $: yyyy MM dd HH mm ss. Enter saves.".into(), false));
                    s.window.request_redraw();
                }
            }
            Some(Hit::Autostart) => {
                let want = !self.autostart;
                match config::set_autostart(want) {
                    Ok(()) if config::autostart_enabled() == want => {
                        self.autostart = want;
                        self.settings_clear_message();
                    }
                    Ok(()) => {
                        self.autostart = config::autostart_enabled();
                        self.settings_message(if want { "Windows didn't keep the startup entry" } else { "Couldn't remove the startup entry" }, true);
                    }
                    Err(e) => self.settings_message(&format!("Couldn't change startup: {e}"), true),
                }
            }
            Some(Hit::AutoSave) => {
                self.config.auto_save = !self.config.auto_save;
                if self.save_config() {
                    self.settings_clear_message();
                }
            }
            Some(Hit::Nested) => {
                self.config.nested_capture = !self.config.nested_capture;
                let synced = self.sync_nested_hotkey();
                let saved = self.save_config();
                match synced {
                    Err(e) => self.settings_message(&e.to_string(), true),
                    Ok(()) if saved => {
                        if self.config.nested_capture && self.config.hotkeys[Action::SnipNested.index()].is_empty() {
                            self.settings_message("Nested capture is on: give it a shortcut on the Hotkeys tab", false);
                        } else {
                            self.settings_clear_message();
                        }
                    }
                    Ok(()) => {}
                }
            }
            Some(Hit::RememberColors) => {
                self.config.remember_colors = !self.config.remember_colors;
                if !self.config.remember_colors {
                    self.config.colors = std::array::from_fn(|_| String::new());
                }
                if self.save_config() {
                    self.settings_clear_message();
                }
            }
            Some(Hit::ConfigDir) => {
                let start = config::config_dir().unwrap_or_default();
                self.pick_settings_folder(Hit::ConfigDir, start);
            }
            Some(Hit::ConfigReset) => {
                if config::config_dir_is_custom() {
                    match config::set_config_dir(None, &self.config) {
                        Ok(()) => {
                            let (msg, is_error) = config_reset_now();
                            self.settings_message(&msg, is_error);
                        }
                        Err(e) => self.settings_message(&format!("Couldn't move the settings: {e}"), true),
                    }
                }
            }
            Some(Hit::RestoreDefaults) => {
                let d = config::Config::defaults();
                let tab = s.tab;
                let mut clash: Vec<String> = Vec::new();
                match tab {
                    0 => {
                        self.config.theme = d.theme;
                        self.theme_changed();
                        self.config.nested_capture = d.nested_capture;
                        if let Err(e) = self.sync_nested_hotkey() {
                            clash.push(e.to_string());
                        }
                        self.config.shell_menu = d.shell_menu;
                        if let Err(e) = crate::shell_menu::sync(self.config.shell_menu) {
                            log(&format!("explorer menu: {e:#}"));
                            clash.push(format!("Couldn't change the Explorer menu: {e}"));
                        }
                        let stopped = config::set_autostart(false);
                        self.autostart = config::autostart_enabled();
                        match stopped {
                            Err(e) => {
                                log(&format!("restore autostart: {e:#}"));
                                clash.push(format!("Couldn't change startup: {e}"));
                            }
                            Ok(()) if self.autostart => clash.push("Couldn't remove the startup entry".to_string()),
                            Ok(()) => {}
                        }
                        if config::config_dir_is_custom() {
                            match config::set_config_dir(None, &self.config) {
                                Ok(()) => {
                                    if let (msg, true) = config_reset_now() {
                                        clash.push(msg);
                                    }
                                }
                                Err(e) => {
                                    log(&format!("restore settings folder: {e:#}"));
                                    clash.push(format!("Couldn't move the settings: {e}"));
                                }
                            }
                        }
                    }
                    1 => {
                        self.config.auto_save = d.auto_save;
                        self.config.save_dir = d.save_dir;
                        self.config.file_name = d.file_name;
                        self.config.remember_colors = d.remember_colors;
                        self.config.colors = d.colors;
                        self.config.format = d.format;
                        self.config.compression = d.compression;
                        self.config.quality = d.quality;
                    }
                    2 => self.config.rec = d.rec,
                    _ => {
                        let reset = hotkeys_to_reset(&self.config.hotkeys, &d.hotkeys);
                        let before: Vec<String> = reset.iter().map(|a| self.config.hotkeys[a.index()].clone()).collect();
                        for a in &reset {
                            let _ = self.set_hotkey(*a, None);
                        }
                        for (a, old) in reset.iter().zip(&before) {
                            if let Err(e) = self.set_hotkey(*a, Some(&d.hotkeys[a.index()])) {
                                log(&format!("restore hotkey: {e:#}"));
                                clash.push(format!("{}: {e}", a.title()));
                                let _ = self.set_hotkey(*a, Some(old));
                            }
                        }
                    }
                }
                let saved = self.save_config();
                if !clash.is_empty() {
                    self.settings_message(&clash.join(". "), true);
                } else if saved {
                    self.settings_message("This page is back to its defaults", false);
                }
            }
            Some(Hit::SaveDirReset) => {
                if self.config.save_dir != config::default_save_dir() {
                    self.config.save_dir = config::default_save_dir();
                    if self.save_config() {
                        self.settings_message("Save folder is back to the default", false);
                    }
                }
            }
            Some(Hit::ChooseFolder) => self.pick_settings_folder(Hit::ChooseFolder, self.config.save_dir.clone()),
            None => {}
        }
    }

    fn pick_settings_folder(&mut self, hit: Hit, start: PathBuf) {
        let Some(s) = &self.settings else { return };
        let (window, proxy) = (s.id(), self.proxy.clone());
        dialogs::in_background(window.into(), hwnd_of(&s.window), move |owner| {
            let res = dialogs::pick_folder(owner, &start).map_err(|e| e.to_string());
            let _ = proxy.send_event(UserEvent::SettingsFolder(window, hit, res));
        });
    }

    pub(crate) fn settings_folder_chosen(&mut self, window: WindowId, hit: Hit, res: Result<Option<PathBuf>, String>) {
        if !self.settings.as_ref().is_some_and(|s| s.id() == window) {
            return;
        }
        let dir = match res {
            Ok(Some(dir)) => dir,
            Ok(None) => return,
            Err(e) => return self.settings_message(&format!("Couldn't open the folder picker: {e}"), true),
        };
        if hit == Hit::ConfigDir {
            match config::set_config_dir(Some(&dir), &self.config) {
                Ok(()) => self.settings_message("Settings now live in that folder", false),
                Err(e) => self.settings_message(&format!("Couldn't use that folder: {e}"), true),
            }
        } else {
            self.config.save_dir = dir;
            if self.save_config() {
                self.settings_message("Save folder updated", false);
            }
        }
    }

    fn end_quality_drag(&mut self) {
        let Some(s) = self.settings.as_mut().filter(|s| s.drag_quality) else { return };
        s.drag_quality = false;
        s.window.request_redraw();
        self.save_config();
    }

    pub(crate) fn close_settings(&mut self) {
        crate::menu::dismiss();
        self.end_quality_drag();
        self.commit_name();
        self.stop_listening();
        if let Some(s) = self.settings.as_mut() {
            s.finish_opening();
            let window = s.window.clone();
            crate::settings::dismiss(&window, |t| s.present_closing(t));
        }
        self.settings = None;
        crate::mem::trim();
    }

    pub(crate) fn settings_event(&mut self, ev: WindowEvent) {
        if matches!(&ev, WindowEvent::MouseInput { state: ElementState::Pressed, .. } | WindowEvent::KeyboardInput { .. } | WindowEvent::MouseWheel { .. })
            && self.settings.as_ref().is_some_and(|s| s.opening.is_some())
        {
            if let Some(s) = self.settings.as_mut() {
                s.finish_opening();
            }
            self.draw_settings();
        }
        match ev {
            WindowEvent::CloseRequested => self.close_settings(),
            WindowEvent::RedrawRequested if self.settings.as_mut().is_some_and(|s| !s.frames.allow() && { s.note_exposed(); true }) => {}
            WindowEvent::RedrawRequested | WindowEvent::Resized(_) => self.draw_settings(),
            WindowEvent::ModifiersChanged(m) => {
                if let Some(s) = self.settings.as_mut() {
                    s.mods = m.state();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(s) = self.settings.as_mut() {
                    s.cursor = (position.x as f32, position.y as f32);
                    if s.drag_quality {
                        self.config.quality = s.quality_at(s.cursor.0);
                        s.window.request_redraw();
                    }
                    if s.track_row() {
                        s.window.request_redraw();
                    }
                    let hit = s.hit(s.cursor.0, s.cursor.1);
                    if hit != s.hover {
                        s.hover = hit;
                        let icon = match hit {
                            Some(Hit::FileName) => CursorIcon::Text,
                            Some(_) => CursorIcon::Pointer,
                            None => CursorIcon::Default,
                        };
                        s.window.set_cursor(icon);
                        s.window.request_redraw();
                    }
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(s) = self.settings.as_mut() {
                    let (a, b) = (s.hover.take().is_some(), s.leave_rows());
                    if a || b {
                        s.window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                if let Some(s) = self.settings.as_ref()
                    && s.in_title_bar(s.cursor.1)
                {
                    let _ = s.window.drag_window();
                    self.revive_frames();
                    return;
                }
                self.settings_click();
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => self.end_quality_drag(),
            WindowEvent::Focused(false) => {
                self.stop_listening();
                self.commit_name();
                self.end_quality_drag();
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let listening = self.settings.as_ref().and_then(|s| s.listening);
                let editing = self.settings.as_ref().is_some_and(|s| s.editing_name);
                if let Some(action) = listening {
                    self.capture_shortcut(action, &event);
                } else if editing {
                    match &event.logical_key {
                        Key::Named(NamedKey::Enter) => self.commit_name(),
                        Key::Named(NamedKey::Escape) => self.cancel_name(),
                        Key::Named(NamedKey::Backspace) => {
                            if let Some(s) = self.settings.as_mut() {
                                use unicode_segmentation::UnicodeSegmentation;
                                if let Some((i, _)) = s.draft.grapheme_indices(true).next_back() {
                                    s.draft.truncate(i);
                                }
                                s.window.request_redraw();
                            }
                        }
                        _ => {}
                    }
                } else if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
                    self.close_settings();
                }
            }
            _ => {}
        }
    }
}
