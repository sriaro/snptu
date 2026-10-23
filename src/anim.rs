
use crate::convert::{Kind, Params, ResizeFilter, ResizeMode, check_size};
use anyhow::{Context, Result, anyhow, bail};
use std::{
    io::{BufReader, Seek, SeekFrom, Write},
    path::Path,
};

const MAX_FRAMES: usize = 4000;
const MAX_BYTES: u64 = 512 << 20;
const CANVASES: u64 = 4;
const MAX_OUTPUT: u64 = 2 << 30;
const MAX_MS: u32 = 655_350;

fn shown_ms(ms: u32) -> u32 {
    if ms <= 10 { 100 } else { ms.min(MAX_MS) }
}

pub(crate) fn check(w: u32, h: u32, frames: usize) -> Result<()> {
    check_size(w, h)?;
    if w as u64 * h as u64 * 4 * CANVASES > MAX_BYTES {
        bail!("the animation is too large to convert ({w} x {h})");
    }
    if frames > MAX_FRAMES {
        bail!("the animation is too large to convert ({frames} frames of {w} x {h})");
    }
    Ok(())
}

pub trait Sink {
    fn start(&mut self, w: u32, h: u32, plays: u32) -> Result<()>;
    fn frame(&mut self, rgba: Vec<u8>, ms: u32) -> Result<()>;
}

struct Held<'s> {
    sink: &'s mut dyn Sink,
    w: u32,
    h: u32,
    plays: u32,
    first: Option<(Vec<u8>, u32)>,
    count: usize,
}

impl<'s> Held<'s> {
    fn new(sink: &'s mut dyn Sink, w: u32, h: u32, plays: u32) -> Self {
        Self { sink, w, h, plays, first: None, count: 0 }
    }

    fn push(&mut self, rgba: Vec<u8>, ms: u32) -> Result<()> {
        self.count += 1;
        if self.count == 1 {
            self.first = Some((rgba, ms));
            return Ok(());
        }
        check(self.w, self.h, self.count)?;
        if let Some((first, first_ms)) = self.first.take() {
            self.sink.start(self.w, self.h, self.plays)?;
            self.sink.frame(first, first_ms)?;
        }
        self.sink.frame(rgba, ms)
    }

    fn animated(&self) -> bool {
        self.count > 1
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sniff {
    Gif,
    Webp,
    Avif,
}

fn sniff(path: &Path) -> Option<Sniff> {
    use std::io::Read;
    let mut head = [0u8; 16];
    let n = std::fs::File::open(path).ok()?.read(&mut head).ok()?;
    let head = &head[..n];
    if head.starts_with(b"GIF8") {
        Some(Sniff::Gif)
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some(Sniff::Webp)
    } else if head.len() >= 12 && &head[4..8] == b"ftyp" && matches!(&head[8..12], b"avif" | b"avis") {
        Some(Sniff::Avif)
    } else {
        None
    }
}

pub fn is_animated(path: &Path) -> bool {
    std::fs::File::open(path).is_ok_and(|f| animated_in(&mut BufReader::new(f)))
}

pub fn animated_in<R: std::io::BufRead + std::io::Seek>(r: &mut R) -> bool {
    let mut head = [0u8; 32];
    let Ok(n) = r.read(&mut head) else { return false };
    let head = &head[..n];
    if head.starts_with(b"GIF8") {
        r.seek_relative(-(n as i64)).is_ok() && gif_has_two_pictures(r)
    } else if head.len() >= 21 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        &head[12..16] == b"VP8X" && head[20] & 0x02 != 0
    } else {
        head.len() >= 12 && &head[4..8] == b"ftyp" && &head[8..12] == b"avis"
    }
}

pub fn canvas_size(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 30];
    let n = f.read(&mut head).ok()?;
    let head = &head[..n];
    if head.len() >= 10 && head.starts_with(b"GIF8") {
        let side = |i: usize| u16::from_le_bytes([head[i], head[i + 1]]) as u32;
        Some((side(6), side(8)))
    } else if head.len() >= 30 && &head[..4] == b"RIFF" && &head[8..16] == b"WEBPVP8X" {
        let side = |i: usize| u32::from_le_bytes([head[i], head[i + 1], head[i + 2], 0]) + 1;
        Some((side(24), side(27)))
    } else if head.len() >= 12 && &head[4..8] == b"ftyp" && matches!(&head[8..12], b"avif" | b"avis") {
        avif_canvas(&mut BufReader::new(f))
    } else {
        None
    }
}

pub(crate) fn webp_size(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 64];
    let n = f.read(&mut head).ok()?;
    let head = &head[..n];
    if head.len() < 20 || &head[..4] != b"RIFF" || &head[8..12] != b"WEBP" {
        return None;
    }
    let size = |i: usize| u32::from_le_bytes([head[i + 4], head[i + 5], head[i + 6], head[i + 7]]) as usize;
    let body_end = 20usize.saturating_add(size(12)).min(head.len());
    match &head[12..16] {
        b"VP8X" if head.len() >= 30 && size(12) >= 10 => {
            let side = |i: usize| u32::from_le_bytes([head[i], head[i + 1], head[i + 2], 0]) + 1;
            Some((side(24), side(27)))
        }
        b"VP8 " => webp_picture_size(head.get(20..body_end)?, true),
        b"VP8L" => webp_picture_size(head.get(20..body_end)?, false),
        _ => None,
    }
}
const AVIF_PATH: [&[u8; 4]; 6] = [b"moov", b"trak", b"mdia", b"minf", b"stbl", b"stsd"];

