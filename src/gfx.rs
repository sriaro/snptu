
use crate::settings::{EASE_OUT, Tween};
use crate::text::Text;
use anyhow::{Context, Result, bail};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Instant;
use winit::window::Window;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    sel: [f32; 4],
    screen: [f32; 2],
    has_sel: f32,
    handle: f32,
    pill: [f32; 4],
    txt: [f32; 4],
    extra: [f32; 4],
    layer: [f32; 4],
    ui: [f32; 4],
    glass: [f32; 4],
    glass_extra: [f32; 4],
    glass2: [f32; 4],
    loupe: [f32; 4],
    loupe_src: [f32; 4],
    loupe_info: [f32; 4],
    theme: [f32; 4],
    fx: [f32; 4],
    fx2: [f32; 4],
    split: [f32; 4],
    ghost: [f32; 4],
    ghost_glass: [f32; 4],
    ghost_glass2: [f32; 4],
    ghost_fx: [f32; 4],
    ghost_r: [f32; 4],
}

const UI_IN_MS: f32 = 190.0;
const UI_OUT_MS: f32 = 140.0;
const UI_RISE: f32 = 7.0;
const UI_SLIDE: f32 = 12.0;

struct UiFade {
    level: Tween,
    ms: f32,
    leaving: bool,
}

impl UiFade {
    fn new() -> Self {
        Self { level: Tween::at(0.0), ms: UI_IN_MS, leaving: false }
    }

    fn full() -> Self {
        Self { level: Tween::at(1.0), ms: UI_IN_MS, leaving: false }
    }

    fn going_from(level: f32, now: Instant) -> Self {
        let mut f = Self { level: Tween::at(level), ms: UI_IN_MS, leaving: false };
        f.hide(true, now);
        f
    }

    fn coming_from(level: f32, now: Instant) -> Self {
        let mut f = Self { level: Tween::at(level), ms: UI_IN_MS, leaving: false };
        f.level.retarget(1.0, now, f.ms, EASE_OUT);
        f
    }

    fn show(&mut self, shown: bool, now: Instant) {
        if !shown {
            self.level = Tween::at(0.0);
        }
        if !shown || self.leaving {
            self.level.retarget(1.0, now, self.ms, EASE_OUT);
            self.ms = UI_IN_MS;
            self.leaving = false;
        }
    }

    fn hide(&mut self, shown: bool, now: Instant) {
        if !shown || self.leaving {
            return;
        }
        self.leaving = true;
        self.level.retarget(0.0, now, self.ms, EASE_OUT);
        self.ms = UI_OUT_MS;
    }

    fn level(&self, now: Instant) -> f32 {
        self.level.value(now, self.ms, EASE_OUT).clamp(0.0, 1.0)
    }

    fn moving(&self, now: Instant) -> bool {
        !self.level.done(now, self.ms)
    }

    fn pending(&self, drawn: f32, now: Instant) -> bool {
        self.moving(now) || self.level(now) != drawn
    }

    fn gone(&self, now: Instant) -> bool {
        self.leaving && !self.moving(now)
    }
}

fn toolbar_fx(level: f32, scale: f32) -> [f32; 4] {
    [level, (1.0 - level) * UI_RISE * scale, 0.0, 0.0]
}

fn panel_side(pill: [f32; 4], pill2: [f32; 4]) -> f32 {
    if pill2[2] <= 0.0 {
        0.0
    } else if pill2[1] > pill[1] {
        1.0
    } else {
        -1.0
    }
}

fn panel_split(origin_y: f32, pill: [f32; 4], pill2: [f32; 4]) -> f32 {
    origin_y + if pill2[1] > pill[1] { (pill[1] + pill[3] + pill2[1]) / 2.0 } else { (pill2[1] + pill2[3] + pill[1]) / 2.0 }
}

struct Ghost {
    origin: [f32; 2],
    pill: [f32; 4],
    pill2: [f32; 4],
    fade: UiFade,
    whole: bool,
    split: f32,
    side: f32,
}

