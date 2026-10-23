
use crate::convert::{MAX_DECODE_PIXELS, MAX_DECODE_SIDE, check_size};
use anyhow::{Context, Result, anyhow, bail};
use std::path::Path;

type Rgba = (Vec<u8>, u32, u32);

pub const RAW_EXTS: &[&str] = &[
    "dng", "cr2", "nef", "nrw", "arw", "srf", "sr2", "orf", "rw2", "raf", "pef", "srw", "mrw", "erf", "kdc", "dcr", "3fr", "mef", "mos",
    "raw", "rwl", "iiq",
];

pub fn is_raw(ext: &str) -> bool {
    RAW_EXTS.contains(&ext)
}

pub fn heic_any(bytes: &[u8]) -> Result<crate::convert::Picture> {
    use crate::convert::{Decoded, Picture};
    let info = heic::ImageInfo::from_bytes(bytes).map_err(|e| anyhow!("HEIC: {e:?}"))?;
    check_size(info.width, info.height)?;
    heic_decodable(bytes)?;
    if info.bit_depth <= 8 && !matches!(info.transfer_characteristics, 16 | 18) {
        let icc = info.icc_profile.clone().or_else(|| crate::color::from_cicp(info.color_primaries as u8, info.transfer_characteristics as u8));
        return heic(bytes).map(|(px, w, h)| Picture { px: Decoded::Eight(px, w, h), icc });
    }
    let frame = heic::DecoderConfig::new().decode_to_frame(bytes).map_err(|e| anyhow!("HEIC: {e}"))?;
    Ok(Picture { px: Decoded::Deep(crate::deep::from_heic_frame(&frame)?), icc: info.icc_profile.clone() })
}

pub fn heic(bytes: &[u8]) -> Result<Rgba> {
    use heic::{DecoderConfig, HeicError, Limits, PixelLayout};
    let mut limits = Limits::default();
    limits.max_width = Some(MAX_DECODE_SIDE as u64);
    limits.max_height = Some(MAX_DECODE_SIDE as u64);
    limits.max_pixels = Some(MAX_DECODE_PIXELS);
    limits.max_memory_bytes = Some(1 << 30);
    let config = DecoderConfig::new();
    let request = || config.decode_request(bytes).with_output_layout(PixelLayout::Rgba8).with_limits(&limits);
    if let Some((w, h)) = plain_grid(bytes) {
        check_size(w, h)?;
        let mut px = vec![0; w as usize * h as usize * 4];
        match request().decode_into(&mut px) {
            Ok((w, h)) => {
                check_size(w, h)?;
                px.truncate(w as usize * h as usize * 4);
                return Ok((px, w, h));
            }
            Err(e) if matches!(e.error(), HeicError::BufferTooSmall { .. }) => {}
            Err(e) => bail!("HEIC: {e}"),
        }
    }
    let out = request().decode().map_err(|e| anyhow!("HEIC: {e}"))?;
    check_size(out.width, out.height)?;
    Ok((out.data, out.width, out.height))
}

const HEIC_ALPHA: [&str; 2] = ["urn:mpeg:hevc:2015:auxid:1", "urn:mpeg:mpegB:cicp:systems:auxiliary:alpha"];

fn plain_grid(bytes: &[u8]) -> Option<(u32, u32)> {
    use heic::heif::{FourCC, ItemType};
    let file = heic::heif::parse(bytes, &heic::Unstoppable).ok()?;
    let grid = file.primary_item().filter(|p| p.item_type == ItemType::Grid && p.transforms.is_empty())?;
    let tile = file.get_item(*file.get_item_references(grid.id, FourCC::DIMG).first()?)?;
    if tile.hevc_config.is_none() || HEIC_ALPHA.iter().any(|urn| !file.find_auxiliary_items(grid.id, urn).is_empty()) {
        return None;
    }
    let d = file.get_item_data(grid.id).ok()?;
    let n = |at: usize, len: usize| d.get(at..at + len).map(|v| v.iter().fold(0u32, |n, &b| n << 8 | u32::from(b)));
    if d.get(1)? & 1 != 0 { Some((n(4, 4)?, n(8, 4)?)) } else { Some((n(4, 2)?, n(6, 2)?)) }
}