fn avif_canvas<R: std::io::Read + Seek>(r: &mut R) -> Option<(u32, u32)> {
    let end = r.seek(SeekFrom::End(0)).ok()?;
    r.seek(SeekFrom::Start(0)).ok()?;
    let (mut boxes, mut largest) = (0, None);
    avif_boxes(r, end, 0, &mut boxes, &mut largest);
    largest
}

fn avif_boxes<R: std::io::Read + Seek>(r: &mut R, end: u64, depth: usize, boxes: &mut u32, largest: &mut Option<(u32, u32)>) -> Option<()> {
    let mut at = r.stream_position().ok()?;
    while at + 8 <= end {
        *boxes += 1;
        if *boxes > 10_000 {
            return None;
        }
        let mut head = [0u8; 8];
        r.read_exact(&mut head).ok()?;
        let (len, body) = match u32::from_be_bytes([head[0], head[1], head[2], head[3]]) {
            0 => (end - at, at + 8),
            1 => {
                let mut long = [0u8; 8];
                r.read_exact(&mut long).ok()?;
                (u64::from_be_bytes(long), at + 16)
            }
            n => (n as u64, at + 8),
        };
        let next = at.saturating_add(len).min(end);
        if next < body {
            return None;
        }
        let kind = &head[4..8];
        if depth < AVIF_PATH.len() && kind == AVIF_PATH[depth] {
            let first = if depth == AVIF_PATH.len() - 1 { body + 8 } else { body };
            r.seek(SeekFrom::Start(first)).ok()?;
            avif_boxes(r, next, depth + 1, boxes, largest)?;
        } else if depth == AVIF_PATH.len() && kind == b"av01" && body + 28 <= next {
            r.seek(SeekFrom::Start(body + 24)).ok()?;
            let mut size = [0u8; 4];
            r.read_exact(&mut size).ok()?;
            let size = (u16::from_be_bytes([size[0], size[1]]) as u32, u16::from_be_bytes([size[2], size[3]]) as u32);
            let area = |(w, h): (u32, u32)| w as u64 * h as u64;
            if area(size) > largest.map_or(0, area) {
                *largest = Some(size);
            }
        }
        r.seek(SeekFrom::Start(next)).ok()?;
        at = next;
    }
    Some(())
}

const GIF_BLOCK_BUDGET: u32 = 1_000_000;

const GIF_SCAN_BYTES: u64 = 16 << 20;

#[derive(Clone, Copy)]
struct Budget {
    blocks: u32,
    bytes: u64,
}

impl Budget {
    const DECODE: Budget = Budget { blocks: GIF_BLOCK_BUDGET, bytes: u64::MAX };
    const SCAN: Budget = Budget { blocks: GIF_BLOCK_BUDGET, bytes: GIF_SCAN_BYTES };

    fn bytes(&mut self, n: u64) -> bool {
        if self.spent() || self.bytes < n {
            *self = Budget { blocks: 0, bytes: 0 };
            return false;
        }
        self.bytes -= n;
        true
    }

    fn block(&mut self, n: u64) -> bool {
        let ok = self.bytes(n);
        if ok {
            self.blocks -= 1;
        }
        ok
    }

    fn spent(&self) -> bool {
        self.blocks == 0 || self.bytes == 0
    }
}

fn skip_sub_blocks<R: std::io::Read + std::io::Seek>(r: &mut R, budget: &mut Budget) -> bool {
    loop {
        if !budget.block(1) {
            return false;
        }
        let mut n = [0u8; 1];
        if r.read_exact(&mut n).is_err() {
            return false;
        }
        if n[0] == 0 {
            return true;
        }
        if !budget.bytes(n[0] as u64) || r.seek_relative(n[0] as i64).is_err() {
            return false;
        }
    }
}

fn skip_image_data<R: std::io::Read + std::io::Seek>(r: &mut R, budget: &mut Budget) -> bool {
    if !budget.bytes(2) {
        return false;
    }
    let mut head = [0u8; 2];
    r.read_exact(&mut head).is_ok() && budget.bytes(head[1] as u64) && r.seek_relative(head[1] as i64).is_ok() && skip_sub_blocks(r, budget)
}

fn gif_has_two_pictures<R: std::io::Read + std::io::Seek>(r: &mut R) -> bool {
    let mut budget = Budget::SCAN;
    gif_pictures_in(r, 2, &mut budget) == 2 || budget.spent()
}

fn gif_pictures<R: std::io::Read + std::io::Seek>(r: &mut R, limit: usize) -> (usize, bool) {
    let mut budget = Budget::DECODE;
    let pictures = gif_pictures_in(r, limit, &mut budget);
    (pictures, !budget.spent())
}

fn gif_pictures_in<R: std::io::Read + std::io::Seek>(r: &mut R, limit: usize, budget: &mut Budget) -> usize {
    let mut screen = [0u8; 13];
    if r.read_exact(&mut screen).is_err() {
        return 0;
    }
    let table = |flags: u8| if flags & 0x80 != 0 { 3 << ((flags & 7) + 1) } else { 0 };
    if !budget.bytes(13 + table(screen[10]) as u64) || r.seek_relative(table(screen[10])).is_err() {
        return 0;
    }
    let mut pictures = 0;
    while pictures < limit && !budget.spent() {
        let mut block = [0u8; 1];
        if r.read_exact(&mut block).is_err() {
            break;
        }
        match block[0] {
            0x21 => {
                let mut label = [0u8; 1];
                if !budget.bytes(2) || r.read_exact(&mut label).is_err() || !skip_sub_blocks(r, budget) {
                    break;
                }
            }
            0x2C => {
                let mut d = [0u8; 9];
                if r.read_exact(&mut d).is_err() {
                    break;
                }
                if !budget.bytes(10 + table(d[8]) as u64) || r.seek_relative(table(d[8])).is_err() {
                    break;
                }
                if !skip_image_data(r, budget) {
                    break;
                }
                pictures += 1;
            }
            _ => break,
        }
    }
    pictures
}

