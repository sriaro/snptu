struct U {
    sel: vec4<f32>,
    screen: vec2<f32>,
    has_sel: f32,
    handle: f32,
    pill: vec4<f32>,
    txt: vec4<f32>,
    extra: vec4<f32>,
    layer: vec4<f32>,
    ui: vec4<f32>,
    glass: vec4<f32>,
    glass_extra: vec4<f32>,
    glass2: vec4<f32>,
    loupe: vec4<f32>,
    loupe_src: vec4<f32>,
    loupe_info: vec4<f32>,
    theme: vec4<f32>,
    fx: vec4<f32>,
    fx2: vec4<f32>,
    split: vec4<f32>,
    ghost: vec4<f32>,
    ghost_glass: vec4<f32>,
    ghost_glass2: vec4<f32>,
    ghost_fx: vec4<f32>,
    ghost_r: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var label: texture_2d<f32>;
@group(0) @binding(4) var layer: texture_2d<f32>;
@group(0) @binding(5) var ui: texture_2d<f32>;
@group(0) @binding(6) var loupe_tex: texture_2d<f32>;
@group(0) @binding(7) var ghost_tex: texture_2d<f32>;

struct VO {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VO {
    var p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var o: VO;
    o.pos = vec4(p[i], 0.0, 1.0);
    o.uv = vec2((p[i].x + 1.0) * 0.5, (1.0 - p[i].y) * 0.5);
    return o;
}

fn box_dist(px: vec2<f32>, lo: vec2<f32>, hi: vec2<f32>, radius: f32) -> f32 {
    let center = (lo + hi) * 0.5;
    let half = (hi - lo) * 0.5;
    let mh = min(half.x, half.y);
    let r = min(radius * 1.3, mh);
    let q = abs(px - center) - half + vec2(r);
    let inner = min(max(q.x, q.y), 0.0);
    if (r < 0.5) {
        return length(max(q, vec2(0.0))) + inner - r;
    }
    let n = select(4.0, 2.6, r >= mh - 0.01);
    let qp = max(q, vec2(0.0));
    let nrm = pow(pow(qp.x, n) + pow(qp.y, n), 1.0 / n);
    return nrm + inner - r;
}

fn over(dst: vec3<f32>, src: vec4<f32>) -> vec3<f32> {
    return src.rgb + dst * (1.0 - src.a);
}

fn blurred_backdrop(p: vec2<f32>) -> vec3<f32> {
    var acc = vec3(0.0);
    for (var i = 0; i < 24; i++) {
        let fi = f32(i) + 0.5;
        let r = u.glass_extra.y * sqrt(fi / 24.0);
        let a = fi * 2.399963;
        let q = clamp(p + vec2(cos(a), sin(a)) * r, vec2(0.0), u.screen - vec2(1.0));
        let c = textureSampleLevel(tex, samp, q / u.screen, 0.0).rgb;
        let inside = u.has_sel > 0.5 && q.x >= u.sel.x && q.x < u.sel.z && q.y >= u.sel.y && q.y < u.sel.w;
        acc += c * select(0.45, 1.0, inside);
    }
    return acc / 24.0;
}

fn glass_color(p: vec2<f32>) -> vec3<f32> {
    let b = blurred_backdrop(p);
    let l = dot(b, vec3(0.299, 0.587, 0.114));
    let vivid = mix(vec3(l), b, 1.4);
    if (u.theme.x > 0.5) {
        return mix(vivid, vec3(0.95, 0.97, 1.0), 0.74);
    }
    let tinted = mix(vivid, vec3(0.045, 0.063, 0.106), 0.58);
    return tinted + vec3(0.055);
}

@fragment
fn fs(in: VO) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, in.uv).rgb;
    let dim = c * 0.45;
    let has = u.has_sel > 0.5;
    if (!has && u.loupe.z <= 0.0) {
        return vec4(dim, 1.0);
    }
    let px = in.uv * u.screen;
    let dist = select(1.0e6, box_dist(px, u.sel.xy, u.sel.zw, 0.0), has);

    var col = select(dim, c, dist < 0.0);

    if (u.layer.z > u.layer.x) {
        let lp = px - u.layer.xy;
        let dims = vec2<f32>(textureDimensions(layer));
        if (lp.x >= 0.0 && lp.y >= 0.0 && lp.x < dims.x && lp.y < dims.y) {
            col = over(col, textureLoad(layer, vec2<i32>(lp), 0));
        }
    }

    let line = clamp(1.25 - abs(dist), 0.0, 1.0);
    let accent = vec3(0.36, 0.55, 1.0);
    col = mix(col, accent, line);

    if (u.handle > 0.0) {
        let lo = u.sel.xy;
        let hi = u.sel.zw;
        let mid = (lo + hi) * 0.5;
        var pts = array<vec2<f32>, 8>(
            lo, vec2(mid.x, lo.y), vec2(hi.x, lo.y), vec2(hi.x, mid.y),
            hi, vec2(mid.x, hi.y), vec2(lo.x, hi.y), vec2(lo.x, mid.y));
        var hd = 1.0e9;
        for (var i = 0; i < 8; i++) {
            hd = min(hd, length(px - pts[i]) - u.handle);
        }
        col = mix(col, accent, clamp(0.5 - hd, 0.0, 1.0));
        col = mix(col, vec3(1.0), clamp(-hd - 1.0, 0.0, 1.0));
    }

    if (u.pill.z > u.pill.x) {
        let pd = box_dist(px, u.pill.xy, u.pill.zw, u.extra.x);
        let cover = clamp(0.5 - pd, 0.0, 1.0);
        if (cover > 0.0) {
            col = mix(col, glass_color(px), cover);
        }
        col = mix(col, vec3(1.0), clamp(1.0 - abs(pd + 0.5), 0.0, 1.0) * 0.22);
        let tp = px - u.txt.xy;
        if (tp.x >= 0.0 && tp.y >= 0.0 && tp.x < u.txt.z && tp.y < u.txt.w) {
            let a = textureLoad(label, vec2<i32>(tp), 0).r;
            col = mix(col, select(vec3(0.94, 0.95, 0.98), vec3(0.08, 0.12, 0.2), u.theme.x > 0.5), a * cover);
        }
    }

    let rise = vec2(0.0, u.fx.y);
    let pm = px - rise;
    let side = u.split.y;
    let on_panel = (side > 0.0 && pm.y >= u.split.x) || (side < 0.0 && pm.y < u.split.x);
    let slide = vec2(0.0, side * u.fx2.y);
    let gpm = px - vec2(0.0, u.ghost_fx.y);
    let gside = u.ghost_fx.w;
    let g_on = gside == 0.0 || (gside > 0.0 && gpm.y >= u.ghost_fx.z) || (gside < 0.0 && gpm.y < u.ghost_fx.z);
    let gp = gpm + vec2(0.0, gside * u.ghost_r.z);

    if (u.ghost_glass.z > 0.0) {
        let gd = box_dist(gp, u.ghost_glass.xy, u.ghost_glass.xy + u.ghost_glass.zw, u.ghost_r.x);
        let cover = clamp(0.5 - gd, 0.0, 1.0) * u.ghost_fx.x;
        if (cover > 0.0) {
            col = mix(col, glass_color(px), cover);
        }
    }
    if (u.ghost_glass2.z > 0.0 && g_on) {
        let gd2 = box_dist(gp, u.ghost_glass2.xy, u.ghost_glass2.xy + u.ghost_glass2.zw, u.ghost_r.y);
        let cover2 = clamp(0.5 - gd2, 0.0, 1.0) * u.ghost_fx.x;
        if (cover2 > 0.0) {
            col = mix(col, glass_color(px), cover2);
        }
    }

    if (u.glass.z > 0.0) {
        let gd = box_dist(pm, u.glass.xy, u.glass.xy + u.glass.zw, u.glass_extra.x);
        let cover = clamp(0.5 - gd, 0.0, 1.0) * u.fx.x;
        if (cover > 0.0) {
            col = mix(col, glass_color(px), cover);
        }
    }

    if (u.glass2.z > 0.0 && on_panel) {
        let gd2 = box_dist(pm + slide, u.glass2.xy, u.glass2.xy + u.glass2.zw, u.glass_extra.z);
        let cover2 = clamp(0.5 - gd2, 0.0, 1.0) * u.fx.x * u.fx2.x;
        if (cover2 > 0.0) {
            col = mix(col, glass_color(px), cover2);
        }
    }

    if (u.loupe.z > 0.0) {
        let lo = u.loupe.xy;
        let hi = lo + vec2(u.loupe.z, u.loupe.w + u.loupe_info.w);
        let ld = box_dist(px, lo, hi, u.loupe_src.w);
        let lcover = clamp(0.5 - ld, 0.0, 1.0);
        if (lcover > 0.0) {
            let lp = px - lo;
            var lc: vec3<f32>;
            if (lp.y < u.loupe.w) {
                let cell = u.loupe_src.z;
                let off = lp - u.loupe.zw * 0.5;
                let idx = floor(off / cell + 0.5);
                lc = textureSampleLevel(tex, samp, (u.loupe_src.xy + idx + vec2(0.5)) / u.screen, 0.0).rgb;
                let on_x = abs(idx.x) < 0.5;
                let on_y = abs(idx.y) < 0.5;
                if (on_x != on_y) {
                    lc = mix(lc, vec3(0.55, 0.66, 0.9), 0.5);
                }
                if (on_x && on_y) {
                    let local = abs(off - idx * cell);
                    if (max(local.x, local.y) > cell * 0.5 - 1.6) {
                        lc = vec3(0.0);
                    } else if (max(local.x, local.y) > cell * 0.5 - 3.0) {
                        lc = vec3(1.0);
                    }
                }
            } else {
                lc = glass_color(px);
                let ip = px - u.loupe_info.xy;
                if (ip.x >= 0.0 && ip.y >= 0.0 && ip.x < u.loupe_info.z && ip.y < u.loupe_info.w) {
                    lc = over(lc, textureLoad(loupe_tex, vec2<i32>(ip), 0));
                }
                lc = mix(lc, vec3(1.0), clamp(1.0 - abs(lp.y - u.loupe.w), 0.0, 1.0) * select(0.16, 0.5, u.theme.x > 0.5));
            }
            col = mix(col, lc, lcover);
            col = mix(col, vec3(1.0), clamp(1.0 - abs(ld + 0.5), 0.0, 1.0) * 0.3);
        }
    }

    if (u.ghost.z > 0.0 && g_on) {
        let gt = gp - u.ghost.xy;
        if (gt.x >= 0.0 && gt.y >= 0.0 && gt.x < u.ghost.z && gt.y < u.ghost.w) {
            col = over(col, textureLoad(ghost_tex, vec2<i32>(gt), 0) * u.ghost_fx.x);
        }
    }

    if (u.ui.z > 0.0) {
        var opacity = u.fx.x;
        var up = pm - u.ui.xy;
        if (on_panel) {
            opacity = opacity * u.fx2.x;
            up = up + slide;
        }
        if (up.x >= 0.0 && up.y >= 0.0 && up.x < u.ui.z && up.y < u.ui.w) {
            col = over(col, textureLoad(ui, vec2<i32>(up), 0) * opacity);
        }
    }
    return vec4(col, 1.0);
}
