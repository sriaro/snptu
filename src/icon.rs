
use std::f32::consts::TAU;

use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LineJoin, LinearGradient, Paint, PathBuilder, Pixmap, Point, Shader,
    SpreadMode, Stroke, Transform,
};

const TILE_TOP: [u8; 3] = [20, 184, 170];
const TILE_BOTTOM: [u8; 3] = [59, 120, 245];
const GLYPH: [u8; 3] = [255, 255, 255];
const SQUIRCLE_POWER: f32 = 5.0;

fn squircle(x: f32, y: f32, w: f32, h: f32) -> Option<tiny_skia::Path> {
    const STEPS: usize = 720;
    let (cx, cy, a, b) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
    let e = 2.0 / SQUIRCLE_POWER;
    let mut pb = PathBuilder::new();
    for i in 0..STEPS {
        let t = TAU * i as f32 / STEPS as f32;
        let (c, s) = (t.cos(), t.sin());
        let (px, py) = (cx + a * c.signum() * c.abs().powf(e), cy + b * s.signum() * s.abs().powf(e));
        if i == 0 {
            pb.move_to(px, py);
        } else {
            pb.line_to(px, py);
        }
    }
    pb.close();
    pb.finish()
}

fn solid(c: [u8; 3], a: u8) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(c[0], c[1], c[2], a);
    p.anti_alias = true;
    p
}

pub fn draw(pix: &mut Pixmap) {
    let s = pix.width() as f32;
    let id = Transform::identity();
    pix.fill(Color::TRANSPARENT);

    let inset = 0.04 * s;
    if let Some(tile) = squircle(inset, inset, s - 2.0 * inset, s - 2.0 * inset) {
        let p = Paint {
            anti_alias: true,
            shader: LinearGradient::new(
                Point::from_xy(inset, inset),
                Point::from_xy(s - inset, s - inset),
                vec![
                    GradientStop::new(0.0, Color::from_rgba8(TILE_TOP[0], TILE_TOP[1], TILE_TOP[2], 255)),
                    GradientStop::new(1.0, Color::from_rgba8(TILE_BOTTOM[0], TILE_BOTTOM[1], TILE_BOTTOM[2], 255)),
                ],
                SpreadMode::Pad,
                id,
            )
            .unwrap_or(Shader::SolidColor(Color::from_rgba8(TILE_BOTTOM[0], TILE_BOTTOM[1], TILE_BOTTOM[2], 255))),
            ..Default::default()
        };
        pix.fill_path(&tile, &p, FillRule::Winding, id, None);
        let edge = Stroke { width: (s / 128.0).max(1.0), ..Default::default() };
        pix.stroke_path(&tile, &solid([255, 255, 255], 46), &edge, id, None);
    }

    let m = 0.27 * s;
    let arm = 0.17 * s;
    let stroke = Stroke {
        width: 0.075 * s,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Default::default()
    };
    let (lo, hi) = (m, s - m);
    for (cx, cy, dx, dy) in [(lo, lo, 1.0, 1.0), (hi, lo, -1.0, 1.0), (lo, hi, 1.0, -1.0), (hi, hi, -1.0, -1.0)] {
        let mut pb = PathBuilder::new();
        pb.move_to(cx + dx * arm, cy);
        pb.line_to(cx, cy);
        pb.line_to(cx, cy + dy * arm);
        if let Some(path) = pb.finish() {
            pix.stroke_path(&path, &solid(GLYPH, 255), &stroke, id, None);
        }
    }
    if let Some(dot) = PathBuilder::from_circle(s / 2.0, s / 2.0, 0.062 * s) {
        pix.fill_path(&dot, &solid(GLYPH, 255), FillRule::Winding, id, None);
    }
}

pub fn render(size: u32) -> Pixmap {
    let mut pix = Pixmap::new(size, size).expect("non-zero icon size");
    draw(&mut pix);
    pix
}

#[allow(dead_code)]
pub fn render_rgba(size: u32) -> Vec<u8> {
    let mut data = render(size).take();
    for px in data.as_chunks_mut::<4>().0 {
        let a = px[3] as u32;
        if a != 0 && a != 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    data
}