fn gif_plays<R: std::io::Read + std::io::Seek>(r: &mut R) -> u32 {
    let mut budget = Budget::DECODE;
    gif_plays_in(r, &mut budget)
}

fn gif_plays_in<R: std::io::Read + std::io::Seek>(r: &mut R, budget: &mut Budget) -> u32 {
    let mut screen = [0u8; 13];
    if r.read_exact(&mut screen).is_err() || (screen[10] & 0x80 != 0 && r.seek_relative(3 << ((screen[10] & 7) + 1)).is_err()) {
        return 1;
    }
    loop {
        if budget.spent() {
            return 1;
        }
        let mut block = [0u8; 2];
        if r.read_exact(&mut block[..1]).is_err() {
            return 1;
        }
        match block[0] {
            0x21 => {
                if r.read_exact(&mut block[1..]).is_err() {
                    return 1;
                }
                if block[1] == 0xFF {
                    let mut n = [0u8; 1];
                    if r.read_exact(&mut n).is_err() {
                        return 1;
                    }
                    let mut id = vec![0u8; n[0] as usize];
                    if r.read_exact(&mut id).is_err() {
                        return 1;
                    }
                    if matches!(&id[..], b"NETSCAPE2.0" | b"ANIMEXTS1.0") {
                        let mut sub = [0u8; 4];
                        if r.read_exact(&mut sub).is_err() || sub[0] < 3 || sub[1] != 1 {
                            return 1;
                        }
                        let n = u16::from_le_bytes([sub[2], sub[3]]) as u32;
                        return if n == 0 { 0 } else { n + 1 };
                    }
                }
                if !skip_sub_blocks(r, budget) {
                    return 1;
                }
            }
            0x2C => {
                let mut d = [0u8; 9];
                if r.read_exact(&mut d).is_err() || (d[8] & 0x80 != 0 && r.seek_relative(3 << ((d[8] & 7) + 1)).is_err()) {
                    return 1;
                }
                if !skip_image_data(r, budget) {
                    return 1;
                }
            }
            _ => return 1,
        }
    }
}

pub fn decode(path: &Path, sink: &mut dyn Sink, stop: &Stop) -> Result<bool> {
    if sniff(path).is_none() {
        return Ok(false);
    }
    crate::convert::decode_animation(path, sink, stop)
}

pub(crate) fn decode_here(path: &Path, sink: &mut dyn Sink) -> Result<bool> {
    let read = || std::fs::read(path).with_context(|| format!("can't read {}", path.display()));
    match sniff(path) {
        Some(Sniff::Gif) => gif(path, sink),
        Some(Sniff::Webp) => webp(&read()?, sink),
        Some(Sniff::Avif) => avif_in_process(&read()?, sink),
        None => Ok(false),
    }
}

fn gif(path: &Path, sink: &mut dyn Sink) -> Result<bool> {
    let len = std::fs::metadata(path)?.len();
    if len > 512 << 20 {
        bail!("the file is too large to open ({} MB)", len >> 20);
    }
    gif_walked(path, sink, gif_pictures(&mut BufReader::new(std::fs::File::open(path)?), MAX_FRAMES + 1))
}

fn gif_walked(path: &Path, sink: &mut dyn Sink, (pictures, whole): (usize, bool)) -> Result<bool> {
    use image::{AnimationDecoder, ImageDecoder, codecs::gif::GifDecoder};
    let mut decoder = GifDecoder::new(BufReader::new(std::fs::File::open(path)?)).map_err(|e| anyhow!("GIF: {e}"))?;
    let (w, h) = decoder.dimensions();
    check_size(w, h)?;
    let mut limits = image::Limits::no_limits();
    limits.max_alloc = Some(w as u64 * h as u64 * 4 * 3 + (16 << 20));
    decoder.set_limits(limits).map_err(|e| anyhow!("GIF: {e}"))?;
    if pictures < 2 && whole {
        return Ok(false);
    }
    if pictures >= 2 {
        check(w, h, pictures)?;
    }
    let plays = gif_plays(&mut BufReader::new(std::fs::File::open(path)?));
    let mut held = Held::new(sink, w, h, plays);
    for frame in decoder.into_frames() {
        let frame = frame.map_err(|e| match e {
            image::ImageError::Limits(_) => anyhow!("GIF: a picture is far larger than the {w} x {h} canvas"),
            e => anyhow!("GIF: {e}"),
        })?;
        let (n, d) = frame.delay().numer_denom_ms();
        let ms = shown_ms((n as f64 / d.max(1) as f64).round() as u32);
        let buf = frame.into_buffer();
        if buf.dimensions() != (w, h) {
            bail!("GIF: frame size differs from the canvas");
        }
        held.push(buf.into_raw(), ms)?;
    }
    Ok(held.animated())
}

fn webp(bytes: &[u8], sink: &mut dyn Sink) -> Result<bool> {
    let Some((w, h, n)) = webp_layout(bytes)? else { return Ok(false) };
    if n < 2 {
        return Ok(false);
    }
    check(w, h, n)?;
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| webp_frames(bytes, sink))).unwrap_or_else(|_| Err(anyhow!("WebP: the file is damaged")))
}