#[derive(Default, Clone, Copy, Debug, PartialEq)]
struct UiDraw {
    ui: [f32; 4],
    glass: [f32; 4],
    glass_extra: [f32; 4],
    glass2: [f32; 4],
    fx: [f32; 4],
    fx2: [f32; 4],
    split: [f32; 4],
    ghost: [f32; 4],
    ghost_glass: [f32; 4],
    ghost_glass2: [f32; 4],
    ghost_fx: [f32; 4],
    ghost_r: [f32; 4],
}

struct ToolbarFx {
    origin: Option<[f32; 2]>,
    pill: [f32; 4],
    pill2: [f32; 4],
    fade: UiFade,
    drawn: f32,
    panel: UiFade,
    panel_drawn: f32,
    ghost: Option<Ghost>,
    ghost_drawn: f32,
}

impl ToolbarFx {
    fn new() -> Self {
        Self { origin: None, pill: [0.0; 4], pill2: [0.0; 4], fade: UiFade::new(), drawn: 0.0, panel: UiFade::full(), panel_drawn: 1.0, ghost: None, ghost_drawn: 0.0 }
    }

    fn place(&mut self, origin: [f32; 2], pill: [f32; 4], pill2: [f32; 4], drawn: bool, now: Instant) -> bool {
        let mut keep = false;
        match self.origin {
            Some(old) if !self.fade.leaving => {
                let screen = |o: [f32; 2], p: [f32; 4]| [o[0] + p[0], o[1] + p[1], p[2], p[3]];
                let moved = screen(old, self.pill).iter().zip(screen(origin, pill)).any(|(a, b)| (a - b).abs() >= 0.5);
                let (had, has) = (self.pill2[2] > 0.0, pill2[2] > 0.0);
                if moved {
                    let from = self.fade.level(now);
                    self.ghost = Some(Ghost { origin: old, pill: self.pill, pill2: self.pill2, fade: UiFade::going_from(from, now), whole: true, split: 0.0, side: 0.0 });
                    self.fade.show(false, now);
                    self.panel = UiFade::full();
                    keep = true;
                } else if had != has {
                    let from = self.panel.level(now);
                    let seen = if has { self.ghost.as_ref().filter(|g| !g.whole && g.pill2[2] > 0.0).map_or(0.0, |g| g.fade.level(now)) } else { 0.0 };
                    let (side, split) = if has {
                        (panel_side(pill, pill2), panel_split(origin[1], pill, pill2))
                    } else {
                        (panel_side(self.pill, self.pill2), panel_split(old[1], self.pill, self.pill2))
                    };
                    self.ghost = Some(Ghost { origin: old, pill: self.pill, pill2: self.pill2, fade: UiFade::going_from(from, now), whole: false, split, side });
                    self.panel = UiFade::coming_from(seen, now);
                    keep = true;
                }
            }
            _ => {
                self.ghost = None;
                self.fade.show(self.origin.is_some(), now);
                self.panel = UiFade::full();
            }
        }
        if keep && !drawn {
            self.ghost = None;
            keep = false;
        }
        self.origin = Some(origin);
        self.pill = pill;
        self.pill2 = pill2;
        keep
    }

    fn hide(&mut self, now: Instant) {
        self.fade.hide(self.origin.is_some(), now);
    }

    fn clear(&mut self) {
        *self = Self::new();
    }

    fn animating(&self, now: Instant) -> bool {
        self.origin.is_some()
            && (self.fade.pending(self.drawn, now)
                || self.panel.pending(self.panel_drawn, now)
                || self.ghost.as_ref().is_some_and(|g| g.fade.pending(self.ghost_drawn, now)))
    }

