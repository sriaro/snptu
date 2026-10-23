
use std::path::PathBuf;

use crate::convert::{IfExists, Kind, Params, PngLevel, ResizeFilter};

#[derive(Clone, Debug, PartialEq)]
pub struct ConvertPrefs {
    pub params: Params,
    pub anim: Params,
    pub threads: usize,
    pub template: String,
    pub exists: IfExists,
    pub resize_on: bool,
    pub resize_mode: u8,
    pub percent: String,
    pub width: String,
    pub height: String,
    pub filter: ResizeFilter,
    pub out_dir: Option<PathBuf>,
}

impl Default for ConvertPrefs {
    fn default() -> Self {
        Self {
            params: Params::new(Kind::Webp),
            anim: Params { animation: true, ..Params::new(Kind::Webp) },
            threads: crate::par::workers(),
            template: "{name}".into(),
            exists: IfExists::Rename,
            resize_on: false,
            resize_mode: 0,
            percent: "50".into(),
            width: "1920".into(),
            height: "1080".into(),
            filter: ResizeFilter::Lanczos,
            out_dir: None,
        }
    }
}

const PNG: [(&str, PngLevel); 3] = [("fast", PngLevel::Fast), ("default", PngLevel::Default), ("best", PngLevel::Best)];
const EXISTS: [(&str, IfExists); 3] = [("rename", IfExists::Rename), ("overwrite", IfExists::Overwrite), ("skip", IfExists::Skip)];
const FILTER: [(&str, ResizeFilter); 4] = [
    ("lanczos", ResizeFilter::Lanczos),
    ("catmull-rom", ResizeFilter::CatmullRom),
    ("bilinear", ResizeFilter::Bilinear),
    ("nearest", ResizeFilter::Nearest),
];

fn name_of<T: PartialEq + Copy>(table: &[(&'static str, T)], v: T) -> &'static str {
    table.iter().find(|(_, t)| *t == v).map_or(table[0].0, |(n, _)| n)
}

fn by_name<T: Copy>(table: &[(&str, T)], v: &str) -> Option<T> {
    table.iter().find(|(n, _)| n.eq_ignore_ascii_case(v.trim())).map(|(_, t)| *t)
}

fn flag(v: &str) -> Option<bool> {
    match v.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

fn number(v: &str) -> Option<String> {
    let v = v.trim();
    (v.len() <= 10 && v.parse::<f32>().is_ok_and(|n| (1.0..=65535.0).contains(&n))).then(|| v.to_string())
}

impl ConvertPrefs {
    pub fn parse_line(&mut self, line: &str) -> bool {
        let Some((key, v)) = line.strip_prefix("convert_").and_then(|r| r.split_once('=')) else { return false };
        let (key, p) = match key.strip_prefix("anim_") {
            Some(k) => (k, &mut self.anim),
            None => (key, &mut self.params),
        };
        let animation = p.animation;
        let allowed: &[Kind] = if animation { &Kind::ANIMATIONS } else { &Kind::IMAGES };
        match key {
            "format" => {
                if let Some(k) = Kind::from_extension(v.trim()).filter(|k| allowed.contains(k)) {
                    *p = Params { animation, ..Params::new(k) };
                }
            }
            "lossless" => p.lossless = flag(v).unwrap_or(p.lossless),
            "quality" => p.quality = v.trim().parse::<u32>().map_or(p.quality, |q| q.clamp(1, 100) as u8),
            "effort" => p.effort = v.trim().parse::<u32>().map_or(p.effort, |e| e.min(6) as u8),
            "png_level" => p.png_level = by_name(&PNG, v).unwrap_or(p.png_level),
            "deep" => p.deep = flag(v).unwrap_or(p.deep),
            "keep_profile" => p.keep_profile = flag(v).unwrap_or(p.keep_profile),
            "gif_speed" => p.gif_speed = v.trim().parse::<u32>().map_or(p.gif_speed, |s| s.clamp(1, 30) as u8),
            "threads" => {
                let max = std::thread::available_parallelism().map_or(4, |n| n.get());
                self.threads = v.trim().parse::<usize>().map_or(self.threads, |t| t.clamp(1, max));
            }
            "template" => {
                let t = v.trim();
                if !t.is_empty() && t.chars().count() <= 120 {
                    self.template = t.to_string();
                }
            }
            "exists" => self.exists = by_name(&EXISTS, v).unwrap_or(self.exists),
            "resize" => self.resize_on = flag(v).unwrap_or(self.resize_on),
            "resize_mode" => self.resize_mode = v.trim().parse::<u32>().map_or(self.resize_mode, |m| m.min(2) as u8),
            "percent" => self.percent = number(v).unwrap_or_else(|| self.percent.clone()),
            "width" => self.width = number(v).unwrap_or_else(|| self.width.clone()),
            "height" => self.height = number(v).unwrap_or_else(|| self.height.clone()),
            "filter" => self.filter = by_name(&FILTER, v).unwrap_or(self.filter),
            "folder" => {
                let dir = PathBuf::from(v.trim());
                self.out_dir = (dir.is_absolute() && !v.contains('\u{FFFD}')).then_some(dir);
            }
            _ => return false,
        }
        true
    }

    pub fn lines(&self) -> String {
        let p = &self.params;
        let a = &self.anim;
        let b = |v: bool| if v { 1 } else { 0 };
        format!(
            "convert_format={}\nconvert_lossless={}\nconvert_quality={}\nconvert_effort={}\nconvert_png_level={}\nconvert_deep={}\nconvert_keep_profile={}\nconvert_gif_speed={}\nconvert_anim_format={}\nconvert_anim_lossless={}\nconvert_anim_quality={}\nconvert_anim_effort={}\nconvert_anim_gif_speed={}\nconvert_threads={}\nconvert_template={}\nconvert_exists={}\nconvert_resize={}\nconvert_resize_mode={}\nconvert_percent={}\nconvert_width={}\nconvert_height={}\nconvert_filter={}\nconvert_folder={}\n",
            p.kind.ext(),
            b(p.lossless),
            p.quality,
            p.effort,
            name_of(&PNG, p.png_level),
            b(p.deep),
            b(p.keep_profile),
            p.gif_speed,
            a.kind.ext(),
            b(a.lossless),
            a.quality,
            a.effort,
            a.gif_speed,
            self.threads,
            self.template,
            name_of(&EXISTS, self.exists),
            b(self.resize_on),
            self.resize_mode,
            self.percent,
            self.width,
            self.height,
            name_of(&FILTER, self.filter),
            self.out_dir.as_ref().map_or_else(String::new, |d| d.display().to_string()),
        )
    }
}