pub(crate) fn webp_layout(bytes: &[u8]) -> Result<Option<(u32, u32, usize)>> {
    if bytes.len() < 30 || &bytes[..4] != b"RIFF" || &bytes[8..16] != b"WEBPVP8X" {
        return Ok(None);
    }
    let size_at = |i: usize| u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]) as usize;
    let u24 = |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], 0]);
    let vp8x = size_at(12);
    if vp8x < 10 {
        return Ok(None);
    }
    let end = bytes.len().min(8 + size_at(0));
    let mut at = 20 + vp8x + vp8x % 2;
    if at > end {
        return Ok(None);
    }
    let (cw, ch) = (u24(24) + 1, u24(27) + 1);
    let mut frames = 0;
    while at + 8 <= end {
        let (body, size) = (at + 8, size_at(at));
        let next = body + size + size % 2;
        if next > bytes.len() {
            if matches!(&bytes[at..at + 4], b"ANIM" | b"ANMF" | b"VP8 " | b"VP8L") {
                bail!("WebP: the file is damaged");
            }
            break;
        }
        match &bytes[at..at + 4] {
            b"VP8 " | b"VP8L" => {
                let (w, h) = webp_picture_size(&bytes[body..body + size], &bytes[at..at + 4] == b"VP8 ").ok_or_else(|| anyhow!("WebP: the picture header is damaged"))?;
                if (w, h) != (cw, ch) {
                    bail!("WebP: a picture is {w} x {h} but the canvas is {cw} x {ch}");
                }
            }
            b"ANMF" => {
                frames += 1;
                if size >= 16 {
                    let (fx, fy) = (2 * u24(body), 2 * u24(body + 3));
                    let (fw, fh) = (u24(body + 6) + 1, u24(body + 9) + 1);
                    if fx + fw > cw || fy + fh > ch {
                        bail!("WebP: frame {frames} lies outside the canvas");
                    }
                    let (mut sub, sub_end) = (body + 16, body + size);
                    let mut picture = None;
                    while sub + 8 <= sub_end {
                        let n = size_at(sub);
                        let next = sub + 8 + n + n % 2;
                        if next > sub_end {
                            bail!("WebP: frame {frames} is damaged");
                        }
                        match &bytes[sub..sub + 4] {
                            b"ALPH" => {}
                            b"VP8 " | b"VP8L" => {
                                picture = webp_picture_size(&bytes[sub + 8..sub + 8 + n], &bytes[sub..sub + 4] == b"VP8 ");
                                break;
                            }
                            _ => {}
                        }
                        sub = next;
                    }
                    let Some((pw, ph)) = picture else { bail!("WebP: frame {frames} has no picture") };
                    if (pw, ph) != (fw, fh) {
                        bail!("WebP: frame {frames} is {fw} x {fh} but its picture is {pw} x {ph}");
                    }
                }
            }
            _ => {}
        }
        at = next;
    }
    Ok(Some((cw, ch, frames)))
}

fn webp_picture_size(bits: &[u8], lossy: bool) -> Option<(u32, u32)> {
    if lossy {
        let b = bits.get(..10)?;
        let side = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]) as u32 & 0x3FFF;
        (b[3..6] == [0x9D, 0x01, 0x2A]).then(|| (side(6), side(8)))
    } else {
        let b = bits.get(..5)?;
        let v = u32::from_le_bytes([b[1], b[2], b[3], b[4]]);
        (b[0] == 0x2F).then_some(((v & 0x3FFF) + 1, ((v >> 14) & 0x3FFF) + 1))
    }
}

fn webp_frames(bytes: &[u8], sink: &mut dyn Sink) -> Result<bool> {
    use zenwebp::{
        decoder::LoopCount,
        mux::{BlendMethod, DisposeMethod, WebPDemuxer},
    };
    let Ok(demux) = WebPDemuxer::new(bytes) else { return Ok(false) };
    if !demux.is_animated() {
        return Ok(false);
    }
    let (w, h) = (demux.canvas_width(), demux.canvas_height());
    let n = demux.frame_count() as usize;
    check(w, h, n)?;
    let plays = match demux.loop_count() {
        LoopCount::Forever => 0,
        LoopCount::Times(n) => n.get() as u32,
    };
    let (cw, ch) = (w as usize, h as usize);
    let mut canvas = vec![0u8; cw * ch * 4];
    let mut held = Held::new(sink, w, h, plays);
    let mut clear: Option<[usize; 4]> = None;
    for f in demux.frames() {
        if let Some([x0, y0, x1, y1]) = clear.take() {
            for y in y0..y1 {
                canvas[(y * cw + x0) * 4..(y * cw + x1) * 4].fill(0);
            }
        }
        if f.x_offset as u64 + f.width as u64 > w as u64 || f.y_offset as u64 + f.height as u64 > h as u64 {
            bail!("WebP: frame {} of {n} is damaged", held.count + 1);
        }
        if webp_picture_size(f.bitstream, f.is_lossy).is_some_and(|s| s != (f.width, f.height)) {
            bail!("WebP: a frame differs from its header");
        }
        let (px, fw, fh) = zenwebp::oneshot::decode_rgba(&still_webp(&f)).map_err(|e| anyhow!("WebP: {e}"))?;
        if (fw, fh) != (f.width, f.height) || px.len() != fw as usize * fh as usize * 4 {
            bail!("WebP: a frame differs from its header");
        }
        let (x0, y0) = (f.x_offset as usize, f.y_offset as usize);
        let (x1, y1) = (x0 + fw as usize, y0 + fh as usize);
        for y in y0..y1 {
            let src = &px[(y - y0) * fw as usize * 4..][..(x1 - x0) * 4];
            let dst = &mut canvas[(y * cw + x0) * 4..(y * cw + x1) * 4];
            match f.blend {
                BlendMethod::Overwrite => dst.copy_from_slice(src),
                BlendMethod::AlphaBlend => dst.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0).for_each(|(d, s)| blend_over(d, s)),
            }
        }
        held.push(canvas.clone(), shown_ms(f.duration_ms))?;
        if f.dispose == DisposeMethod::Background {
            clear = Some([x0, y0, x1, y1]);
        }
    }
    if held.count != n {
        bail!("WebP: frame {} of {n} is damaged", held.count + 1);
    }
    Ok(held.animated())
}