    fn frame(&mut self, now: Instant, scale: f32, ui_size: [f32; 2], ghost_size: [f32; 2]) -> UiDraw {
        let level = self.fade.level(now);
        self.drawn = level;
        let panel = self.panel.level(now);
        self.panel_drawn = panel;
        if self.fade.gone(now) {
            self.clear();
        } else if self.ghost.as_ref().is_some_and(|g| g.fade.gone(now)) {
            self.ghost = None;
        }
        let ghost_level = self.ghost.as_ref().map(|g| g.fade.level(now));
        self.ghost_drawn = ghost_level.unwrap_or(0.0);
        let rect = |o: [f32; 2], p: [f32; 4]| [o[0] + p[0], o[1] + p[1], p[2], p[3]];
        let mut d = UiDraw { fx: toolbar_fx(level, scale), glass_extra: [0.0, 14.0 * scale, self.pill2[3] / 2.0, 0.0], ..Default::default() };
        if let Some(o) = self.origin.filter(|_| level > 0.002) {
            d.ui = [o[0], o[1], ui_size[0], ui_size[1]];
            d.glass = rect(o, self.pill);
            d.glass_extra[0] = self.pill[3] / 2.0;
            let own = panel_side(self.pill, self.pill2);
            if own != 0.0 {
                d.glass2 = rect(o, self.pill2);
            }
            let (side, split) = if own != 0.0 {
                (own, panel_split(o[1], self.pill, self.pill2))
            } else {
                self.ghost.as_ref().filter(|g| !g.whole).map_or((0.0, 0.0), |g| (g.side, g.split))
            };
            d.split = [split, side, 0.0, 0.0];
            d.fx2 = [panel, if own != 0.0 { (1.0 - panel) * UI_SLIDE * scale } else { 0.0 }, 0.0, 0.0];
        }
        if let (Some(g), Some(gl)) = (&self.ghost, ghost_level.filter(|gl| *gl > 0.002)) {
            d.ghost = [g.origin[0], g.origin[1], ghost_size[0], ghost_size[1]];
            if g.whole {
                d.ghost_glass = rect(g.origin, g.pill);
            }
            if g.pill2[2] > 0.0 {
                d.ghost_glass2 = rect(g.origin, g.pill2);
            }
            d.ghost_fx = if g.whole {
                [gl, (1.0 - gl) * UI_RISE * scale, 0.0, 0.0]
            } else {
                [gl * level, (1.0 - level) * UI_RISE * scale, g.split, g.side]
            };
            let slide = if g.pill2[2] > 0.0 { (1.0 - gl) * UI_SLIDE * scale } else { 0.0 };
            d.ghost_r = [g.pill[3] / 2.0, g.pill2[3] / 2.0, slide, 0.0];
        }
        d
    }
}

#[derive(Clone, Copy)]
pub struct LoupeGpu {
    pub area: [f32; 4],
    pub info: [f32; 4],
    pub src: (f32, f32),
    pub cell: f32,
    pub radius: f32,
}

const LABEL_W: u32 = 256;
const LABEL_H: u32 = 64;

static LOUPE_TEX_GEN: AtomicU64 = AtomicU64::new(0);

pub fn loupe_tex_gen() -> u64 {
    LOUPE_TEX_GEN.load(Ordering::Relaxed)
}

fn blank_loupe_tex(device: &wgpu::Device) -> Tex {
    LOUPE_TEX_GEN.fetch_add(1, Ordering::Relaxed);
    make_tex(device, 1, 1, wgpu::TextureFormat::Rgba8Unorm)
}

const SURFACE_RETRIES: u8 = 3;

struct Tex {
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    w: u32,
    h: u32,
}

fn make_tex(device: &wgpu::Device, w: u32, h: u32, format: wgpu::TextureFormat) -> Tex {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    Tex { tex, view, w: w.max(1), h: h.max(1) }
}

fn label_tex_size(have: (u32, u32), need: (u32, u32), max: u32) -> Option<(u32, u32)> {
    let grown = (have.0.max(need.0).min(max), have.1.max(need.1).min(max));
    (grown != have).then_some(grown)
}

