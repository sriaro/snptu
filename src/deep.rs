
use crate::convert::{Kind, Params, ResizeFilter, ResizeMode};
use anyhow::{Result, anyhow, bail};

pub struct Deep {
    pub px: Vec<u16>,
    pub w: u32,
    pub h: u32,
    pub cp: u8,
    pub tc: u8,
    pub alpha: bool,
    pub icc: Option<Vec<u8>>,
}

const PQ: u8 = 16;
const HLG: u8 = 18;
const BT2020: u8 = 9;
const P3: u8 = 12;

impl Deep {
    pub fn is_hdr(&self) -> bool {
        matches!(self.tc, PQ | HLG)
    }

    fn needs_mapping(&self) -> bool {
        self.is_hdr() || matches!(self.cp, BT2020 | P3) || self.tc == 8 || self.icc.is_some()
    }

    pub fn native_profile(&self) -> Option<Vec<u8>> {
        if self.is_hdr() || self.tc == 8 {
            return None;
        }
        self.icc.clone().or_else(|| crate::color::from_cicp(self.cp, self.tc))
    }
}


fn pq_to_nits(e: f32) -> f32 {
    const M1: f32 = 0.159_301_76;
    const M2: f32 = 78.84375;
    const C1: f32 = 0.835_937_5;
    const C2: f32 = 18.851_562;
    const C3: f32 = 18.6875;
    let p = e.max(0.0).powf(1.0 / M2);
    ((p - C1).max(0.0) / (C2 - C3 * p)).max(0.0).powf(1.0 / M1) * 10000.0
}

fn hlg_to_scene(e: f32) -> f32 {
    const A: f32 = 0.178_832_77;
    const B: f32 = 0.284_668_92;
    const C: f32 = 0.559_910_7;
    if e <= 0.5 { e * e / 3.0 } else { (((e - C) / A).exp() + B) / 12.0 }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(l: f32) -> f32 {
    let l = l.clamp(0.0, 1.0);
    if l <= 0.003_130_8 { l * 12.92 } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 }
}

const REFERENCE_WHITE: f32 = 203.0;
const PEAK_NITS: f32 = 1000.0;

fn tone(x: f32) -> f32 {
    const KNEE: f32 = 0.8;
    if x <= KNEE {
        x
    } else {
        let s = (x - KNEE) / (1.0 - KNEE);
        KNEE + (1.0 - KNEE) * s / (1.0 + s)
    }
}

fn gamut_to_709(c: u8, rgb: [f32; 3]) -> [f32; 3] {
    let m: [[f32; 3]; 3] = match c {
        BT2020 => [[1.6605, -0.5876, -0.0728], [-0.1246, 1.1329, -0.0083], [-0.0182, -0.1006, 1.1187]],
        P3 => [[1.2249, -0.2247, 0.0], [-0.0420, 1.0419, 0.0], [-0.0197, -0.0786, 1.0979]],
        _ => return rgb,
    };
    [
        m[0][0] * rgb[0] + m[0][1] * rgb[1] + m[0][2] * rgb[2],
        m[1][0] * rgb[0] + m[1][1] * rgb[1] + m[1][2] * rgb[2],
        m[2][0] * rgb[0] + m[2][1] * rgb[1] + m[2][2] * rgb[2],
    ]
}

fn to_srgb(enc: [f32; 3], cp: u8, tc: u8) -> [f32; 3] {
    let mut lin = match tc {
        PQ => enc.map(|e| pq_to_nits(e) / REFERENCE_WHITE),
        HLG => {
            let s = enc.map(hlg_to_scene);
            let y = 0.2627 * s[0] + 0.678 * s[1] + 0.0593 * s[2];
            let k = y.max(1e-6).powf(0.2) * PEAK_NITS / REFERENCE_WHITE;
            s.map(|v| v * k)
        }
        8 => enc,
        _ => enc.map(srgb_to_linear),
    };
    if matches!(tc, PQ | HLG) {
        let m = lin[0].max(lin[1]).max(lin[2]);
        if m > 1e-6 {
            let k = tone(m) / m;
            lin = lin.map(|v| v * k);
        }
    }
    gamut_to_709(cp, lin).map(linear_to_srgb)
}