fn still_webp(f: &zenwebp::mux::DemuxFrame) -> Vec<u8> {
    let chunk = |out: &mut Vec<u8>, tag: &[u8], data: &[u8]| {
        out.extend_from_slice(tag);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(0);
        }
    };
    let mut body = b"WEBP".to_vec();
    if let Some(alpha) = f.alpha_data {
        let mut x = vec![0x10, 0, 0, 0];
        x.extend_from_slice(&(f.width - 1).to_le_bytes()[..3]);
        x.extend_from_slice(&(f.height - 1).to_le_bytes()[..3]);
        chunk(&mut body, b"VP8X", &x);
        chunk(&mut body, b"ALPH", alpha);
    }
    let at = body.len() + 8;
    chunk(&mut body, if f.is_lossy { b"VP8 " } else { b"VP8L" }, f.bitstream);
    if !f.is_lossy && f.bitstream.len() >= 5 {
        body[at + 4] |= 0x10;
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

fn blend_over(d: &mut [u8], s: &[u8]) {
    let (sa, da) = (s[3] as u32, d[3] as u32);
    if sa == 255 || da == 0 {
        d.copy_from_slice(s);
        return;
    }
    if sa == 0 {
        return;
    }
    let dw = da * (255 - sa);
    let a = sa * 255 + dw;
    for k in 0..3 {
        d[k] = ((s[k] as u32 * sa * 255 + d[k] as u32 * dw + a / 2) / a) as u8;
    }
    d[3] = ((a + 127) / 255) as u8;
}

fn avif_in_process(bytes: &[u8], sink: &mut dyn Sink) -> Result<bool> {
    let config = zenavif::DecoderConfig::new().threads(1).frame_size_limit(crate::convert::MAX_DECODE_PIXELS.min(u32::MAX as u64) as u32);
    let Ok(mut dec) = zenavif::AnimationDecoder::new(bytes, &config) else { return Ok(false) };
    let n = dec.info().frame_count;
    if n < 2 {
        return Ok(false);
    }
    let stated = avif_canvas(&mut std::io::Cursor::new(bytes));
    avif_frames(n, stated, sink, || {
        Ok(match dec.next_frame(&zenavif::Unstoppable) {
            Ok(Some(f)) => {
                let (px, w, h) = crate::convert::pixels_to_rgba8(&f.pixels)?;
                Ok(Some((px, w, h, f.duration_ms)))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(e.to_string()),
        })
    })
}

type AvifFrame = std::result::Result<Option<(Vec<u8>, u32, u32, u32)>, String>;

fn avif_frames(n: usize, stated: Option<(u32, u32)>, sink: &mut dyn Sink, mut next: impl FnMut() -> Result<AvifFrame>) -> Result<bool> {
    let Ok(Some((px, w, h, ms))) = next()? else { return Ok(false) };
    match stated {
        None => bail!("AVIF: the file states no picture size"),
        Some((sw, sh)) if w as u64 * h as u64 > sw as u64 * sh as u64 => bail!("AVIF: the frames are {w} x {h} but the file states {sw} x {sh}"),
        Some(_) => {}
    }
    check(w, h, n)?;
    let mut held = Held::new(sink, w, h, 0);
    held.push(px, shown_ms(ms))?;
    loop {
        match next()? {
            Ok(Some((px, fw, fh, ms))) => {
                if (fw, fh) != (w, h) {
                    bail!("AVIF: the frames differ in size");
                }
                held.push(px, shown_ms(ms))?;
            }
            Ok(None) => break,
            Err(_) if !held.animated() => return Ok(false),
            Err(e) => bail!("AVIF: {e}"),
        }
    }
    Ok(held.animated())
}

#[derive(Debug)]
pub struct Stopped;

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the conversion was cancelled")
    }
}

impl std::error::Error for Stopped {}

pub type Stop<'a> = dyn Fn() -> bool + Sync + 'a;

pub fn convert<W: Write + Seek>(path: &Path, p: &Params, out: W, stop: &Stop) -> Result<Option<(u32, u32)>> {
    let mut writer = Writer::new(p, out, stop)?;
    let animated = match p.resize {
        Some((mode, filter)) => decode(path, &mut Resized { sink: &mut writer, mode, filter, from: (0, 0) }, stop)?,
        None => decode(path, &mut writer, stop)?,
    };
    if !animated {
        return Ok(None);
    }
    writer.finish().map(Some)
}

struct Resized<'s> {
    sink: &'s mut dyn Sink,
    mode: ResizeMode,
    filter: ResizeFilter,
    from: (u32, u32),
}

impl Sink for Resized<'_> {
    fn start(&mut self, w: u32, h: u32, plays: u32) -> Result<()> {
        self.from = (w, h);
        let (nw, nh) = crate::convert::resized_size(w, h, self.mode);
        crate::convert::check_resized(nw, nh)?;
        self.sink.start(nw, nh, plays)
    }

    fn frame(&mut self, rgba: Vec<u8>, ms: u32) -> Result<()> {
        let (px, _, _) = crate::convert::resize(rgba, self.from.0, self.from.1, self.mode, self.filter)?;
        self.sink.frame(px, ms)
    }
}

