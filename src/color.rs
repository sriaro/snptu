
use crate::convert::Kind;
use anyhow::{Result, anyhow, bail};
use moxcms::{ColorProfile, Layout, RenderingIntent, TransformOptions};

const MAX_ICC: usize = 8 << 20;

const MAX_TAGS: usize = 100;

fn parse(icc: &[u8]) -> Result<ColorProfile> {
    let count = icc.get(128..132).map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize);
    if count > MAX_TAGS || 132 + 12 * count > icc.len() {
        bail!("colour profile: damaged tag table");
    }
    let mut copy = icc.to_vec();
    for tag in copy[132..132 + 12 * count].as_chunks_mut::<12>().0.iter_mut() {
        if matches!(&tag[..4], b"desc" | b"cprt" | b"dmnd" | b"dmdd" | b"vued" | b"targ") {
            tag[0] = 0;
        }
    }
    ColorProfile::new_from_slice(&copy).map_err(|e| anyhow!("colour profile: {e}"))
}

pub fn embeds(kind: Kind) -> bool {
    !matches!(kind, Kind::Gif | Kind::Ico)
}

fn options(src: &ColorProfile) -> TransformOptions {
    let mut o = TransformOptions::default();
    if src.is_matrix_shaper() {
        o.rendering_intent = RenderingIntent::RelativeColorimetric;
    }
    o
}

pub fn usable(icc: Option<Vec<u8>>) -> Option<Vec<u8>> {
    let icc = icc.filter(|b| b.len() > 128 && b.len() <= MAX_ICC && b.get(16..20) == Some(b"RGB "))?;
    let src = parse(&icc).ok()?;
    let srgb = ColorProfile::new_srgb();
    let t = src.create_transform_8bit(Layout::Rgb, &srgb, Layout::Rgb, options(&src)).ok()?;
    let mut probe = Vec::with_capacity(6 * 6 * 6 * 3);
    for r in 0..6u8 {
        for g in 0..6u8 {
            for b in 0..6u8 {
                probe.extend_from_slice(&[r * 51, g * 51, b * 51]);
            }
        }
    }
    let mut out = vec![0u8; probe.len()];
    t.transform(&probe, &mut out).ok()?;
    let moved = probe.iter().zip(&out).any(|(a, b)| a.abs_diff(*b) > 2);
    moved.then_some(icc)
}

pub fn from_cicp(cp: u8, tc: u8) -> Option<Vec<u8>> {
    if !matches!(tc, 0 | 1 | 6 | 13 | 14 | 15) {
        return None;
    }
    let p = match cp {
        12 => ColorProfile::new_display_p3(),
        9 => ColorProfile::new_bt2020(),
        _ => return None,
    };
    p.encode().ok()
}

pub fn to_srgb_8(rgba: &[u8], w: u32, icc: &[u8]) -> Result<Vec<u8>> {
    let src = parse(icc)?;
    let t = src
        .create_transform_8bit(Layout::Rgba, &ColorProfile::new_srgb(), Layout::Rgba, options(&src))
        .map_err(|e| anyhow!("colour profile: {e}"))?;
    let mut out = vec![0u8; rgba.len()];
    let row = (w as usize * 4).max(4);
    crate::par::rows(&mut out, row, |i, dst| {
        let src_row = &rgba[i * row..i * row + dst.len()];
        if t.transform(src_row, dst).is_err() {
            dst.copy_from_slice(src_row);
        }
    });
    Ok(out)
}

pub fn to_srgb_16(px: &[u16], w: u32, icc: &[u8]) -> Result<Vec<u16>> {
    let src = parse(icc)?;
    let t = src
        .create_transform_16bit(Layout::Rgba, &ColorProfile::new_srgb(), Layout::Rgba, options(&src))
        .map_err(|e| anyhow!("colour profile: {e}"))?;
    let mut out = vec![0u16; px.len()];
    let row = (w as usize * 4).max(4);
    crate::par::rows(&mut out, row, |i, dst| {
        let src_row = &px[i * row..i * row + dst.len()];
        if t.transform(src_row, dst).is_err() {
            dst.copy_from_slice(src_row);
        }
    });
    Ok(out)
}