fn heic_decodable(bytes: &[u8]) -> Result<()> {
    use heic::heif::{FourCC, ItemType};
    let file = heic::heif::parse(bytes, &heic::Unstoppable).map_err(|e| anyhow!("HEIC: {e}"))?;
    let Some(primary) = file.primary_item() else { return Ok(()) };
    let (mut todo, mut seen, mut depth) = (vec![primary.id], std::collections::HashSet::new(), None);
    while let Some(id) = todo.pop() {
        let Some(item) = file.get_item(id).filter(|_| seen.insert(id)) else { continue };
        let refs = file.get_item_references(id, FourCC::DIMG);
        match item.item_type {
            ItemType::Grid | ItemType::Iovl => todo.extend(refs),
            ItemType::Iden | ItemType::Tmap => todo.extend(refs.first()),
            _ => {
                if let Some(why) = hevc_unsupported(&file, &item) {
                    bail!("HEIC: pictures coded as {why} are not supported");
                }
                depth = depth.or(item.hevc_config.as_ref().map(|c| c.bit_depth_luma_minus8));
            }
        }
    }
    let alpha = HEIC_ALPHA
        .into_iter()
        .find_map(|urn| file.find_auxiliary_items(primary.id, urn).first().copied())
        .and_then(|id| file.get_item(id));
    if let Some(alpha) = alpha {
        let Some(config) = &alpha.hevc_config else { bail!("HEIC: transparency that is not a single HEVC picture is not supported") };
        if let Some(why) = hevc_unsupported(&file, &alpha) {
            bail!("HEIC: transparency coded as {why} is not supported");
        }
        if Some(config.bit_depth_luma_minus8) != depth {
            bail!("HEIC: transparency at a different bit depth from the picture is not supported");
        }
        if !primary.transforms.is_empty() {
            bail!("HEIC: transparency on a rotated, mirrored or cropped picture is not supported");
        }
    }
    Ok(())
}