pub fn to_8bit(d: &Deep) -> Vec<u8> {
    if d.icc.is_some() && !d.is_hdr() {
        return reduce(&to_srgb16(d).px);
    }
    let n = d.w as usize * d.h as usize;
    let mut out = vec![0u8; n * 4];
    if !d.needs_mapping() {
        crate::par::rows(&mut out, d.w as usize * 4, |y, row| {
            let src = &d.px[y * d.w as usize * 4..(y + 1) * d.w as usize * 4];
            for (o, s) in row.iter_mut().zip(src) {
                *o = ((*s as u32 + 128) / 257) as u8;
            }
        });
        return out;
    }
    let lut: Vec<f32> = (0..=65535u32).map(|v| v as f32 / 65535.0).collect();
    crate::par::rows(&mut out, d.w as usize * 4, |y, row| {
        let src = &d.px[y * d.w as usize * 4..(y + 1) * d.w as usize * 4];
        for (o, s) in row.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0.iter()) {
            let c = to_srgb([lut[s[0] as usize], lut[s[1] as usize], lut[s[2] as usize]], d.cp, d.tc);
            for k in 0..3 {
                o[k] = (c[k] * 255.0 + 0.5) as u8;
            }
            o[3] = ((s[3] as u32 + 128) / 257) as u8;
        }
    });
    out
}

pub fn reduce(px: &[u16]) -> Vec<u8> {
    px.iter().map(|s| ((*s as u32 + 128) / 257) as u8).collect()
}

pub fn to_srgb16(d: &Deep) -> Deep {
    if !d.needs_mapping() {
        return Deep { px: d.px.clone(), w: d.w, h: d.h, cp: 0, tc: 0, alpha: d.alpha, icc: None };
    }
    if let (Some(icc), false) = (&d.icc, d.is_hdr()) {
        let px = crate::color::to_srgb_16(&d.px, d.w, icc).unwrap_or_else(|_| d.px.clone());
        return Deep { px, w: d.w, h: d.h, cp: 0, tc: 0, alpha: d.alpha, icc: None };
    }
    let mut px = vec![0u16; d.px.len()];
    crate::par::rows(&mut px, d.w as usize * 4, |y, row| {
        let src = &d.px[y * d.w as usize * 4..(y + 1) * d.w as usize * 4];
        for (o, s) in row.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0.iter()) {
            let c = to_srgb([s[0] as f32 / 65535.0, s[1] as f32 / 65535.0, s[2] as f32 / 65535.0], d.cp, d.tc);
            for k in 0..3 {
                o[k] = (c[k] * 65535.0 + 0.5) as u16;
            }
            o[3] = s[3];
        }
    });
    Deep { px, w: d.w, h: d.h, cp: 0, tc: 0, alpha: d.alpha, icc: None }
}

fn is_alpha(px: &[u16]) -> bool {
    px.as_chunks::<4>().0.iter().any(|p| p[3] != 65535)
}

pub fn from_rgba16(px: Vec<u16>, w: u32, h: u32, cp: u8, tc: u8) -> Deep {
    let alpha = is_alpha(&px);
    Deep { px, w, h, cp, tc, alpha, icc: None }
}


pub fn resize(mut d: Deep, mode: ResizeMode, filter: ResizeFilter) -> Result<Deep> {
    use fast_image_resize::{MulDiv, PixelType, ResizeOptions, Resizer, images::Image};
    let (w, h) = (d.w, d.h);
    let (nw, nh) = crate::convert::resized_size(w, h, mode);
    if (nw, nh) == (w, h) {
        return Ok(d);
    }
    crate::convert::check_resized(nw, nh)?;
    let mut px = vec![0u16; nw as usize * nh as usize * 4];
    let mut src = Image::from_slice_u8(w, h, bytemuck::cast_slice_mut::<u16, u8>(&mut d.px), PixelType::U16x4).map_err(|e| anyhow!("resize: {e}"))?;
    let mut dst = Image::from_slice_u8(nw, nh, bytemuck::cast_slice_mut::<u16, u8>(&mut px), PixelType::U16x4).map_err(|e| anyhow!("resize: {e}"))?;
    let muldiv = MulDiv::default();
    if d.alpha {
        muldiv.multiply_alpha_inplace(&mut src).map_err(|e| anyhow!("resize: {e}"))?;
    }
    Resizer::new()
        .resize(&src, &mut dst, &ResizeOptions::new().resize_alg(crate::convert::resize_alg(filter)).use_alpha(false))
        .map_err(|e| anyhow!("resize: {e}"))?;
    if d.alpha {
        muldiv.divide_alpha_inplace(&mut dst).map_err(|e| anyhow!("resize: {e}"))?;
    }
    Ok(Deep { px, w: nw, h: nh, cp: d.cp, tc: d.tc, alpha: d.alpha, icc: d.icc })
}


pub fn can_hold(kind: Kind) -> bool {
    matches!(kind, Kind::Png)
}