struct Writer<'p, W: Write + Seek> {
    p: &'p Params,
    out: Option<W>,
    enc: Option<Encoding<'p, W>>,
    size: usize,
    stop: &'p Stop<'p>,
}

enum Encoding<'p, W: Write + Seek> {
    Gif(GifWriter<'p, Capped<W>>),
    Webp(WebpWriter<Capped<W>>),
}

impl<'p, W: Write + Seek> Writer<'p, W> {
    fn new(p: &'p Params, out: W, stop: &'p Stop<'p>) -> Result<Self> {
        if !matches!(p.kind, Kind::Gif | Kind::Webp) {
            bail!("only GIF and WebP can hold an animation");
        }
        Ok(Self { p, out: Some(out), enc: None, size: 0, stop })
    }

    fn finish(self) -> Result<(u32, u32)> {
        match self.enc {
            Some(Encoding::Gif(g)) => g.finish(),
            Some(Encoding::Webp(w)) => w.finish_capped(),
            None => bail!("the animation has no frames"),
        }
    }
}

impl<W: Write + Seek> Sink for Writer<'_, W> {
    fn start(&mut self, w: u32, h: u32, plays: u32) -> Result<()> {
        check(w, h, 0)?;
        let out = Capped { inner: self.out.take().context("the animation started twice")?, left: MAX_OUTPUT };
        self.size = w as usize * h as usize * 4;
        self.enc = Some(match self.p.kind {
            Kind::Gif => Encoding::Gif(GifWriter::new(self.p, out, w, h, plays)?),
            _ => Encoding::Webp(WebpWriter::new(self.p, out, w, h, plays)?),
        });
        Ok(())
    }

    fn frame(&mut self, rgba: Vec<u8>, ms: u32) -> Result<()> {
        if (self.stop)() {
            return Err(Stopped.into());
        }
        if rgba.len() != self.size {
            bail!("the animation's frames don't match its size");
        }
        match &mut self.enc {
            Some(Encoding::Gif(g)) => g.frame(rgba, ms),
            Some(Encoding::Webp(w)) => w.frame(rgba, ms),
            None => bail!("the animation's frames don't match its size"),
        }
    }
}

struct Capped<W> {
    inner: W,
    left: u64,
}

impl<W: Write> Write for Capped<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.len() as u64 > self.left {
            return Err(std::io::Error::other(format!("the result would be larger than {} GB", MAX_OUTPUT >> 30)));
        }
        let n = self.inner.write(buf)?;
        self.left -= n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Seek> Seek for Capped<W> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

fn gif_err(e: gif::EncodingError) -> anyhow::Error {
    match e {
        gif::EncodingError::Io(e) => e.into(),
        e => anyhow!("GIF: {e}"),
    }
}

fn gif_encoder<W: Write>(out: W, w: u32, h: u32, plays: u32) -> Result<gif::Encoder<W>> {
    use gif::Repeat;
    if w > u16::MAX as u32 || h > u16::MAX as u32 {
        bail!("GIF can't be larger than 65535 px");
    }
    let mut enc = gif::Encoder::new(out, w as u16, h as u16, &[]).map_err(gif_err)?;
    match plays {
        0 => enc.set_repeat(Repeat::Infinite).map_err(gif_err)?,
        1 => {}
        n => enc.set_repeat(Repeat::Finite((n - 1).min(u16::MAX as u32) as u16)).map_err(gif_err)?,
    }
    Ok(enc)
}

pub fn still_gif(p: &Params, w: u32, h: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    let mut enc = gif_encoder(Vec::new(), w, h, 1)?;
    let mut frame = gif_picture(p, rgba, w as usize, [0, 0, w as usize, h as usize], None, false);
    frame.dispose = gif::DisposalMethod::Keep;
    frame.delay = 10;
    enc.write_frame(&frame).map_err(gif_err)?;
    enc.into_inner().map_err(gif_err)
}

fn gif_quality(p: &Params) -> u8 {
    match p.gif_speed {
        0..=3 => 2,
        4..=15 => 1,
        _ => 0,
    }
}

fn gif_picture(p: &Params, pic: &[u8], w: usize, rect: [usize; 4], under: Option<&[u8]>, clears: bool) -> gif::Frame<'static> {
    let [x0, y0, x1, y1] = rect;
    let (rw, rh) = (x1 - x0, y1 - y0);
    let sub = if under.is_none() && rw == w && rw * rh * 4 == pic.len() {
        std::borrow::Cow::Borrowed(pic)
    } else {
        let mut sub = Vec::with_capacity(rw * rh * 4);
        for y in y0..y1 {
            let row = &pic[(y * w + x0) * 4..(y * w + x1) * 4];
            match under {
                Some(u) => {
                    let old = &u[(y * w + x0) * 4..(y * w + x1) * 4];
                    for (c, o) in row.as_chunks::<4>().0.iter().zip(old.as_chunks::<4>().0) {
                        sub.extend_from_slice(if c == o { &[0, 0, 0, 0] } else { c });
                    }
                }
                None => sub.extend_from_slice(row),
            }
        }
        std::borrow::Cow::Owned(sub)
    };
    let q = crate::recfmt::quantize_with(&sub, rw, rh, gif_quality(p), true, clears);
    let (mut palette, mut transparent) = (q.palette, q.transparent);
    if clears && transparent.is_none() && palette.len() < 256 * 3 {
        transparent = Some((palette.len() / 3) as u8);
        palette.extend_from_slice(&[0, 0, 0]);
    }
    gif::Frame {
        left: x0 as u16,
        top: y0 as u16,
        width: rw as u16,
        height: rh as u16,
        palette: Some(palette),
        transparent,
        buffer: std::borrow::Cow::Owned(q.indices),
        ..gif::Frame::default()
    }
}