fn tex_entry(binding: u32, filterable: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

pub struct Gfx {
    pub adapter_name: String,
    instance: wgpu::Instance,
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    surface_losses: u8,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
    bind_group: Option<wgpu::BindGroup>,
    frame: Option<Tex>,
    frame_size: [f32; 2],
    max_dim: u32,
    label_tex: Tex,
    layer: Tex,
    layer_origin: Option<[f32; 2]>,
    ui: Tex,
    ghost_tex: Tex,
    ui_written: bool,
    fx: ToolbarFx,
    loupe_tex: Tex,
    loupe: Option<LoupeGpu>,
    handle: f32,
    light: bool,
    text: Option<Arc<Text>>,
    label: Option<(u32, u32, u32, u32, u32)>,
    label_scratch: Vec<u8>,
    lost: Arc<AtomicBool>,
    scale: f32,
}

struct Opened {
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    name: String,
    max_dim: u32,
}

fn has_battery() -> bool {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut st = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut st) }.is_ok() && st.BatteryFlag != 128 && st.BatteryFlag != 255
}

fn request_device(adapter: &wgpu::Adapter) -> Result<(wgpu::Device, wgpu::Queue)> {
    let limits = adapter.limits();
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("snptu"),
        required_limits: limits,
        ..Default::default()
    }))
    .context("request device")
}

fn open_device(instance: &wgpu::Instance, surface: &wgpu::Surface<'_>) -> Result<Opened> {
    use wgpu::DeviceType::*;
    let laptop = has_battery();
    let mut candidates: Vec<wgpu::Adapter> = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::DX12))
        .into_iter()
        .filter(|a| a.is_surface_supported(surface))
        .collect();
    let rank = |t: wgpu::DeviceType| match (laptop, t) {
        (true, IntegratedGpu) | (false, DiscreteGpu) => 0,
        (true, DiscreteGpu) | (false, IntegratedGpu) => 1,
        (_, VirtualGpu) => 2,
        (_, Cpu) => 3,
        _ => 4,
    };
    candidates.sort_by_key(|a| rank(a.get_info().device_type));
    crate::log(&format!(
        "gpus ({}): {}",
        if laptop { "laptop: integrated first" } else { "desktop: discrete first" },
        candidates
            .iter()
            .map(|a| {
                let i = a.get_info();
                format!("{} [{:?}]", i.name, i.device_type)
            })
            .collect::<Vec<_>>()
            .join("; ")
    ));
    let fallback = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::None,
        compatible_surface: Some(surface),
        force_fallback_adapter: true,
        apply_limit_buckets: false,
    }))
    .ok();
    for adapter in candidates.into_iter().chain(fallback) {
        match request_device(&adapter) {
            Ok((device, queue)) => {
                let info = adapter.get_info();
                return Ok(Opened {
                    name: format!("{} [{:?}, {:?}]", info.name, info.device_type, info.backend),
                    max_dim: adapter.limits().max_texture_dimension_2d,
                    adapter,
                    device,
                    queue,
                });
            }
            Err(e) => crate::log(&format!("gpu {} unusable: {e:#}", adapter.get_info().name)),
        }
    }
    anyhow::bail!("no usable Direct3D 12 adapter")
}

pub struct Start {
    instance: wgpu::Instance,
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
}

impl Gfx {
    pub fn new(window: Arc<Window>) -> Result<Self> {
        Self::finish(Self::begin(window)?)
    }