pub fn encode(p: &Params, d: &Deep) -> Result<Vec<u8>> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    let icc = if p.keep_profile { d.native_profile() } else { None };
    let own;
    let sdr: &Deep = if icc.is_some() || !d.needs_mapping() {
        d
    } else {
        own = to_srgb16(d);
        &own
    };
    match p.kind {
        Kind::Png => {
            let (level, filter) = crate::convert::png_options(p.png_level);
            let mut enc = image::codecs::png::PngEncoder::new_with_quality(&mut out, level, filter);
            if let Some(i) = icc {
                let _ = enc.set_icc_profile(i);
            }
            let (px, ct) = pack(sdr);
            enc.write_image(bytemuck::cast_slice(&px), sdr.w, sdr.h, ct)?;
        }
        _ => bail!("{} can't hold more than 8 bits", p.kind.label()),
    }
    Ok(out)
}

fn pack(d: &Deep) -> (std::borrow::Cow<'_, [u16]>, image::ExtendedColorType) {
    if d.alpha {
        (std::borrow::Cow::Borrowed(&d.px[..]), image::ExtendedColorType::Rgba16)
    } else {
        (d.px.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect(), image::ExtendedColorType::Rgb16)
    }
}


pub fn from_heic_frame(f: &heic::hevc::DecodedFrame) -> Result<Deep> {
    let (fw, fh) = (f.width as usize, f.height as usize);
    let (cl, cr, ct, cb_) = (f.crop_left as usize, f.crop_right as usize, f.crop_top as usize, f.crop_bottom as usize);
    if cl + cr >= fw || ct + cb_ >= fh {
        bail!("HEIC: bad crop");
    }
    let (w, h) = (fw - cl - cr, fh - ct - cb_);
    crate::convert::check_size(w as u32, h as u32)?;
    let (sx, sy) = match f.chroma_format {
        1 => (1usize, 1usize),
        2 => (1, 0),
        _ => (0, 0),
    };
    let cw = fw.div_ceil(1 << sx);
    let mono = f.chroma_format == 0 || f.cb_plane.is_empty();
    let chroma_len = cw * fh.div_ceil(1 << sy);
    if f.y_plane.len() < fw * fh || (!mono && (f.cb_plane.len() < chroma_len || f.cr_plane.len() < chroma_len)) {
        bail!("HEIC: the decoder returned too little data");
    }
    let bd = f.bit_depth.clamp(8, 16) as u32;
    let (kr, kb) = match f.matrix_coeffs {
        5 | 6 => (0.299f32, 0.114f32),
        9 | 10 => (0.2627, 0.0593),
        _ => (0.2126, 0.0722),
    };
    let kg = 1.0 - kr - kb;
    let scale = (1u32 << (bd - 8)) as f32;
    let (y_off, y_rng, c_mid, c_rng) = if f.full_range {
        (0.0, ((1u32 << bd) - 1) as f32, (128.0 * scale), ((1u32 << bd) - 1) as f32)
    } else {
        (16.0 * scale, 219.0 * scale, 128.0 * scale, 224.0 * scale)
    };
    let mut px = vec![0u16; w * h * 4];
    crate::par::rows(&mut px, w * 4, |y, row| {
        let sy_ = y + ct;
        for x in 0..w {
            let sx_ = x + cl;
            let yy = ((f.y_plane[sy_ * fw + sx_] as f32 - y_off) / y_rng).clamp(0.0, 1.0);
            let (r, g, b) = if mono {
                (yy, yy, yy)
            } else {
                let ci = (sy_ >> sy) * cw + (sx_ >> sx);
                let cb = (f.cb_plane[ci] as f32 - c_mid) / c_rng;
                let cr_ = (f.cr_plane[ci] as f32 - c_mid) / c_rng;
                let r = yy + 2.0 * (1.0 - kr) * cr_;
                let b = yy + 2.0 * (1.0 - kb) * cb;
                let g = (yy - kr * r - kb * b) / kg;
                (r, g, b)
            };
            let o = &mut row[x * 4..x * 4 + 4];
            o[0] = (r.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
            o[1] = (g.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
            o[2] = (b.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
            o[3] = match &f.alpha_plane {
                Some(a) if a.len() >= w * h => ((a[y * w + x] as u32 * 65535 + ((1u32 << bd) - 1) / 2) / ((1u32 << bd) - 1)).min(65535) as u16,
                _ => 65535,
            };
        }
    });
    Ok(from_rgba16(px, w as u32, h as u32, f.color_primaries, f.transfer_characteristics))
}