fn cs(ms: u64) -> u64 {
    (ms + 5) / 10
}

struct GifWriter<'p, W: Write> {
    p: &'p Params,
    enc: gif::Encoder<W>,
    w: usize,
    h: usize,
    t: u64,
    waiting: Option<(Vec<u8>, u64)>,
    ready: Option<(Vec<u8>, u64)>,
    canvas: Vec<u8>,
    plays: u32,
    first_clear: Option<Vec<u64>>,
}

impl<'p, W: Write> GifWriter<'p, W> {
    fn new(p: &'p Params, out: W, w: u32, h: u32, plays: u32) -> Result<Self> {
        let enc = gif_encoder(out, w, h, plays)?;
        Ok(Self { p, enc, w: w as usize, h: h as usize, t: 0, waiting: None, ready: None, canvas: Vec::new(), plays, first_clear: None })
    }

    fn frame(&mut self, mut pic: Vec<u8>, ms: u32) -> Result<()> {
        for c in pic.as_chunks_mut::<4>().0 {
            if c[3] >= 128 {
                c[3] = 255;
            } else {
                c.fill(0);
            }
        }
        let start = cs(self.t);
        self.t += ms as u64;
        self.waiting = match self.waiting.take() {
            None => Some((pic, start)),
            Some((prev, at)) if prev == pic => Some((prev, at)),
            Some((_, at)) if start < at + 2 => Some((pic, at)),
            Some((prev, at)) => {
                self.put(prev, start - at)?;
                Some((pic, start))
            }
        };
        Ok(())
    }

    fn put(&mut self, pic: Vec<u8>, delay: u64) -> Result<()> {
        if let Some((prev, d)) = self.ready.take() {
            let gone = turned_clear(&prev, self.w, |i| pic[i * 4 + 3] == 0);
            self.write(prev, d, gone)?;
        }
        self.ready = Some((pic, delay));
        Ok(())
    }

    fn write(&mut self, mut pic: Vec<u8>, delay: u64, gone: Option<[usize; 4]>) -> Result<()> {
        use gif::DisposalMethod;
        let (w, h) = (self.w, self.h);
        if self.canvas.is_empty() && self.plays != 1 {
            let mut clear = vec![0u64; (w * h).div_ceil(64)];
            for (i, c) in pic.as_chunks::<4>().0.iter().enumerate() {
                if c[3] == 0 {
                    clear[i / 64] |= 1 << (i % 64);
                }
            }
            self.first_clear = clear.iter().any(|&b| b != 0).then_some(clear);
        }
        let changed = if self.canvas.is_empty() { Some([0, 0, w, h]) } else { changed_rect(&self.canvas, &pic, w, h) };
        let rect = match (changed, gone) {
            (Some(a), Some(b)) => Some([a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]),
            (a, b) => a.or(b),
        };
        let under = (!self.canvas.is_empty()).then_some(&self.canvas[..]);
        let mut frame = match rect {
            Some(r) => gif_picture(self.p, &pic, w, r, under, gone.is_some()),
            None => gif::Frame { width: 1, height: 1, palette: Some(vec![0, 0, 0]), transparent: Some(0), buffer: std::borrow::Cow::Owned(vec![0]), ..gif::Frame::default() },
        };
        frame.dispose = if gone.is_some() { DisposalMethod::Background } else { DisposalMethod::Keep };
        frame.delay = delay.clamp(2, u16::MAX as u64) as u16;
        self.enc.write_frame(&frame).map_err(gif_err)?;
        if let (Some(_), Some([x0, y0, x1, y1])) = (gone, rect) {
            for y in y0..y1 {
                pic[(y * w + x0) * 4..(y * w + x1) * 4].fill(0);
            }
        }
        self.canvas = pic;
        Ok(())
    }

    fn finish(mut self) -> Result<(u32, u32)> {
        if let Some((prev, at)) = self.waiting.take() {
            let end = cs(self.t);
            self.put(prev, end.saturating_sub(at))?;
        }
        if let Some((last, d)) = self.ready.take() {
            let gone = self.first_clear.as_ref().and_then(|clear| turned_clear(&last, self.w, |i| (clear[i / 64] >> (i % 64)) & 1 != 0));
            self.write(last, d, gone)?;
        }
        self.enc.into_inner().map_err(gif_err)?.flush()?;
        Ok((self.w as u32, self.h as u32))
    }
}

fn changed_rect(prev: &[u8], cur: &[u8], w: usize, h: usize) -> Option<[usize; 4]> {
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
    let differs = |(p, q): (&[u8; 4], &[u8; 4])| p != q;
    for y in 0..h {
        let (a, b) = (&prev[y * w * 4..(y + 1) * w * 4], &cur[y * w * 4..(y + 1) * w * 4]);
        if a == b {
            continue;
        }
        let first = a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0).position(differs).unwrap_or(0);
        let last = a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0).rposition(differs).unwrap_or(w - 1);
        x0 = x0.min(first);
        x1 = x1.max(last + 1);
        y0 = y0.min(y);
        y1 = y + 1;
    }
    (x1 > x0 && y1 > y0).then_some([x0, y0, x1, y1])
}