fn hevc_unsupported(file: &heic::heif::HeifContainer<'_>, item: &heic::heif::Item) -> Option<&'static str> {
    let config = item.hevc_config.as_ref()?;
    match config.chroma_format {
        0 => return Some("grayscale HEVC"),
        2 => return Some("4:2:2 HEVC"),
        3 => return Some("4:4:4 HEVC"),
        _ => {}
    }
    if config.bit_depth_luma_minus8 > 2 || config.bit_depth_chroma_minus8 > 2 {
        return Some("12-bit HEVC");
    }
    let data = file.get_item_data(item.id).ok()?;
    let size = usize::from(config.length_size_minus_one) + 1;
    let mut at = 0;
    let in_data = std::iter::from_fn(|| {
        let len = data.get(at..at + size)?.iter().fold(0, |n, &b| n << 8 | usize::from(b));
        let unit = data.get(at + size..at + size + len)?;
        at += size + len;
        Some(unit)
    });
    let mut slices = 0;
    for unit in config.nal_units.iter().map(Vec::as_slice).chain(in_data) {
        match unit.first().map(|b| b >> 1 & 0x3f) {
            Some(34) if pps_lossless(unit) => return Some("lossless HEVC"),
            Some(k) if k < 32 && unit.len() >= 2 && unit[0] & 1 == 0 && unit[1] >> 3 == 0 => slices += 1,
            _ => {}
        }
    }
    (slices > 1).then_some("HEVC in several slices")
}

fn pps_lossless(unit: &[u8]) -> bool {
    fn golomb(bits: &mut impl Iterator<Item = bool>) -> Option<()> {
        match bits.position(|b| b)? {
            0 => Some(()),
            n => bits.nth(n - 1).map(drop),
        }
    }
    let mut bits = unit.iter().skip(2).flat_map(|b| (0..8).rev().map(move |i| b >> i & 1 == 1));
    let mut read = || {
        golomb(&mut bits)?;
        golomb(&mut bits)?;
        bits.nth(6)?;
        golomb(&mut bits)?;
        golomb(&mut bits)?;
        golomb(&mut bits)?;
        bits.nth(1)?;
        if bits.next()? {
            golomb(&mut bits)?;
        }
        golomb(&mut bits)?;
        golomb(&mut bits)?;
        bits.nth(2)?;
        bits.next()
    };
    read().unwrap_or(false)
}

pub fn jpeg2000(bytes: &[u8]) -> Result<Rgba> {
    use hayro_jpeg2000::{ColorSpace, DecodeSettings, DecoderContext, Image};
    let image = Image::new(bytes, &DecodeSettings::default()).map_err(|e| anyhow!("JPEG 2000: {e:?}"))?;
    let (w, h) = (image.width(), image.height());
    check_size(w, h)?;
    let alpha = image.has_alpha();
    let channels = image.color_space().num_channels() as usize;
    let cmyk = matches!(image.color_space(), ColorSpace::CMYK);
    let mut ctx = DecoderContext::default();
    let decoded = image.decode(&mut ctx).map_err(|e| anyhow!("JPEG 2000: {e:?}"))?;
    let data = decoded.data_u8();
    let stride = channels + usize::from(alpha);
    if stride == 0 || data.len() < w as usize * h as usize * stride {
        bail!("JPEG 2000: the decoder returned too little data");
    }
    let mut out = Vec::with_capacity(w as usize * h as usize * 4);
    for px in data.chunks_exact(stride).take(w as usize * h as usize) {
        let a = if alpha { px[channels] } else { 255 };
        match channels {
            1 => out.extend_from_slice(&[px[0], px[0], px[0], a]),
            2 => out.extend_from_slice(&[px[0], px[0], px[0], px[1]]),
            3 => out.extend_from_slice(&[px[0], px[1], px[2], a]),
            _ if cmyk => {
                let k = 255 - px[3] as u32;
                let c = |v: u8| ((255 - v as u32) * k / 255) as u8;
                out.extend_from_slice(&[c(px[0]), c(px[1]), c(px[2]), a]);
            }
            _ => out.extend_from_slice(&[px[0], px[1], px[2], a]),
        }
    }
    Ok((out, w, h))
}

pub fn psd(bytes: &[u8]) -> Result<Rgba> {
    if bytes.len() < 26 || &bytes[..4] != b"8BPS" {
        bail!("not a Photoshop file");
    }
    let h = u32::from_be_bytes([bytes[14], bytes[15], bytes[16], bytes[17]]);
    let w = u32::from_be_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]);
    check_size(w, h)?;
    let depth = u16::from_be_bytes([bytes[22], bytes[23]]);
    let mode = u16::from_be_bytes([bytes[24], bytes[25]]);
    if !matches!(mode, 1 | 3) {
        bail!("Photoshop: only RGB and grayscale documents are supported (this one is CMYK, indexed, Lab or similar)");
    }
    if depth != 8 {
        bail!("Photoshop: only 8-bit documents are supported");
    }
    if bytes[4..6] != [0, 1] {
        bail!("Photoshop: large documents (PSB) are not supported");
    }
    let channels = u16::from_be_bytes([bytes[12], bytes[13]]) as usize;
    let colours = if mode == 1 { 1 } else { 3 };
    if channels < colours {
        bail!("Photoshop: the document has too few channels");
    }
    let cut = || anyhow!("Photoshop: the file is cut off");
    let be32 = |at: usize| bytes.get(at..at + 4).map(|v| u32::from_be_bytes([v[0], v[1], v[2], v[3]]) as usize).ok_or_else(cut);
    let resources = 30 + be32(26)?;
    let layers = resources + 4 + be32(resources)?;
    let image_data = layers + 4 + be32(layers)?;
    let mut at = resources + 4;
    while at + 12 <= layers && &bytes[at..at + 4] == b"8BIM" {
        let id = u16::from_be_bytes([bytes[at + 4], bytes[at + 5]]);
        let name = (bytes[at + 6] as usize + 2) & !1;
        let size = be32(at + 6 + name)?;
        let data = at + 10 + name;
        if id == 1057 && size > 4 && bytes.get(data + 4) == Some(&0) {
            bail!("Photoshop: the document was saved without Maximize Compatibility, so it holds no flattened picture");
        }
        at = data + size + (size & 1);
    }
    let transparency = channels > colours && psd_merged_transparency(bytes, layers + 4, image_data);
    let (w, h) = (w as usize, h as usize);
    let wanted = colours + usize::from(transparency);
    let data = bytes.get(image_data..).ok_or_else(cut)?;
    let compression = data.get(..2).ok_or_else(cut)?;
    let needed = match compression {
        [0, 0] => 2 + wanted * h * w,
        [0, 1] => {
            let table = data.get(2..2 + channels * h * 2).ok_or_else(cut)?;
            let least = 2 * w.div_ceil(128);
            let mut total = 2 + table.len();
            for i in 0..wanted * h {
                let n = usize::from(u16::from_be_bytes([table[2 * i], table[2 * i + 1]]));
                if n < least {
                    return Err(cut());
                }
                total += n;
            }
            total
        }
        _ => 0,
    };
    if data.len() < needed {
        return Err(cut());
    }
    let mut out = vec![255u8; w * h * 4];
    let mut put = |c: usize, y: usize, row: &[u8]| {
        for (p, &v) in out[y * w * 4..(y + 1) * w * 4].as_chunks_mut::<4>().0.iter_mut().zip(row) {
            match c {
                _ if c == colours => p[3] = v,
                0 if colours == 1 => p[..3].fill(v),
                _ => p[c] = v,
            }
        }
    };
    match compression {
        [0, 0] => {
            for c in 0..wanted {
                for y in 0..h {
                    put(c, y, data.get(2 + (c * h + y) * w..).and_then(|r| r.get(..w)).ok_or_else(cut)?);
                }
            }
        }
        [0, 1] => {
            let table = data.get(2..2 + channels * h * 2).ok_or_else(cut)?;
            let mut at = 2 + table.len();
            let mut row = Vec::with_capacity(w);
            for c in 0..wanted {
                for y in 0..h {
                    let len = u16::from_be_bytes([table[(c * h + y) * 2], table[(c * h + y) * 2 + 1]]) as usize;
                    row.clear();
                    unpack_bits(data.get(at..at + len).ok_or_else(cut)?, w, &mut row)?;
                    put(c, y, &row);
                    at += len;
                }
            }
        }
        _ => bail!("Photoshop: the composite uses a compression that is not supported"),
    }
    if transparency {
        for p in out.as_chunks_mut::<4>().0.iter_mut() {
            let a = p[3] as i32;
            if a != 0 && a != 255 {
                for c in &mut p[..3] {
                    *c = (((*c as i32 - (255 - a)) * 255 + a / 2) / a).clamp(0, 255) as u8;
                }
            }
        }
    }
    Ok((out, w as u32, h as u32))
}

