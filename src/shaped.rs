
use cosmic_text::{Attrs, Buffer, Color, FontSystem, Metrics, Shaping, SwashCache, Weight};
use tiny_skia::Pixmap;

const CACHE_ENTRIES: usize = 4;

pub const MAX_CHARS: usize = 2000;

struct Entry {
    text: String,
    size_bits: u32,
    bold: bool,
    buffer: Buffer,
    width: f32,
}

pub struct Shaper {
    fs: FontSystem,
    swash: SwashCache,
    entries: Vec<Entry>,
    next_victim: usize,
}

impl Shaper {
    pub fn new(fs: FontSystem) -> Self {
        Self { fs, swash: SwashCache::new(), entries: Vec::with_capacity(CACHE_ENTRIES), next_victim: 0 }
    }

    fn entry(&mut self, text: &str, size: f32, bold: bool) -> usize {
        let text = text.char_indices().nth(MAX_CHARS).map_or(text, |(i, _)| &text[..i]);
        let bits = size.to_bits();
        if let Some(i) = self.entries.iter().position(|e| e.size_bits == bits && e.bold == bold && e.text == text) {
            return i;
        }
        let mut b = Buffer::new(&mut self.fs, Metrics::new(size, (size * 1.3).ceil()));
        b.set_size(None, None);
        let attrs = if bold { Attrs::new().weight(Weight::BOLD) } else { Attrs::new() };
        b.set_text(text, &attrs, Shaping::Advanced, None);
        b.shape_until_scroll(&mut self.fs, false);
        let width = b.layout_runs().map(|r| r.line_w).fold(0.0, f32::max);
        let e = Entry { text: text.to_string(), size_bits: bits, bold, buffer: b, width };
        if self.entries.len() < CACHE_ENTRIES {
            self.entries.push(e);
            self.entries.len() - 1
        } else {
            let i = self.next_victim;
            self.next_victim = (i + 1) % CACHE_ENTRIES;
            self.entries[i] = e;
            i
        }
    }

    pub fn width(&mut self, text: &str, size: f32, bold: bool) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let i = self.entry(text, size, bold);
        self.entries[i].width
    }

    pub fn draw(&mut self, pix: &mut Pixmap, text: &str, (x, y): (f32, f32), size: f32, (color, alpha): ([u8; 3], u8), bold: bool) {
        if text.is_empty() {
            return;
        }
        let i = self.entry(text, size, bold);
        let (pw, ph) = (pix.width() as i32, pix.height() as i32);
        let (ox, oy) = (x.round() as i32, y.round() as i32);
        let data = pix.data_mut();
        let (fs, swash, buffer) = (&mut self.fs, &mut self.swash, &mut self.entries[i].buffer);
        buffer.draw(fs, swash, Color::rgba(color[0], color[1], color[2], alpha), |gx, gy, gw, gh, c| {
            let ca = c.a() as u32;
            if ca == 0 {
                return;
            }
            let inv = 255 - ca;
            for yy in (oy + gy).max(0)..(oy + gy + gh as i32).min(ph) {
                for xx in (ox + gx).max(0)..(ox + gx + gw as i32).min(pw) {
                    let i = ((yy * pw + xx) * 4) as usize;
                    data[i] = ((c.r() as u32 * ca + data[i] as u32 * inv) / 255) as u8;
                    data[i + 1] = ((c.g() as u32 * ca + data[i + 1] as u32 * inv) / 255) as u8;
                    data[i + 2] = ((c.b() as u32 * ca + data[i + 2] as u32 * inv) / 255) as u8;
                    data[i + 3] = (ca + data[i + 3] as u32 * inv / 255) as u8;
                }
            }
        });
    }
}