fn turned_clear(pic: &[u8], w: usize, clear: impl Fn(usize) -> bool) -> Option<[usize; 4]> {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for (y, row) in pic.chunks_exact(w * 4).enumerate() {
        let gone = |x: &usize| row[x * 4 + 3] != 0 && clear(y * w + x);
        if let Some(first) = (0..w).find(gone) {
            let last = (first..w).rfind(gone).unwrap_or(first);
            x0 = x0.min(first);
            x1 = x1.max(last + 1);
            y0 = y0.min(y);
            y1 = y + 1;
        }
    }
    (x1 > x0).then_some([x0, y0, x1, y1])
}

struct WebpWriter<W: Write + Seek> {
    out: W,
    cfg: zenwebp::EncoderConfig,
    w: usize,
    h: usize,
    prev: Vec<u8>,
    len: u64,
}

impl<W: Write + Seek> WebpWriter<W> {
    fn new(p: &Params, mut out: W, w: u32, h: u32, plays: u32) -> Result<Self> {
        use zenwebp::{EncoderConfig, LosslessConfig, LossyConfig};
        if !p.lossless && (w > 16383 || h > 16383) {
            bail!("lossy WebP can't be larger than 16383 px (lossless can be 16384)");
        }
        if w > 16384 || h > 16384 {
            bail!("WebP can't be larger than 16384 px");
        }
        let method = p.effort.min(6);
        let cfg = if p.lossless {
            EncoderConfig::Lossless(LosslessConfig::new().with_quality(100.0).with_method(method))
        } else {
            EncoderConfig::Lossy(LossyConfig::new().with_quality(p.quality as f32).with_method(method))
        };
        let head = webp_anim_head(w, h, plays);
        out.write_all(&head)?;
        Ok(Self { out, cfg, w: w as usize, h: h as usize, prev: Vec::new(), len: head.len() as u64 })
    }

    fn frame(&mut self, rgba: Vec<u8>, ms: u32) -> Result<()> {
        let (w, h) = (self.w, self.h);
        let [x0, y0, x1, y1] = if self.prev.is_empty() {
            [0, 0, w, h]
        } else {
            match changed_rect(&self.prev, &rgba, w, h) {
                Some([x0, y0, x1, y1]) => [x0 & !1, y0 & !1, ((x1 + 1) & !1).min(w), ((y1 + 1) & !1).min(h)],
                None => [0, 0, 1, 1],
            }
        };
        let (fw, fh) = (x1 - x0, y1 - y0);
        let still = {
            let sub = if (fw, fh) == (w, h) {
                std::borrow::Cow::Borrowed(&rgba[..])
            } else {
                let mut s = Vec::with_capacity(fw * fh * 4);
                for y in y0..y1 {
                    s.extend_from_slice(&rgba[(y * w + x0) * 4..(y * w + x1) * 4]);
                }
                std::borrow::Cow::Owned(s)
            };
            zenwebp::EncodeRequest::new(&self.cfg, &sub, zenwebp::PixelLayout::Rgba8, fw as u32, fh as u32).encode().map_err(|e| anyhow!("WebP: {e}"))?
        };
        let chunks = webp_still_chunks(&still);
        let head = webp_frame_head([x0, y0, fw, fh].map(|v| v as u32), ms, chunks.len())?;
        self.out.write_all(&head)?;
        self.out.write_all(chunks)?;
        self.len += (head.len() + chunks.len()) as u64;
        self.prev = rgba;
        Ok(())
    }

    fn finish(mut self) -> Result<(u32, u32)> {
        let size = u32::try_from(self.len - 8).map_err(|_| anyhow!("the result would be larger than 4 GB"))?;
        self.out.seek(SeekFrom::Start(4))?;
        self.out.write_all(&size.to_le_bytes())?;
        self.out.seek(SeekFrom::End(0))?;
        self.out.flush()?;
        Ok((self.w as u32, self.h as u32))
    }
}

impl<W: Write + Seek> WebpWriter<Capped<W>> {
    fn finish_capped(mut self) -> Result<(u32, u32)> {
        self.out.left += 4;
        self.finish()
    }
}

pub(crate) fn webp_anim_head(w: u32, h: u32, plays: u32) -> Vec<u8> {
    let mut head = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
    head.extend_from_slice(&10u32.to_le_bytes());
    head.extend_from_slice(&[0x12, 0, 0, 0]);
    head.extend_from_slice(&u24(w - 1));
    head.extend_from_slice(&u24(h - 1));
    head.extend_from_slice(b"ANIM");
    head.extend_from_slice(&6u32.to_le_bytes());
    head.extend_from_slice(&[0, 0, 0, 0]);
    head.extend_from_slice(&(plays.min(u16::MAX as u32) as u16).to_le_bytes());
    head
}

pub(crate) fn webp_still_chunks(still: &[u8]) -> &[u8] {
    let body = still.get(12..).unwrap_or_default();
    if body.starts_with(b"VP8X") { body.get(18..).unwrap_or_default() } else { body }
}

pub(crate) fn webp_frame_head([x, y, w, h]: [u32; 4], ms: u32, chunks: usize) -> Result<Vec<u8>> {
    let mut head = b"ANMF".to_vec();
    head.extend_from_slice(&u32::try_from(16 + chunks)?.to_le_bytes());
    for v in [x / 2, y / 2, w - 1, h - 1, ms.min(0xFF_FFFF)] {
        head.extend_from_slice(&u24(v));
    }
    head.push(2);
    Ok(head)
}

fn u24(v: u32) -> [u8; 3] {
    let b = v.to_le_bytes();
    [b[0], b[1], b[2]]
}