fn psd_merged_transparency(b: &[u8], at: usize, end: usize) -> bool {
    let b = &b[..end.min(b.len())];
    let end = b.len();
    let be32 = |at: usize| b.get(at..at + 4).map(|v| u32::from_be_bytes([v[0], v[1], v[2], v[3]]) as usize);
    let negative = |at: usize| b.get(at..at + 2).is_some_and(|v| i16::from_be_bytes([v[0], v[1]]) < 0);
    let Some(info) = be32(at) else { return false };
    if info >= 2 && negative(at + 4) {
        return true;
    }
    let Some(mask) = be32(at + 4 + info) else { return false };
    let mut at = at + 8 + info + mask;
    while at + 12 <= end && matches!(&b[at..at + 4], b"8BIM" | b"8B64") {
        match &b[at + 4..at + 8] {
            b"Mtrn" | b"Mt16" | b"Mt32" => return true,
            b"Layr" | b"Lr16" | b"Lr32" if negative(at + 12) => return true,
            _ => {}
        }
        at += 12 + be32(at + 8).unwrap_or(0);
        while at < end && b[at] == 0 {
            at += 1;
        }
    }
    false
}

fn unpack_bits(mut row: &[u8], w: usize, out: &mut Vec<u8>) -> Result<()> {
    let short = || anyhow!("Photoshop: a row of the composite is cut short");
    let end = out.len() + w;
    while out.len() < end {
        let (&n, rest) = row.split_first().ok_or_else(short)?;
        row = rest;
        match n {
            0..=127 => {
                let (lit, rest) = row.split_at_checked(n as usize + 1).ok_or_else(short)?;
                out.extend_from_slice(lit);
                row = rest;
            }
            128 => {}
            _ => {
                let (&v, rest) = row.split_first().ok_or_else(short)?;
                out.resize(out.len() + 257 - n as usize, v);
                row = rest;
            }
        }
    }
    out.truncate(end);
    Ok(())
}