    pub fn begin(window: Arc<Window>) -> Result<Start> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = wgpu::Backends::DX12;
        let instance = wgpu::Instance::new(desc);
        let surface = instance.create_surface(window.clone()).context("create surface")?;
        Ok(Start { instance, window, surface })
    }

    pub fn finish(start: Start) -> Result<Self> {
        let Start { instance, window, surface } = start;
        let Opened { adapter, device, queue, name: adapter_name, max_dim } = open_device(&instance, &surface)?;

        let lost = Arc::new(AtomicBool::new(false));
        let on_error = lost.clone();
        device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| {
            crate::log(&format!("wgpu error: {e}"));
            if !matches!(e, wgpu::Error::Validation { .. }) {
                on_error.store(true, Ordering::Relaxed);
            }
        }));
        let on_lost = lost.clone();
        device.set_device_lost_callback(move |reason, msg| {
            crate::log(&format!("gpu device lost ({reason:?}): {msg}"));
            on_lost.store(true, Ordering::Relaxed);
        });

        let caps = surface.get_capabilities(&adapter);
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: 1,
            height: 1,
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 1,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::include_wgsl!("overlay.wgsl"));
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex_entry(1, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                tex_entry(3, false),
                tex_entry(4, false),
                tex_entry(5, false),
                tex_entry(6, false),
                tex_entry(7, false),
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let rgba = wgpu::TextureFormat::Rgba8Unorm;
        Ok(Self {
            adapter_name,
            label_tex: make_tex(&device, LABEL_W, LABEL_H, wgpu::TextureFormat::R8Unorm),
            layer: make_tex(&device, 1, 1, rgba),
            ui: make_tex(&device, 1, 1, rgba),
            ghost_tex: make_tex(&device, 1, 1, rgba),
            ui_written: false,
            layer_origin: None,
            fx: ToolbarFx::new(),
            loupe_tex: blank_loupe_tex(&device),
            loupe: None,
            handle: 0.0,
            light: false,
            text: None,
            label: None,
            label_scratch: Vec::new(),
            lost,
            scale: 1.0,
            instance,
            window,
            surface,
            surface_losses: 0,
            device,
            queue,
            config,
            pipeline,
            layout,
            sampler,
            uniform,
            bind_group: None,
            frame: None,
            frame_size: [1.0, 1.0],
            max_dim,
        })
    }

    pub fn set_text(&mut self, text: Option<Arc<Text>>) {
        self.text = text;
    }

    pub fn set_scale(&mut self, scale: f64) {
        self.scale = scale.clamp(1.0, 4.0) as f32;
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 || (w == self.config.width && h == self.config.height) {
            return;
        }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);
    }

    fn rebuild_bind_group(&mut self) {
        let Some(frame) = &self.frame else { return };
        self.bind_group = Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&frame.view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&self.label_tex.view) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&self.layer.view) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&self.ui.view) },
                wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(&self.loupe_tex.view) },
                wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::TextureView(&self.ghost_tex.view) },
            ],
        }));
    }

    pub fn set_frame(&mut self, w: u32, h: u32, bgra: &[u8]) -> Result<()> {
        if w > self.max_dim || h > self.max_dim {
            bail!("screen {w}x{h} exceeds GPU texture limit {}", self.max_dim);
        }
        self.resize(w, h);
        let tex = make_tex(&self.device, w, h, wgpu::TextureFormat::Bgra8Unorm);
        self.write(&tex.tex, [0, 0, w, h], bgra);
        self.frame = Some(tex);
        self.frame_size = [w as f32, h as f32];
        self.rebuild_bind_group();
        Ok(())
    }

    fn write(&self, tex: &wgpu::Texture, r: [u32; 4], data: &[u8]) {
        let bpp = if tex.format() == wgpu::TextureFormat::R8Unorm { 1 } else { 4 };
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: tex,
                mip_level: 0,
                origin: wgpu::Origin3d { x: r[0], y: r[1], z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(r[2] * bpp),
                rows_per_image: Some(r[3]),
            },
            wgpu::Extent3d { width: r[2], height: r[3], depth_or_array_layers: 1 },
        );
    }

    pub fn set_layer(&mut self, w: u32, h: u32, origin: (f32, f32)) {
        self.layer = make_tex(&self.device, w, h, wgpu::TextureFormat::Rgba8Unorm);
        self.layer_origin = Some([origin.0, origin.1]);
        self.rebuild_bind_group();
    }

    pub fn set_light(&mut self, light: bool) {
        self.light = light;
    }

    pub fn set_handles(&mut self, radius: f32) {
        self.handle = radius;
    }

    pub fn clear_layer(&mut self) {
        self.layer_origin = None;
    }

    pub fn update_layer(&mut self, r: [u32; 4], data: &[u8]) {
        if r[2] == 0 || r[3] == 0 {
            return;
        }
        self.write(&self.layer.tex, r, data);
    }

    pub fn set_ui(&mut self, w: u32, h: u32, origin: (f32, f32), pill: [f32; 4], pill2: Option<[f32; 4]>) {
        self.set_ui_at(w, h, origin, pill, pill2, Instant::now());
    }

    fn set_ui_at(&mut self, w: u32, h: u32, origin: (f32, f32), pill: [f32; 4], pill2: Option<[f32; 4]>, now: Instant) {
        let old = std::mem::replace(&mut self.ui, make_tex(&self.device, w, h, wgpu::TextureFormat::Rgba8Unorm));
        let drawn = std::mem::replace(&mut self.ui_written, false);
        if self.fx.place([origin.0, origin.1], pill, pill2.unwrap_or([0.0; 4]), drawn, now) {
            self.ghost_tex = old;
        }
        self.rebuild_bind_group();
    }

    pub fn set_loupe(&mut self, loupe: Option<LoupeGpu>, info: Option<(&[u8], u32, u32)>) {
        self.loupe = loupe;
        let Some((data, w, h)) = info else { return };
        if self.loupe_tex.w != w || self.loupe_tex.h != h {
            self.loupe_tex = make_tex(&self.device, w, h, wgpu::TextureFormat::Rgba8Unorm);
            self.rebuild_bind_group();
        }
        self.write(&self.loupe_tex.tex, [0, 0, w, h], data);
    }

    pub fn clear_ui(&mut self) {
        self.fx.clear();
    }

    pub fn hide_ui(&mut self) {
        self.fx.hide(Instant::now());
    }

    pub fn ui_animating(&self) -> bool {
        self.fx.animating(Instant::now())
    }

    pub fn update_ui(&mut self, data: &[u8]) {
        let (w, h) = (self.ui.w, self.ui.h);
        self.write(&self.ui.tex, [0, 0, w, h], data);
        self.ui_written = true;
    }

    fn place_label(&mut self, r: [f32; 4]) -> ([f32; 4], [f32; 4]) {
        const NONE: ([f32; 4], [f32; 4]) = ([0.0; 4], [0.0; 4]);
        let Some(text) = self.text.clone() else { return NONE };
        let (lw, lh) = ((r[2] - r[0]).round() as u32, (r[3] - r[1]).round() as u32);
        let key = self.scale.to_bits();
        let (bw, bh) = match self.label {
            Some((cw, ch, k, w, h)) if cw == lw && ch == lh && k == key => (w, h),
            _ => {
                let bm = text.render(&format!("{lw} × {lh}"), 13.0 * self.scale);
                if let Some((tw, th)) = label_tex_size((self.label_tex.w, self.label_tex.h), (bm.w, bm.h), self.max_dim) {
                    self.label_tex = make_tex(&self.device, tw, th, wgpu::TextureFormat::R8Unorm);
                    self.rebuild_bind_group();
                }
                let (w, h) = (bm.w.min(self.label_tex.w), bm.h.min(self.label_tex.h));
                self.label_scratch.clear();
                for row in 0..h {
                    let start = (row * bm.w) as usize;
                    self.label_scratch.extend_from_slice(&bm.alpha[start..start + w as usize]);
                }
                self.queue.write_texture(
                    self.label_tex.tex.as_image_copy(),
                    &self.label_scratch,
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w), rows_per_image: Some(h) },
                    wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                );
                self.label = Some((lw, lh, key, w, h));
                (w, h)
            }
        };
        let sc = self.scale;
        let (pad_x, pad_y, gap) = (8.0 * sc, 4.0 * sc, 8.0 * sc);
        let (pw, ph) = (bw as f32 + 2.0 * pad_x, bh as f32 + 2.0 * pad_y);
        let m = crate::loupe::monitor_bounds((r[0], r[1]), (self.frame_size[0], self.frame_size[1]));
        let x = r[0].min(m[2] - pw).max(m[0]);
        let above = r[1] - gap - ph;
        let y = if above >= m[1] { above } else { r[1] + gap };
        ([x, y, x + pw, y + ph], [x + pad_x, y + pad_y, bw as f32, bh as f32])
    }

    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Relaxed)
    }

    pub fn probe_lost(&self) -> bool {
        let _ = self.device.poll(wgpu::PollType::Poll);
        self.is_lost()
    }

    pub fn release(&mut self) {
        self.bind_group = None;
        self.frame = None;
        self.layer_origin = None;
        self.clear_ui();
        self.label = None;
        self.layer = make_tex(&self.device, 1, 1, wgpu::TextureFormat::Rgba8Unorm);
        self.ui = make_tex(&self.device, 1, 1, wgpu::TextureFormat::Rgba8Unorm);
        self.ui_written = false;
        self.ghost_tex = make_tex(&self.device, 1, 1, wgpu::TextureFormat::Rgba8Unorm);
        self.label_scratch = Vec::new();
        self.loupe = None;
        self.loupe_tex = blank_loupe_tex(&self.device);
        self.config.width = 1;
        self.config.height = 1;
        self.surface.configure(&self.device, &self.config);
        let _ = self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
    }

    fn write_uniforms(&mut self, sel: Option<[f32; 4]>, now: Instant) {
        let (pill, txt) = sel.map_or(([0.0; 4], [0.0; 4]), |r| self.place_label(r));
        let layer = match (sel, self.layer_origin) {
            (Some(_), Some(o)) => [o[0], o[1], o[0] + self.layer.w as f32, o[1] + self.layer.h as f32],
            _ => [0.0; 4],
        };
        let d = self.fx.frame(now, self.scale, [self.ui.w as f32, self.ui.h as f32], [self.ghost_tex.w as f32, self.ghost_tex.h as f32]);
        let u = Uniforms {
            sel: sel.unwrap_or([0.0; 4]),
            screen: self.frame_size,
            has_sel: if sel.is_some() { 1.0 } else { 0.0 },
            handle: self.handle,
            pill,
            txt,
            extra: [(pill[3] - pill[1]) / 2.0, 0.0, 0.0, 0.0],
            layer,
            ui: d.ui,
            glass: d.glass,
            glass_extra: d.glass_extra,
            glass2: d.glass2,
            loupe: self.loupe.map_or([0.0; 4], |l| l.area),
            loupe_src: self.loupe.map_or([0.0; 4], |l| [l.src.0, l.src.1, l.cell, l.radius]),
            loupe_info: self.loupe.map_or([0.0; 4], |l| l.info),
            theme: [if self.light { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0],
            fx: d.fx,
            fx2: d.fx2,
            split: d.split,
            ghost: d.ghost,
            ghost_glass: d.ghost_glass,
            ghost_glass2: d.ghost_glass2,
            ghost_fx: d.ghost_fx,
            ghost_r: d.ghost_r,
        };
        self.queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&u));
    }

    fn render(&self, view: &wgpu::TextureView) {
        let Some(bind_group) = &self.bind_group else { return };
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([enc.finish()]);
    }

    pub fn draw(&mut self, sel: Option<[f32; 4]>) -> bool {
        if self.bind_group.is_none() {
            return false;
        }
        self.write_uniforms(sel, Instant::now());
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Lost => {
                self.surface_losses += 1;
                if self.surface_losses > SURFACE_RETRIES {
                    crate::log("gpu surface lost again and again");
                    self.lost.store(true, Ordering::Relaxed);
                    return false;
                }
                match self.instance.create_surface(self.window.clone()) {
                    Ok(s) => {
                        self.surface = s;
                        self.surface.configure(&self.device, &self.config);
                        return true;
                    }
                    Err(e) => {
                        crate::log(&format!("gpu surface lost, not made again: {e}"));
                        self.lost.store(true, Ordering::Relaxed);
                        return false;
                    }
                }
            }
            _ => {
                self.surface.configure(&self.device, &self.config);
                return true;
            }
        };
        self.surface_losses = 0;
        let view = frame.texture.create_view(&Default::default());
        self.render(&view);
        self.queue.present(frame);
        false
    }

}