pub fn svg(bytes: &[u8]) -> Result<Rgba> {
    use resvg::{tiny_skia, usvg};
    let mut opt = usvg::Options::default();
    opt.image_href_resolver.resolve_string = Box::new(|_, _| None);
    if bytes.windows(5).any(|w| &w[1..] == b"text" && matches!(w[0], b'<' | b':')) || bytes.starts_with(&[0x1f, 0x8b]) {
        opt.fontdb_mut().load_system_fonts();
    }
    let tree = usvg::Tree::from_data(bytes, &opt).map_err(|e| anyhow!("SVG: {e}"))?;
    let size = tree.size();
    let (sw, sh) = (size.width().max(1.0), size.height().max(1.0));
    let longest = sw.max(sh);
    let scale = if longest < 1024.0 {
        1024.0 / longest
    } else if longest > 8192.0 {
        8192.0 / longest
    } else {
        1.0
    };
    let (w, h) = (((sw * scale).round() as u32).max(1), ((sh * scale).round() as u32).max(1));
    check_size(w, h)?;
    let mut pixmap = tiny_skia::Pixmap::new(w, h).context("SVG: can't allocate the picture")?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(w as f32 / sw, h as f32 / sh), &mut pixmap.as_mut());
    let mut data = pixmap.take();
    for p in data.as_chunks_mut::<4>().0.iter_mut() {
        let a = p[3] as u32;
        if a != 0 && a != 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    Ok((data, w, h))
}

pub fn raw(path: &Path) -> Result<Rgba> {
    let mut pipeline = imagepipe::Pipeline::new_from_file(path).map_err(|e| anyhow!("RAW: {e}"))?;
    if let imagepipe::ImageSource::Raw(img) = &pipeline.globals.image {
        if img.orientation.to_u16() == 7 {
            (pipeline.ops.transform.fliph, pipeline.ops.transform.flipv) = (false, true);
        }
        neutral_at_d65(&mut pipeline.ops.tolab);
    }
    let img = pipeline.output_8bit(None).map_err(|e| anyhow!("RAW: {e}"))?;
    let (w, h) = (img.width as u32, img.height as u32);
    check_size(w, h)?;
    if img.data.len() < w as usize * h as usize * 3 {
        bail!("RAW: the decoder returned too little data");
    }
    let mut out = Vec::with_capacity(w as usize * h as usize * 4);
    for p in img.data.as_chunks::<3>().0.iter().take(w as usize * h as usize) {
        out.extend_from_slice(&[p[0], p[1], p[2], 255]);
    }
    Ok((out, w, h))
}

fn neutral_at_d65(tolab: &mut imagepipe::colorspaces::OpToLab) {
    const D65: [f32; 3] = [0.95047, 1.0, 1.08883];
    for (j, row) in tolab.xyz_to_cam.iter().enumerate() {
        let to_e: f32 = row.iter().sum();
        let to_d65: f32 = row.iter().zip(D65).map(|(c, w)| c * w).sum();
        if to_e.is_normal() && to_d65.is_normal() {
            for xyz in &mut tolab.cam_to_xyz_normalized {
                xyz[j] *= to_d65 / to_e;
            }
        }
    }
}
