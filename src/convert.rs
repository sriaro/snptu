
use crate::anim::Stop;
use anyhow::{Context, Result, anyhow, bail};
use image::{ExtendedColorType, ImageEncoder};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};


pub(crate) const MAX_DECODE_SIDE: u32 = 65_535;
pub(crate) const MAX_DECODE_PIXELS: u64 = 128 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
const JXL_ALLOC_LIMIT: usize = 1024 * 1024 * 1024;
const MAX_DECODE_BYTES: u64 = 512 * 1024 * 1024;

pub(crate) fn check_size(w: u32, h: u32) -> Result<()> {
    if w == 0 || h == 0 {
        bail!("the image is empty");
    }
    if w > MAX_DECODE_SIDE || h > MAX_DECODE_SIDE || w as u64 * h as u64 > MAX_DECODE_PIXELS {
        bail!("the image is too large to open ({w} x {h})");
    }
    Ok(())
}

fn guarded<T>(what: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| Err(anyhow!("{what} failed (internal error)")))
}


#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Png,
    Jpeg,
    Webp,
    Gif,
    Ico,
}

impl Kind {
    pub const IMAGES: [Kind; 4] = [Kind::Png, Kind::Jpeg, Kind::Webp, Kind::Ico];
    pub const ANIMATIONS: [Kind; 2] = [Kind::Gif, Kind::Webp];

    pub fn ext(self) -> &'static str {
        match self {
            Kind::Png => "png",
            Kind::Jpeg => "jpg",
            Kind::Webp => "webp",
            Kind::Gif => "gif",
            Kind::Ico => "ico",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Png => "PNG",
            Kind::Jpeg => "JPEG",
            Kind::Webp => "WebP",
            Kind::Gif => "GIF",
            Kind::Ico => "ICO",
        }
    }

    pub fn has_lossless_switch(self) -> bool {
        matches!(self, Kind::Webp)
    }

    pub fn has_quality(self) -> bool {
        matches!(self, Kind::Jpeg | Kind::Webp)
    }

    pub fn from_extension(ext: &str) -> Option<Kind> {
        Some(match ext.to_ascii_lowercase().as_str() {
            "png" => Kind::Png,
            "jpg" | "jpeg" | "jfif" => Kind::Jpeg,
            "webp" => Kind::Webp,
            "gif" => Kind::Gif,
            "ico" => Kind::Ico,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PngLevel {
    Fast,
    Default,
    Best,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResizeFilter {
    Lanczos,
    CatmullRom,
    Bilinear,
    Nearest,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ResizeMode {
    Percent(f32),
    Fit(u32, u32),
    Exact(u32, u32),
}

#[derive(Clone, PartialEq, Debug)]
pub struct Params {
    pub kind: Kind,
    pub lossless: bool,
    pub quality: u8,
    pub effort: u8,
    pub png_level: PngLevel,
    pub deep: bool,
    pub keep_profile: bool,
    pub animation: bool,
    pub gif_speed: u8,
    pub resize: Option<(ResizeMode, ResizeFilter)>,
    pub background: [u8; 3],
}

impl Params {
    pub fn new(kind: Kind) -> Self {
        let (quality, effort) = match kind {
            Kind::Jpeg => (85, 5),
            Kind::Webp => (80, 5),
            _ => (90, 5),
        };
        Self {
            kind,
            lossless: false,
            quality,
            effort,
            png_level: PngLevel::Best,
            deep: true,
            keep_profile: true,
            animation: false,
            gif_speed: 10,
            resize: None,
            background: [255, 255, 255],
        }
    }
}


fn is_opaque(rgba: &[u8]) -> bool {
    rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255)
}

fn rgb_of(rgba: &[u8], bg: [u8; 3]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len() / 4 * 3);
    for p in rgba.as_chunks::<4>().0.iter() {
        let a = p[3] as u32;
        for k in 0..3 {
            out.push(((p[k] as u32 * a + bg[k] as u32 * (255 - a) + 127) / 255) as u8);
        }
    }
    out
}

fn check_buffer(w: u32, h: u32, rgba: &[u8]) -> Result<()> {
    if w == 0 || h == 0 || rgba.len() as u64 != w as u64 * h as u64 * 4 {
        bail!("not a {w} x {h} RGBA image");
    }
    if w as u64 * h as u64 > MAX_DECODE_PIXELS {
        bail!("{w} x {h} is too large");
    }
    Ok(())
}

pub(crate) fn resized_size(w: u32, h: u32, mode: ResizeMode) -> (u32, u32) {
    match mode {
        ResizeMode::Percent(p) => (((w as f32 * p / 100.0).round() as u32).max(1), ((h as f32 * p / 100.0).round() as u32).max(1)),
        ResizeMode::Fit(bw, bh) => {
            let k = (bw as f32 / w as f32).min(bh as f32 / h as f32);
            (((w as f32 * k).round() as u32).max(1), ((h as f32 * k).round() as u32).max(1))
        }
        ResizeMode::Exact(ew, eh) => (ew.max(1), eh.max(1)),
    }
}

pub(crate) fn check_resized(nw: u32, nh: u32) -> Result<()> {
    if nw > MAX_DECODE_SIDE || nh > MAX_DECODE_SIDE || nw as u64 * nh as u64 > MAX_DECODE_PIXELS {
        bail!("{nw} x {nh} is too large");
    }
    Ok(())
}

pub(crate) fn resize_alg(filter: ResizeFilter) -> fast_image_resize::ResizeAlg {
    use fast_image_resize::{FilterType, ResizeAlg};
    match filter {
        ResizeFilter::Lanczos => ResizeAlg::Convolution(FilterType::Lanczos3),
        ResizeFilter::CatmullRom => ResizeAlg::Convolution(FilterType::CatmullRom),
        ResizeFilter::Bilinear => ResizeAlg::Convolution(FilterType::Bilinear),
        ResizeFilter::Nearest => ResizeAlg::Nearest,
    }
}

pub(crate) fn png_options(level: PngLevel) -> (image::codecs::png::CompressionType, image::codecs::png::FilterType) {
    use image::codecs::png::{CompressionType, FilterType};
    match level {
        PngLevel::Fast => (CompressionType::Fast, FilterType::Sub),
        PngLevel::Default => (CompressionType::Default, FilterType::Adaptive),
        PngLevel::Best => (CompressionType::Best, FilterType::Adaptive),
    }
}

pub fn resize(rgba: Vec<u8>, w: u32, h: u32, mode: ResizeMode, filter: ResizeFilter) -> Result<(Vec<u8>, u32, u32)> {
    use fast_image_resize::{MulDiv, PixelType, ResizeOptions, Resizer, images::Image};
    let (nw, nh) = resized_size(w, h, mode);
    if (nw, nh) == (w, h) {
        return Ok((rgba, w, h));
    }
    check_resized(nw, nh)?;
    let opaque = is_opaque(&rgba);
    let mut src = Image::from_vec_u8(w, h, rgba, PixelType::U8x4).map_err(|e| anyhow!("resize: {e}"))?;
    let mut dst = Image::new(nw, nh, PixelType::U8x4);
    let muldiv = MulDiv::default();
    if !opaque {
        muldiv.multiply_alpha_inplace(&mut src).map_err(|e| anyhow!("resize: {e}"))?;
    }
    Resizer::new()
        .resize(&src, &mut dst, &ResizeOptions::new().resize_alg(resize_alg(filter)).use_alpha(false))
        .map_err(|e| anyhow!("resize: {e}"))?;
    if !opaque {
        muldiv.divide_alpha_inplace(&mut dst).map_err(|e| anyhow!("resize: {e}"))?;
    }
    Ok((dst.into_vec(), nw, nh))
}


pub fn encode(p: &Params, w: u32, h: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    guarded("encoding", || {
        check_buffer(w, h, rgba)?;
        if let Some((mode, filter)) = p.resize {
            let (px, nw, nh) = resize(rgba.to_vec(), w, h, mode, filter)?;
            return encode_exact(p, nw, nh, &px, None);
        }
        encode_exact(p, w, h, rgba, None)
    })
}

fn encode_in_profile(p: &Params, w: u32, h: u32, rgba: Vec<u8>, icc: Option<Vec<u8>>) -> Result<Vec<u8>> {
    guarded("encoding", || {
        check_buffer(w, h, &rgba)?;
        match icc {
            Some(icc) if p.keep_profile && crate::color::embeds(p.kind) => encode_exact(p, w, h, &rgba, Some(&icc)),
            Some(icc) => encode_exact(p, w, h, &crate::color::to_srgb_8(&rgba, w, &icc)?, None),
            None => encode_exact(p, w, h, &rgba, None),
        }
    })
}

fn encode_exact(p: &Params, w: u32, h: u32, rgba: &[u8], icc: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let opaque = is_opaque(rgba);
    match p.kind {
        Kind::Png => {
            let (level, filter) = png_options(p.png_level);
            let mut enc = image::codecs::png::PngEncoder::new_with_quality(&mut out, level, filter);
            if let Some(i) = icc {
                let _ = enc.set_icc_profile(i.to_vec());
            }
            if opaque {
                enc.write_image(&rgb_of(rgba, [0; 3]), w, h, ExtendedColorType::Rgb8)?;
            } else {
                enc.write_image(rgba, w, h, ExtendedColorType::Rgba8)?;
            }
        }
        Kind::Jpeg => {
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, p.quality.clamp(1, 100));
            if let Some(i) = icc {
                let _ = enc.set_icc_profile(i.to_vec());
            }
            enc.write_image(&rgb_of(rgba, p.background), w, h, ExtendedColorType::Rgb8)?;
        }
        Kind::Webp => {
            use zenwebp::{EncodeRequest, LosslessConfig, LossyConfig, PixelLayout};
            let method = p.effort.min(6);
            out = if p.lossless {
                let cfg = LosslessConfig::new().with_quality(100.0).with_method(method);
                let req = EncodeRequest::lossless(&cfg, rgba, PixelLayout::Rgba8, w, h);
                (if let Some(i) = icc { req.with_icc_profile(i) } else { req }).encode().map_err(|e| anyhow!("WebP: {e}"))?
            } else {
                if w > 16383 || h > 16383 {
                    bail!("lossy WebP can't be larger than 16383 px (lossless can be 16384)");
                }
                let cfg = LossyConfig::new().with_quality(p.quality as f32).with_method(method);
                let req = EncodeRequest::lossy(&cfg, rgba, PixelLayout::Rgba8, w, h);
                (if let Some(i) = icc { req.with_icc_profile(i) } else { req }).encode().map_err(|e| anyhow!("WebP: {e}"))?
            };
        }
        Kind::Gif => out = crate::anim::still_gif(p, w, h, rgba)?,
        Kind::Ico => {
            let (iw, ih, data) = if w > 256 || h > 256 {
                let (d, nw, nh) = resize(rgba.to_vec(), w, h, ResizeMode::Fit(256, 256), ResizeFilter::Lanczos)?;
                (nw, nh, std::borrow::Cow::Owned(d))
            } else {
                (w, h, std::borrow::Cow::Borrowed(rgba))
            };
            image::codecs::ico::IcoEncoder::new(&mut out).write_image(&data, iw, ih, ExtendedColorType::Rgba8)?;
        }
    }
    Ok(out)
}


pub enum Decoded {
    Eight(Vec<u8>, u32, u32),
    Deep(crate::deep::Deep),
}

impl Decoded {
    pub fn into_8bit(self) -> (Vec<u8>, u32, u32) {
        self.into_8bit_keeping(false)
    }

    pub fn into_8bit_keeping(self, keep: bool) -> (Vec<u8>, u32, u32) {
        match self {
            Decoded::Eight(px, w, h) => (px, w, h),
            Decoded::Deep(d) if keep && !d.is_hdr() => (crate::deep::reduce(&d.px), d.w, d.h),
            Decoded::Deep(d) => (crate::deep::to_8bit(&d), d.w, d.h),
        }
    }
}

pub struct Picture {
    pub px: Decoded,
    pub icc: Option<Vec<u8>>,
}

fn eight(r: Result<(Vec<u8>, u32, u32)>) -> Result<Decoded> {
    r.map(|(px, w, h)| Decoded::Eight(px, w, h))
}

fn plain(d: Decoded) -> Picture {
    Picture { px: d, icc: None }
}

pub fn decode(path: &Path) -> Result<(Vec<u8>, u32, u32)> {
    Ok(decode_picture(path)?.px.into_8bit())
}

pub fn decode_srgb(path: &Path) -> Result<(Vec<u8>, u32, u32)> {
    guarded("decoding", || {
        let pic = decode_unguarded(path)?;
        Ok(match (pic.px, pic.icc) {
            (Decoded::Eight(px, w, h), Some(icc)) => match crate::color::to_srgb_8(&px, w, &icc) {
                Ok(converted) => (converted, w, h),
                Err(_) => (px, w, h),
            },
            (Decoded::Deep(mut d), icc) => {
                d.icc = icc;
                (crate::deep::to_8bit(&d), d.w, d.h)
            }
            (px, _) => px.into_8bit(),
        })
    })
}

pub fn decode_picture(path: &Path) -> Result<Picture> {
    guarded("decoding", || decode_unguarded(path))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reader {
    Webp,
    Raw,
    Heic,
    Jpeg2000,
    Psd,
    Svg,
    Avif,
    Jxl,
    Ico,
    Image,
}

impl Reader {
    fn isolated(self) -> bool {
        matches!(self, Reader::Raw | Reader::Heic | Reader::Jpeg2000 | Reader::Psd | Reader::Svg | Reader::Avif | Reader::Jxl | Reader::Ico)
    }
}

fn pick_reader(path: &Path) -> Result<Reader> {
    use std::io::Read;
    let len = std::fs::metadata(path).with_context(|| format!("can't open {}", path.display()))?.len();
    if len > MAX_FILE_BYTES {
        bail!("the file is too large to open ({} MB)", len >> 20);
    }
    let mut head = [0u8; 16];
    let n = std::fs::File::open(path).with_context(|| format!("can't open {}", path.display()))?.read(&mut head)?;
    let head = &head[..n];
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let is_riff_webp = head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP";
    let is_avif = head.len() >= 12 && &head[4..8] == b"ftyp" && matches!(&head[8..12], b"avif" | b"avis");
    let is_jxl = head.starts_with(&[0xFF, 0x0A]) || head.get(4..8) == Some(b"JXL ");
    if is_riff_webp {
        return Ok(Reader::Webp);
    }
    if crate::readers::is_raw(&ext) {
        return Ok(Reader::Raw);
    }
    let brand = if head.len() >= 12 && &head[4..8] == b"ftyp" { &head[8..12] } else { &[][..] };
    let is_heic = matches!(brand, b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"hevm" | b"hevs" | b"mif1" | b"msf1")
        || matches!(ext.as_str(), "heic" | "heif" | "hif");
    if is_heic && !is_avif && ext != "avif" {
        return Ok(Reader::Heic);
    }
    if head.starts_with(&[0, 0, 0, 12, b'j', b'P', b' ', b' ']) || head.starts_with(&[0xFF, 0x4F, 0xFF, 0x51]) || matches!(ext.as_str(), "jp2" | "j2k" | "j2c" | "jpc") {
        return Ok(Reader::Jpeg2000);
    }
    if head.starts_with(b"8BPS") {
        return Ok(Reader::Psd);
    }
    if matches!(ext.as_str(), "svg" | "svgz") {
        return Ok(Reader::Svg);
    }
    if is_avif || ext == "avif" {
        return Ok(Reader::Avif);
    }
    if is_jxl || ext == "jxl" {
        return Ok(Reader::Jxl);
    }
    if head.starts_with(&[0, 0, 1, 0]) || ext == "ico" {
        return Ok(Reader::Ico);
    }
    Ok(Reader::Image)
}

fn run_reader(reader: Reader, path: &Path) -> Result<Picture> {
    let bytes = || -> Result<Vec<u8>> {
        use std::io::Read;
        let mut b = Vec::new();
        std::fs::File::open(path)
            .and_then(|f| f.take(MAX_FILE_BYTES + 1).read_to_end(&mut b))
            .with_context(|| format!("can't read {}", path.display()))?;
        if b.len() as u64 > MAX_FILE_BYTES {
            bail!("the file is too large to open");
        }
        Ok(b)
    };
    match reader {
        Reader::Webp => decode_webp(&bytes()?),
        Reader::Raw => Ok(plain(eight(crate::readers::raw(path))?)),
        Reader::Heic => crate::readers::heic_any(&bytes()?),
        Reader::Jpeg2000 => Ok(plain(eight(crate::readers::jpeg2000(&bytes()?))?)),
        Reader::Psd => Ok(plain(eight(crate::readers::psd(&bytes()?))?)),
        Reader::Svg => Ok(plain(eight(crate::readers::svg(&bytes()?))?)),
        Reader::Avif => decode_avif_any(&bytes()?),
        Reader::Jxl => decode_jxl(path),
        Reader::Ico | Reader::Image => decode_with_image(path),
    }
}

fn decode_unguarded(path: &Path) -> Result<Picture> {
    let reader = pick_reader(path)?;
    if reader.isolated() && !IN_HELPER.load(Ordering::Relaxed) {
        return decode_in_helper(path);
    }
    let mut pic = run_reader(reader, path)?;
    pic.icc = crate::color::usable(pic.icc.take());
    Ok(pic)
}


const HELPER_ARG: &str = "--decode-helper";
const HELPER_MEMORY: usize = 4 << 30;
const HELPER_TIMEOUT: Duration = Duration::from_secs(120);

static IN_HELPER: AtomicBool = AtomicBool::new(false);

struct HelperJob(windows::Win32::Foundation::HANDLE);

impl HelperJob {
    fn contain(child: &std::process::Child) -> Option<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{
            Foundation::HANDLE,
            System::JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
            },
        };
        unsafe {
            let job = HelperJob(CreateJobObjectW(None, windows::core::PCWSTR::null()).ok()?);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags =
                JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
            info.ProcessMemoryLimit = HELPER_MEMORY;
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .ok()?;
            AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle())).ok()?;
            Some(job)
        }
    }
}

impl Drop for HelperJob {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

enum Helped {
    Still(Picture),
    Anim(bool),
}

fn run_helper(path: &Path, anim: Option<&mut dyn crate::anim::Sink>, stop: &Stop) -> Result<Helped> {
    use std::{
        os::windows::io::AsRawHandle,
        process::{Command, Stdio},
        sync::mpsc,
    };
    let exe = std::env::current_exe().context("can't find Snptu.exe")?;
    let mut cmd = Command::new(exe);
    cmd.arg(HELPER_ARG).arg(path);
    if anim.is_some() {
        cmd.arg("--anim");
    }
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().context("can't start the decoder")?;
    let Some(job) = HelperJob::contain(&child) else {
        let _ = child.kill();
        let _ = child.wait();
        bail!("can't start the decoder safely");
    };
    if let Some(mut go) = child.stdin.take() {
        use std::io::Write;
        let _ = go.write_all(b"G");
    }
    let mut out = child.stdout.take().context("no decoder output")?;
    let (clock, ticks) = mpsc::channel::<bool>();
    let handle = child.as_raw_handle() as isize;
    let result = std::thread::scope(|s| {
        let watchdog = s.spawn(move || {
            watch(&ticks, HELPER_TIMEOUT, stop, || unsafe {
                let _ = windows::Win32::System::Threading::TerminateProcess(windows::Win32::Foundation::HANDLE(handle as *mut _), 1);
            })
        });
        let result = read_answer(&mut out, anim, &|waiting| {
            let _ = clock.send(waiting);
        });
        if result.is_err() {
            let _ = child.kill();
        }
        drop(out);
        let _ = child.wait();
        drop(clock);
        let _ = watchdog.join();
        result
    });
    drop(job);
    match result {
        Err(_) if stop() => Err(crate::anim::Stopped.into()),
        result => result,
    }
}

const WATCH_STEP: Duration = Duration::from_millis(100);

fn watch(ticks: &std::sync::mpsc::Receiver<bool>, limit: Duration, stop: &Stop, kill: impl FnOnce()) {
    use std::sync::mpsc::RecvTimeoutError;
    let mut since = Some(Instant::now());
    loop {
        if stop() || since.is_some_and(|t| t.elapsed() >= limit) {
            return kill();
        }
        match ticks.recv_timeout(WATCH_STEP.min(limit)) {
            Ok(waiting) => since = waiting.then(Instant::now),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn read_answer<R: std::io::Read>(out: &mut R, anim: Option<&mut dyn crate::anim::Sink>, clock: &dyn Fn(bool)) -> Result<Helped> {
    use std::io::Read;
    let crashed = || anyhow!("couldn't read the file (it is damaged, too large, or not supported)");
    let message = |out: &mut R| {
        let mut msg = Vec::new();
        let _ = out.by_ref().take(4096).read_to_end(&mut msg);
        match String::from_utf8_lossy(&msg).trim() {
            "" => crashed(),
            msg => anyhow!("{msg}"),
        }
    };
    let u32_at = |out: &mut R| -> Result<u32> {
        let mut b = [0u8; 4];
        out.read_exact(&mut b).map_err(|_| crashed())?;
        Ok(u32::from_le_bytes(b))
    };
    let read_icc = |out: &mut R| -> Result<Option<Vec<u8>>> {
        let n = u32_at(out)? as usize;
        if n == 0 {
            return Ok(None);
        }
        if n > (8 << 20) {
            bail!("the colour profile is too large");
        }
        let mut b = vec![0u8; n];
        out.read_exact(&mut b).map_err(|_| crashed())?;
        Ok(Some(b))
    };
    let mut tag = [0u8; 2];
    out.read_exact(&mut tag).map_err(|_| crashed())?;
    match &tag {
        b"OK" => {
            let (w, h) = (u32_at(out)?, u32_at(out)?);
            check_size(w, h)?;
            let mut px = vec![0u8; w as usize * h as usize * 4];
            out.read_exact(&mut px).map_err(|_| crashed())?;
            let icc = read_icc(out)?;
            Ok(Helped::Still(Picture { px: Decoded::Eight(px, w, h), icc }))
        }
        b"D6" => {
            let (w, h) = (u32_at(out)?, u32_at(out)?);
            check_size(w, h)?;
            if w as u64 * h as u64 * 8 > MAX_DECODE_BYTES {
                bail!("the image is too large to open at 16 bits ({w} x {h})");
            }
            let mut meta = [0u8; 2];
            out.read_exact(&mut meta).map_err(|_| crashed())?;
            let mut px = vec![0u16; w as usize * h as usize * 4];
            out.read_exact(bytemuck::cast_slice_mut(&mut px)).map_err(|_| crashed())?;
            if cfg!(target_endian = "big") {
                px.iter_mut().for_each(|v| *v = u16::from_le(*v));
            }
            let icc = read_icc(out)?;
            Ok(Helped::Still(Picture { px: Decoded::Deep(crate::deep::from_rgba16(px, w, h, meta[0], meta[1])), icc }))
        }
        b"NA" => Ok(Helped::Anim(false)),
        b"A3" => {
            let Some(sink) = anim else { bail!("unexpected decoder answer") };
            let (w, h, plays) = (u32_at(out)?, u32_at(out)?, u32_at(out)?);
            crate::anim::check(w, h, 0)?;
            clock(false);
            let started = sink.start(w, h, plays);
            clock(true);
            started?;
            let (mut frames, mut next) = (0, [0u8; 2]);
            loop {
                out.read_exact(&mut next).map_err(|_| crashed())?;
                match &next {
                    b"FR" => {
                        frames += 1;
                        crate::anim::check(w, h, frames)?;
                        let ms = u32_at(out)?;
                        let mut px = vec![0u8; w as usize * h as usize * 4];
                        out.read_exact(&mut px).map_err(|_| crashed())?;
                        clock(false);
                        sink.frame(px, ms)?;
                        clock(true);
                    }
                    b"EN" if frames >= 2 => return Ok(Helped::Anim(true)),
                    b"EN" => return Err(crashed()),
                    b"ER" => return Err(message(out)),
                    _ => bail!("unexpected decoder answer"),
                }
            }
        }
        b"ER" => Err(message(out)),
        _ => bail!("unexpected decoder answer"),
    }
}

fn decode_in_helper(path: &Path) -> Result<Picture> {
    match run_helper(path, None, &|| false)? {
        Helped::Still(x) => Ok(x),
        Helped::Anim(_) => bail!("unexpected decoder answer"),
    }
}

pub(crate) fn decode_animation(path: &Path, sink: &mut dyn crate::anim::Sink, stop: &Stop) -> Result<bool> {
    if IN_HELPER.load(Ordering::Relaxed) {
        let len = std::fs::metadata(path)?.len();
        if len > MAX_FILE_BYTES {
            bail!("the file is too large to open ({} MB)", len >> 20);
        }
        return crate::anim::decode_here(path, sink);
    }
    match run_helper(path, Some(sink), stop)? {
        Helped::Anim(a) => Ok(a),
        Helped::Still(_) => bail!("unexpected decoder answer"),
    }
}

struct FrameSender<'a, W: std::io::Write> {
    out: &'a mut W,
    size: usize,
}

impl<W: std::io::Write> crate::anim::Sink for FrameSender<'_, W> {
    fn start(&mut self, w: u32, h: u32, plays: u32) -> Result<()> {
        self.size = w as usize * h as usize * 4;
        self.out.write_all(b"A3")?;
        self.out.write_all(&w.to_le_bytes())?;
        self.out.write_all(&h.to_le_bytes())?;
        self.out.write_all(&plays.to_le_bytes())?;
        Ok(())
    }

    fn frame(&mut self, rgba: Vec<u8>, ms: u32) -> Result<()> {
        if rgba.len() != self.size {
            bail!("the animation's frames don't match its size");
        }
        self.out.write_all(b"FR")?;
        self.out.write_all(&ms.to_le_bytes())?;
        self.out.write_all(&rgba)?;
        self.out.flush()?;
        Ok(())
    }
}

pub fn run_decode_helper(args: &[std::ffi::OsString]) -> Option<i32> {
    use std::io::Write;
    if args.get(1).and_then(|a| a.to_str()) != Some(HELPER_ARG) {
        return None;
    }
    let Some(path) = args.get(2) else { return Some(2) };
    let animation = args.get(3).and_then(|a| a.to_str()) == Some("--anim");
    IN_HELPER.store(true, Ordering::Relaxed);
    crate::par::lower_priority();
    let main = std::thread::current().id();
    let logged = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        logged(info);
        if std::thread::current().id() != main {
            unsafe {
                use windows::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
                let _ = TerminateProcess(GetCurrentProcess(), 3);
            }
        }
    }));
    {
        use std::io::Read;
        let mut go = [0u8; 1];
        if !matches!(std::io::stdin().lock().read(&mut go), Ok(1)) {
            return Some(3);
        }
    }
    let mut out = std::io::BufWriter::with_capacity(1 << 20, std::io::stdout().lock());
    let written = if animation {
        match decode_animation(Path::new(path), &mut FrameSender { out: &mut out, size: 0 }, &|| false) {
            Ok(false) => out.write_all(b"NA"),
            Ok(true) => out.write_all(b"EN"),
            Err(e) => out.write_all(b"ER").and_then(|()| out.write_all(format!("{e:#}").as_bytes())),
        }
    } else {
        let decoded = decode_picture(Path::new(path)).map(|pic| match pic.px {
            Decoded::Deep(deep) if deep.w as u64 * deep.h as u64 * 8 > MAX_DECODE_BYTES => eight_for_app(deep, pic.icc),
            _ => pic,
        });
        let write_icc = |out: &mut std::io::BufWriter<std::io::StdoutLock>, icc: &Option<Vec<u8>>| -> std::io::Result<()> {
            match icc {
                Some(b) => out.write_all(&(b.len() as u32).to_le_bytes()).and_then(|()| out.write_all(b)),
                None => out.write_all(&0u32.to_le_bytes()),
            }
        };
        match decoded {
            Ok(Picture { px: Decoded::Eight(px, w, h), icc }) => out
                .write_all(b"OK")
                .and_then(|()| out.write_all(&w.to_le_bytes()))
                .and_then(|()| out.write_all(&h.to_le_bytes()))
                .and_then(|()| out.write_all(&px))
                .and_then(|()| write_icc(&mut out, &icc)),
            Ok(Picture { px: Decoded::Deep(d), icc }) => (|| {
                out.write_all(b"D6")?;
                out.write_all(&d.w.to_le_bytes())?;
                out.write_all(&d.h.to_le_bytes())?;
                out.write_all(&[d.cp, d.tc])?;
                if cfg!(target_endian = "little") {
                    out.write_all(bytemuck::cast_slice(&d.px))?;
                } else {
                    for v in &d.px {
                        out.write_all(&v.to_le_bytes())?;
                    }
                }
                write_icc(&mut out, &icc)
            })(),
            Err(e) => out.write_all(b"ER").and_then(|()| out.write_all(format!("{e:#}").as_bytes())),
        }
    };
    Some(if written.and_then(|()| out.flush()).is_ok() { 0 } else { 1 })
}

fn eight_for_app(mut deep: crate::deep::Deep, icc: Option<Vec<u8>>) -> Picture {
    let icc = icc.filter(|_| !deep.is_hdr());
    deep.icc = icc.clone();
    let (px, w, h) = Decoded::Deep(deep).into_8bit_keeping(icc.is_some());
    Picture { px: Decoded::Eight(px, w, h), icc }
}

pub fn can_read(ext: &str) -> bool {
    let e = ext.to_ascii_lowercase();
    Kind::from_extension(&e).is_some()
        || crate::readers::is_raw(&e)
        || matches!(
            e.as_str(),
            "avif" | "jxl" | "tif" | "tiff" | "bmp" | "heic" | "heif" | "hif" | "jp2" | "j2k" | "j2c" | "jpc" | "psd" | "svg" | "svgz"
        )
}

pub(crate) fn image_format_allowed(format: Option<image::ImageFormat>) -> bool {
    use image::ImageFormat::{Bmp, Gif, Ico, Jpeg, Png, Tiff, WebP};
    matches!(format, Some(Png | Jpeg | Gif | Bmp | Tiff | Ico | WebP))
}

pub(crate) fn ico_size<R: std::io::Read + std::io::Seek>(r: &mut R) -> Option<(u32, u32)> {
    use std::io::Read;
    let mut head = [0u8; 6];
    r.read_exact(&mut head).ok()?;
    if head[..2] != [0, 0] || u16::from_le_bytes([head[2], head[3]]) != 1 {
        return None;
    }
    let mut entries = Vec::new();
    let mut entry = [0u8; 16];
    for _ in 0..u16::from_le_bytes([head[4], head[5]]) {
        r.read_exact(&mut entry).ok()?;
        let w = if entry[0] == 0 { 256u32 } else { u32::from(entry[0]) };
        let h = if entry[1] == 0 { 256u32 } else { u32::from(entry[1]) };
        let bits = u16::from_le_bytes([entry[6], entry[7]]);
        let offset = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]);
        entries.push((w, h, bits, offset));
    }
    let mut best = entries.pop()?;
    for e in entries {
        if (e.2, e.0 * e.1) > (best.2, best.0 * best.1) {
            best = e;
        }
    }
    let (w, h, _, offset) = best;
    if w < 256 && h < 256 {
        return Some((w, h));
    }
    let mut own = Vec::new();
    r.seek(std::io::SeekFrom::Start(u64::from(offset))).ok()?;
    r.take(24).read_to_end(&mut own).ok()?;
    let be = |i: usize| own.get(i..i + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let le = |i: usize| own.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let le16 = |i: usize| own.get(i..i + 2).map(|b| u32::from(u16::from_le_bytes([b[0], b[1]])));
    if own.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        if own.get(12..16) != Some(&b"IHDR"[..]) {
            return None;
        }
        return Some((w.max(be(16)?), h.max(be(20)?)));
    }
    match le(0)? {
        12 => Some((w.max(le16(4)?), h.max(le16(6)? / 2))),
        n if n >= 40 => Some((w.max((le(4)? as i32).unsigned_abs()), h.max((le(8)? as i32).unsigned_abs() / 2))),
        _ => None,
    }
}

fn decode_with_image(path: &Path) -> Result<Picture> {
    use image::{ColorType, ImageDecoder, ImageReader, Limits};
    let mut reader = ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .with_context(|| format!("can't read {}", path.display()))?;
    if !image_format_allowed(reader.format()) {
        bail!("{} is not a picture format Snptu reads", path.display());
    }
    if reader.format() == Some(image::ImageFormat::Ico) && !IN_HELPER.load(Ordering::Relaxed) {
        bail!("can't read {}: the file changed while it was being read", path.display());
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DECODE_SIDE);
    limits.max_image_height = Some(MAX_DECODE_SIDE);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().with_context(|| format!("can't read {}", path.display()))?;
    let (w, h) = decoder.dimensions();
    check_size(w, h)?;
    if decoder.total_bytes() > MAX_DECODE_BYTES {
        bail!("the image is too large to open ({w} x {h}, {} MB decoded)", decoder.total_bytes() >> 20);
    }
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let sixteen = matches!(decoder.color_type(), ColorType::L16 | ColorType::La16 | ColorType::Rgb16 | ColorType::Rgba16);
    let icc = decoder.icc_profile().ok().flatten();
    let mut img = image::DynamicImage::from_decoder(decoder).with_context(|| format!("can't read {}", path.display()))?;
    img.apply_orientation(orientation);
    if sixteen && w as u64 * h as u64 * 8 <= MAX_DECODE_BYTES {
        let rgba = img.into_rgba16();
        let (w, h) = rgba.dimensions();
        return Ok(Picture { px: Decoded::Deep(crate::deep::from_rgba16(rgba.into_raw(), w, h, 0, 0)), icc });
    }
    let rgba = img.into_rgba8();
    let (w, h) = rgba.dimensions();
    Ok(Picture { px: Decoded::Eight(rgba.into_raw(), w, h), icc })
}

fn decode_webp(bytes: &[u8]) -> Result<Picture> {
    if let Some((w, h, _)) = crate::anim::webp_layout(bytes)? {
        check_size(w, h)?;
    }
    let mut decoder = zenwebp::WebPDecoder::build(bytes).map_err(|e| anyhow!("WebP: {e}"))?;
    let info = decoder.info();
    check_size(info.width, info.height)?;
    let icc = decoder.icc_profile().ok().flatten();
    let orientation = info.orientation.and_then(|o| image::metadata::Orientation::from_exif(o.to_exif()));
    let (px, w, h) = zenwebp::oneshot::decode_rgba(bytes).map_err(|e| anyhow!("WebP: {e}"))?;
    let px = match orientation {
        Some(o) if o != image::metadata::Orientation::NoTransforms => {
            let rgba = image::RgbaImage::from_raw(w, h, px).ok_or_else(|| anyhow!("WebP: unexpected pixel data"))?;
            let mut img = image::DynamicImage::ImageRgba8(rgba);
            img.apply_orientation(o);
            let rgba = img.into_rgba8();
            let (w, h) = rgba.dimensions();
            Decoded::Eight(rgba.into_raw(), w, h)
        }
        _ => Decoded::Eight(px, w, h),
    };
    Ok(Picture { px, icc })
}

fn decode_avif_any(bytes: &[u8]) -> Result<Picture> {
    let config = zenavif::DecoderConfig::new().threads(1).frame_size_limit(MAX_DECODE_PIXELS.min(u32::MAX as u64) as u32);
    let mut dec = zenavif::ManagedAvifDecoder::new(bytes, &config).map_err(|e| anyhow!("AVIF: {e}"))?;
    let (img, info) = dec.decode_full(&zenavif::Unstoppable).map_err(|e| anyhow!("AVIF: {e}"))?;
    let (cp, tc) = (info.color_primaries.0, info.transfer_characteristics.0);
    let icc = info.icc_profile.clone().or_else(|| crate::color::from_cicp(cp, tc));
    if info.bit_depth <= 8 && !matches!(tc, 16 | 18) {
        return Ok(Picture { px: eight(pixels_to_rgba8(&img).map(|(px, w, h)| avif_transformed(px, w, h, &info)))?, icc });
    }
    let (w, h) = (img.width(), img.height());
    let icc = info.icc_profile.clone();
    check_size(w, h)?;
    let mut px = Vec::with_capacity(w as usize * h as usize * 4);
    if let Some(b) = img.try_as_imgref::<rgb::Rgba<u16>>() {
        b.rows().for_each(|r| r.iter().for_each(|p| px.extend_from_slice(&[p.r, p.g, p.b, p.a])));
    } else if let Some(b) = img.try_as_imgref::<rgb::Rgb<u16>>() {
        b.rows().for_each(|r| r.iter().for_each(|p| px.extend_from_slice(&[p.r, p.g, p.b, 65535])));
    } else if let Some(b) = img.try_as_imgref::<rgb::Rgba<u8>>() {
        b.rows().for_each(|r| r.iter().for_each(|p| px.extend_from_slice(&[p.r as u16 * 257, p.g as u16 * 257, p.b as u16 * 257, p.a as u16 * 257])));
    } else if let Some(b) = img.try_as_imgref::<rgb::Rgb<u8>>() {
        b.rows().for_each(|r| r.iter().for_each(|p| px.extend_from_slice(&[p.r as u16 * 257, p.g as u16 * 257, p.b as u16 * 257, 65535])));
    } else {
        return Ok(Picture { px: eight(pixels_to_rgba8(&img).map(|(px, w, h)| avif_transformed(px, w, h, &info)))?, icc });
    }
    let (px, w, h) = avif_transformed(px, w, h, &info);
    Ok(Picture { px: Decoded::Deep(crate::deep::from_rgba16(px, w, h, cp, tc)), icc })
}

fn avif_aperture(c: &zenavif::CleanAperture, w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    let ratio = |n: f64, d: u32| (d != 0).then(|| n / d as f64);
    let (cw, ch) = (ratio(c.width_n as f64, c.width_d)?, ratio(c.height_n as f64, c.height_d)?);
    let x = ratio(c.horiz_off_n as f64, c.horiz_off_d)? + (w as f64 - cw) / 2.0;
    let y = ratio(c.vert_off_n as f64, c.vert_off_d)? + (h as f64 - ch) / 2.0;
    let whole = |v: f64| (v >= 0.0 && (v - v.round()).abs() < 1e-6).then_some(v.round() as usize);
    let (x, y, cw, ch) = (whole(x)?, whole(y)?, whole(cw)?, whole(ch)?);
    (cw >= 1 && ch >= 1 && x + cw <= w && y + ch <= h).then_some((x, y, cw, ch))
}

fn avif_transformed<T: Copy>(mut px: Vec<T>, w: u32, h: u32, info: &zenavif::ImageInfo) -> (Vec<T>, u32, u32) {
    let (mut w, mut h) = (w as usize, h as usize);
    let (angle, mirror) = (info.rotation.map_or(0, |r| r.angle), info.mirror.map(|m| m.axis));
    if let Some((x0, y0, cw, ch)) = info.clean_aperture.as_ref().and_then(|c| avif_aperture(c, w, h))
        && (cw, ch) != (w, h)
    {
        let mut cut = Vec::with_capacity(cw * ch * 4);
        for y in y0..y0 + ch {
            cut.extend_from_slice(&px[(y * w + x0) * 4..(y * w + x0 + cw) * 4]);
        }
        (px, w, h) = (cut, cw, ch);
    }
    if matches!(angle, 90 | 180 | 270) {
        let (nw, nh) = if angle == 180 { (w, h) } else { (h, w) };
        let mut turned = Vec::with_capacity(px.len());
        for y in 0..nh {
            for x in 0..nw {
                let (sx, sy) = match angle {
                    90 => (nh - 1 - y, x),
                    180 => (w - 1 - x, h - 1 - y),
                    _ => (y, nw - 1 - x),
                };
                turned.extend_from_slice(&px[(sy * w + sx) * 4..(sy * w + sx) * 4 + 4]);
            }
        }
        (px, w, h) = (turned, nw, nh);
    }
    match mirror {
        Some(0) => {
            for y in 0..h / 2 {
                let (top, bottom) = px.split_at_mut((h - 1 - y) * w * 4);
                top[y * w * 4..(y + 1) * w * 4].swap_with_slice(&mut bottom[..w * 4]);
            }
        }
        Some(1) => {
            for row in px.chunks_exact_mut(w * 4) {
                for x in 0..w / 2 {
                    for k in 0..4 {
                        row.swap(x * 4 + k, (w - 1 - x) * 4 + k);
                    }
                }
            }
        }
        _ => {}
    }
    (px, w as u32, h as u32)
}

pub(crate) fn pixels_to_rgba8(img: &zenavif::PixelBuffer) -> Result<(Vec<u8>, u32, u32)> {
    use zenpixels::PixelDescriptor;
    let desc = img.descriptor();
    let (w, h) = (img.width(), img.height());
    check_size(w, h)?;
    let mut out = Vec::with_capacity(w as usize * h as usize * 4);
    if desc.layout_compatible(PixelDescriptor::RGBA8) {
        let buf = img.try_as_imgref::<rgb::Rgba<u8>>().ok_or_else(|| anyhow!("AVIF: unsupported pixel layout"))?;
        for row in buf.rows() {
            for px in row {
                out.extend_from_slice(&[px.r, px.g, px.b, px.a]);
            }
        }
    } else if desc.layout_compatible(PixelDescriptor::RGB8) {
        let buf = img.try_as_imgref::<rgb::Rgb<u8>>().ok_or_else(|| anyhow!("AVIF: unsupported pixel layout"))?;
        for row in buf.rows() {
            for px in row {
                out.extend_from_slice(&[px.r, px.g, px.b, 255]);
            }
        }
    } else if desc.layout_compatible(PixelDescriptor::RGBA16) {
        let buf = img.try_as_imgref::<rgb::Rgba<u16>>().ok_or_else(|| anyhow!("AVIF: unsupported pixel layout"))?;
        for row in buf.rows() {
            for px in row {
                out.extend_from_slice(&[(px.r >> 8) as u8, (px.g >> 8) as u8, (px.b >> 8) as u8, (px.a >> 8) as u8]);
            }
        }
    } else if desc.layout_compatible(PixelDescriptor::RGB16) {
        let buf = img.try_as_imgref::<rgb::Rgb<u16>>().ok_or_else(|| anyhow!("AVIF: unsupported pixel layout"))?;
        for row in buf.rows() {
            for px in row {
                out.extend_from_slice(&[(px.r >> 8) as u8, (px.g >> 8) as u8, (px.b >> 8) as u8, 255]);
            }
        }
    } else {
        bail!("AVIF: unsupported pixel layout");
    }
    Ok((out, w, h))
}

fn decode_jxl(path: &Path) -> Result<Picture> {
    use jxl_oxide::{AllocTracker, JxlImage};
    let image = JxlImage::builder()
        .alloc_tracker(AllocTracker::with_limit(JXL_ALLOC_LIMIT))
        .open(path)
        .map_err(|e| anyhow!("JPEG XL: {e}"))?;
    jxl_picture(image)
}

fn jxl_picture(mut image: jxl_oxide::JxlImage) -> Result<Picture> {
    use jxl_oxide::{EnumColourEncoding, RenderingIntent};
    check_size(image.width(), image.height())?;
    let hdr = image.hdr_type().is_some();
    let wide = image.image_header().metadata.bit_depth.bits_per_sample() > 8;
    let deep = (hdr || wide) && image.width() as u64 * image.height() as u64 * 8 <= MAX_DECODE_BYTES;
    if hdr && !deep {
        image.request_color_encoding(EnumColourEncoding::srgb(RenderingIntent::Relative));
    }
    jxl_render(image, deep)
}

fn jxl_hdr_tags(hdr: jxl_oxide::HdrType, cicp: Option<[u8; 4]>) -> (u8, u8) {
    let cp = match cicp.map(|c| c[0]) {
        Some(11) => 12,
        Some(p) => p,
        None => 9,
    };
    (cp, if hdr == jxl_oxide::HdrType::Pq { 16 } else { 18 })
}

fn jxl_render(image: jxl_oxide::JxlImage, deep: bool) -> Result<Picture> {
    let format = image.pixel_format();
    let hdr = image.hdr_type().filter(|_| deep);
    let icc = if hdr.is_some() { None } else { Some(image.rendered_icc()) };
    let render = image.render_frame(0).map_err(|e| anyhow!("JPEG XL: {e}"))?;
    let mut stream = render.stream();
    check_size(stream.width(), stream.height())?;
    let (w, h, ch) = (stream.width() as usize, stream.height() as usize, stream.channels() as usize);
    if !(1..=4).contains(&ch) {
        bail!("this JPEG XL colour layout (CMYK or similar) isn't supported");
    }
    if deep {
        let mut buf = vec![0u16; w * h * ch];
        stream.write_to_buffer::<u16>(&mut buf);
        let rgba = jxl_rgba(buf, format, ch, u16::MAX)?;
        let (cp, tc) = hdr.map_or((0, 0), |t| jxl_hdr_tags(t, image.rendered_cicp()));
        return Ok(Picture { px: Decoded::Deep(crate::deep::from_rgba16(rgba, w as u32, h as u32, cp, tc)), icc });
    }
    let mut buf = vec![0u8; w * h * ch];
    stream.write_to_buffer::<u8>(&mut buf);
    Ok(Picture { px: Decoded::Eight(jxl_rgba(buf, format, ch, 255)?, w as u32, h as u32), icc })
}

fn jxl_rgba<T: Copy>(buf: Vec<T>, format: jxl_oxide::PixelFormat, ch: usize, opaque: T) -> Result<Vec<T>> {
    use jxl_oxide::PixelFormat;
    let mut rgba = Vec::with_capacity(buf.len() / ch * 4);
    match (format, ch) {
        (PixelFormat::Gray, 1) => buf.iter().for_each(|&g| rgba.extend_from_slice(&[g, g, g, opaque])),
        (PixelFormat::Graya, 2) => buf.as_chunks::<2>().0.iter().for_each(|p| rgba.extend_from_slice(&[p[0], p[0], p[0], p[1]])),
        (PixelFormat::Rgb, 3) => buf.as_chunks::<3>().0.iter().for_each(|p| rgba.extend_from_slice(&[p[0], p[1], p[2], opaque])),
        (PixelFormat::Rgba, 4) => rgba = buf,
        _ => bail!("this JPEG XL colour layout (CMYK or similar) isn't supported"),
    }
    Ok(rgba)
}


pub(crate) fn write_part(path: &Path, bytes: &[u8], made: Option<&Folders>) -> std::io::Result<PathBuf> {
    use std::io::Write;
    let (tmp, mut f, doomed) = create_part(path, made)?;
    let res = f.write_all(bytes).and_then(|()| finish_part(&f, doomed));
    drop(f);
    match res {
        Ok(()) => Ok(tmp),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn create_part(beside: &Path, made: Option<&Folders>) -> std::io::Result<(PathBuf, std::fs::File, bool)> {
    create_part_named(|| part_path(beside), made)
}

fn create_part_named(mut name: impl FnMut() -> PathBuf, made: Option<&Folders>) -> std::io::Result<(PathBuf, std::fs::File, bool)> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::{
        Foundation::GENERIC_WRITE,
        Storage::FileSystem::{DELETE, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_TEMPORARY, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES},
    };
    let mut tries = 0;
    loop {
        let path = name();
        tries += 1;
        let opened = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .access_mode(GENERIC_WRITE.0 | DELETE.0 | FILE_READ_ATTRIBUTES.0)
            .attributes(FILE_ATTRIBUTE_HIDDEN.0 | FILE_ATTRIBUTE_TEMPORARY.0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(&path);
        match opened {
            Ok(f) => {
                let doomed = delete_on_close(&f, true).is_ok();
                return Ok((path, f, doomed));
            }
            Err(e) if tries < 16 && e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) if tries < 16 && e.kind() == std::io::ErrorKind::NotFound => match (made, path.parent()) {
                (Some(made), Some(dir)) => make_dirs(dir, made)?,
                _ => return Err(e),
            },
            Err(e) => return Err(e),
        }
    }
}

fn finish_part(f: &std::fs::File, doomed: bool) -> std::io::Result<()> {
    f.sync_all()?;
    unhide(f)?;
    if doomed { delete_on_close(f, false) } else { Ok(()) }
}

fn part_path(path: &Path) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!(".{name}.{}-{}.part", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)))
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    replace_with(&write_part(path, bytes, None)?, path)
}

pub(crate) fn copy_atomic(mut src: impl std::io::Read, path: &Path) -> std::io::Result<()> {
    let (tmp, f, doomed) = create_part(path, None)?;
    let mut out = std::io::BufWriter::with_capacity(1 << 20, f);
    let res = match std::io::copy(&mut src, &mut out) {
        Ok(_) => out.into_inner().map_err(|e| e.into_error()).and_then(|f| finish_part(&f, doomed)),
        Err(e) => {
            drop(out.into_parts());
            Err(e)
        }
    };
    match res {
        Ok(()) => replace_with(&tmp, path),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn replace_with(tmp: &Path, path: &Path) -> std::io::Result<()> {
    let res = std::fs::rename(tmp, path);
    if res.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    res
}

pub(crate) struct PartFile<'a> {
    beside: PathBuf,
    made: &'a Folders,
    file: Option<(PathBuf, std::io::BufWriter<std::fs::File>, bool)>,
}

impl<'a> PartFile<'a> {
    pub(crate) fn new(beside: PathBuf, made: &'a Folders) -> Self {
        Self { beside, made, file: None }
    }

    fn open(&mut self) -> std::io::Result<&mut std::io::BufWriter<std::fs::File>> {
        if self.file.is_none() {
            let (path, f, doomed) = create_part(&self.beside, Some(self.made))?;
            self.file = Some((path, std::io::BufWriter::with_capacity(1 << 20, f), doomed));
        }
        match &mut self.file {
            Some((_, f, _)) => Ok(f),
            None => Err(std::io::Error::other("the file wasn't created")),
        }
    }

    pub(crate) fn keep(mut self) -> std::io::Result<PathBuf> {
        let Some((path, out, doomed)) = self.file.take() else { return Err(std::io::Error::other("nothing was written")) };
        let kept = out.into_inner().map_err(|e| e.into_error()).and_then(|f| finish_part(&f, doomed));
        match kept {
            Ok(()) => Ok(path),
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                Err(e)
            }
        }
    }
}

fn delete_on_close(f: &std::fs::File, delete: bool) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle},
    };
    let info = FILE_DISPOSITION_INFO { DeleteFile: delete };
    unsafe { SetFileInformationByHandle(HANDLE(f.as_raw_handle()), FileDispositionInfo, &info as *const _ as *const std::ffi::c_void, size_of::<FILE_DISPOSITION_INFO>() as u32) }?;
    Ok(())
}

fn unhide(f: &std::fs::File) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{
            FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_TEMPORARY, FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx, SetFileInformationByHandle,
        },
    };
    let (handle, size) = (HANDLE(f.as_raw_handle()), size_of::<FILE_BASIC_INFO>() as u32);
    let mut info = FILE_BASIC_INFO::default();
    unsafe { GetFileInformationByHandleEx(handle, FileBasicInfo, &mut info as *mut _ as *mut std::ffi::c_void, size) }?;
    let attrs = info.FileAttributes & !(FILE_ATTRIBUTE_HIDDEN.0 | FILE_ATTRIBUTE_TEMPORARY.0);
    if attrs == info.FileAttributes {
        return Ok(());
    }
    let set = FILE_BASIC_INFO { FileAttributes: if attrs == 0 { FILE_ATTRIBUTE_NORMAL.0 } else { attrs }, ..Default::default() };
    unsafe { SetFileInformationByHandle(handle, FileBasicInfo, &set as *const _ as *const std::ffi::c_void, size) }?;
    Ok(())
}

impl std::io::Write for PartFile<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        std::io::Write::write(self.open()?, buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match &mut self.file {
            Some((_, f, _)) => std::io::Write::flush(f),
            None => Ok(()),
        }
    }
}

impl std::io::Seek for PartFile<'_> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        std::io::Seek::seek(self.open()?, pos)
    }
}

impl Drop for PartFile<'_> {
    fn drop(&mut self) {
        if let Some((path, out, _)) = self.file.take() {
            drop(out.into_parts());
            let _ = std::fs::remove_file(path);
        }
    }
}

pub(crate) fn rename_new(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        Win32::Storage::FileSystem::{MOVE_FILE_FLAGS, MoveFileExW},
        core::PCWSTR,
    };
    let wide = |p: &Path| -> Vec<u16> {
        let long = match (p.parent().and_then(|d| std::fs::canonicalize(d).ok()), p.file_name()) {
            (Some(d), Some(n)) => d.join(n),
            _ => p.to_path_buf(),
        };
        long.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
    };
    let (from, to) = (wide(from), wide(to));
    unsafe { MoveFileExW(PCWSTR(from.as_ptr()), PCWSTR(to.as_ptr()), MOVE_FILE_FLAGS(0)) }.map_err(|e| std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF))
}

pub fn with_added_extension(path: &Path, ext: &str) -> PathBuf {
    let make = |n: u32| {
        let mut s = path.as_os_str().to_owned();
        if n > 0 {
            s.push(format!("_{n}"));
        }
        s.push(format!(".{ext}"));
        PathBuf::from(s)
    };
    (0..10_000).map(make).find(|p| !p.exists()).unwrap_or_else(|| make(0))
}


#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IfExists {
    Rename,
    Overwrite,
    Skip,
}

#[derive(Clone, Debug)]
pub struct Output {
    pub dir: Option<PathBuf>,
    pub template: String,
    pub if_exists: IfExists,
    pub delete_original: bool,
}

impl Output {
    pub fn new() -> Self {
        Self { dir: None, template: "{name}".to_string(), if_exists: IfExists::Rename, delete_original: false }
    }
}

#[derive(Clone)]
pub enum Source {
    File(PathBuf),
    Pixels { rgba: Arc<Vec<u8>>, w: u32, h: u32, name: String, dir: Option<PathBuf> },
}

impl Source {
    fn stem(&self) -> String {
        match self {
            Source::File(p) => p.file_stem().map_or_else(|| "image".into(), |n| n.to_string_lossy().into_owned()),
            Source::Pixels { name, .. } => name.clone(),
        }
    }

    fn folder(&self) -> Option<PathBuf> {
        match self {
            Source::File(p) => p.parent().map(Path::to_path_buf),
            Source::Pixels { dir, .. } => dir.clone(),
        }
    }
}

fn safe_name(s: &str) -> String {
    let t: String = s.chars().map(|c| if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control() { '_' } else { c }).collect();
    let t = t.trim().trim_end_matches('.').to_string();
    let mut units = 0;
    let t: String = if t.is_empty() {
        "image".to_string()
    } else {
        t.chars()
            .take_while(|c| {
                units += c.len_utf16();
                units <= 150
            })
            .collect()
    };
    let base = t.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    let device = matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (base.len() == 4 && (base.starts_with("COM") || base.starts_with("LPT")) && base.as_bytes()[3].is_ascii_digit());
    if device { format!("_{t}") } else { t }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

fn has_recycle_bin(path: &Path) -> bool {
    use windows::{
        Win32::Storage::FileSystem::GetDriveTypeW,
        core::PCWSTR,
    };
    const DRIVE_FIXED: u32 = 3;
    let Ok(full) = std::fs::canonicalize(path) else { return false };
    let s = full.to_string_lossy();
    let Some(rest) = s.strip_prefix(r"\\?\") else { return false };
    let b = rest.as_bytes();
    if b.len() < 3 || !b[0].is_ascii_alphabetic() || b[1] != b':' || b[2] != b'\\' {
        return false;
    }
    let root = crate::wide(&rest[..3]);
    unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) == DRIVE_FIXED }
}

pub fn output_name(out: &Output, src: &Source, n: usize, w: u32, h: u32, ext: &str) -> String {
    let stamp = crate::config::now_stamp();
    let name = out
        .template
        .replace("{name}", &src.stem())
        .replace("{n}", &n.to_string())
        .replace("{w}", &w.to_string())
        .replace("{h}", &h.to_string())
        .replace("{date}", &format!("{:04}-{:02}-{:02}", stamp.year, stamp.month, stamp.day))
        .replace("{time}", &format!("{:02}-{:02}-{:02}", stamp.hour, stamp.minute, stamp.second));
    format!("{}.{ext}", safe_name(&name))
}

fn path_key(path: &Path) -> PathBuf {
    let full = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let real = full.symlink_metadata().is_ok_and(|m| !m.file_type().is_symlink()).then(|| std::fs::canonicalize(&full).ok()).flatten();
    let full = real.unwrap_or(full);
    let dir = full.parent().map(|d| std::fs::canonicalize(d).unwrap_or_else(|_| d.to_path_buf())).unwrap_or_default();
    dir.join(full.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default())
}

#[derive(Default)]
pub struct Claims {
    sources: std::collections::HashSet<PathBuf>,
    written: std::sync::Mutex<std::collections::HashSet<PathBuf>>,
    folders: Folders,
}

#[derive(Default)]
pub struct Folders(std::sync::Mutex<Vec<(PathBuf, std::time::SystemTime)>>);

impl Folders {
    fn note(&self, dir: &Path) {
        if let Ok(at) = std::fs::symlink_metadata(dir).and_then(|m| m.created()) {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).push((dir.to_path_buf(), at));
        }
    }

    fn take_back(&self) {
        let mut made = std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()));
        made.sort_by_key(|(dir, _)| std::cmp::Reverse(dir.components().count()));
        for (dir, at) in made {
            if std::fs::symlink_metadata(&dir).and_then(|m| m.created()).is_ok_and(|t| t == at) {
                let _ = std::fs::remove_dir(&dir);
            }
        }
    }
}

fn make_dirs(dir: &Path, made: &Folders) -> std::io::Result<()> {
    if dir.as_os_str().is_empty() {
        return Ok(());
    }
    match std::fs::create_dir(dir) {
        Ok(()) => {
            made.note(dir);
            return Ok(());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) if dir.is_dir() => return Ok(()),
        Err(e) => return Err(e),
    }
    match dir.parent() {
        Some(parent) => make_dirs(parent, made)?,
        None => return Err(std::io::Error::other("failed to create whole tree")),
    }
    match std::fs::create_dir(dir) {
        Ok(()) => {
            made.note(dir);
            Ok(())
        }
        Err(_) if dir.is_dir() => Ok(()),
        Err(e) => Err(e),
    }
}

impl Claims {
    fn new(items: &[Source], if_exists: IfExists) -> Self {
        let files = items.iter().filter_map(|s| if let Source::File(p) = s { Some(path_key(p)) } else { None });
        Self { sources: if if_exists == IfExists::Overwrite { files.collect() } else { Default::default() }, ..Default::default() }
    }

    fn may_replace(&self, src: &Source, target: &Path) -> bool {
        let key = path_key(target);
        let own = matches!(src, Source::File(p) if path_key(p) == key && match (std::fs::canonicalize(p), std::fs::canonicalize(target)) {
            (Ok(a), Ok(b)) => a == b,
            _ => p.as_os_str().eq_ignore_ascii_case(target.as_os_str()),
        });
        (own || !self.sources.contains(&key)) && self.written.lock().is_ok_and(|mut w| w.insert(key))
    }

    fn claim(&self, path: &Path) -> bool {
        let key = path_key(path);
        !self.sources.contains(&key) && self.written.lock().is_ok_and(|mut w| w.insert(key))
    }

    fn release(&self, path: &Path) {
        if let Ok(mut w) = self.written.lock() {
            w.remove(&path_key(path));
        }
    }
}

fn write_result(dir: &Path, name: &str, if_exists: IfExists, bytes: &[u8], src: &Source, claims: &Claims) -> Result<Option<PathBuf>> {
    make_dirs(dir, &claims.folders)?;
    place_result(dir, name, if_exists, src, claims, |target| write_part(target, bytes, Some(&claims.folders)))
}

fn place_result(dir: &Path, name: &str, if_exists: IfExists, src: &Source, claims: &Claims, part: impl FnOnce(&Path) -> std::io::Result<PathBuf>) -> Result<Option<PathBuf>> {
    let target = dir.join(name);
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.to_string(), String::new()),
    };
    let fallback = if_exists == IfExists::Overwrite && !claims.may_replace(src, &target);
    let if_exists = if fallback { IfExists::Rename } else { if_exists };
    match if_exists {
        IfExists::Overwrite => {
            replace_with(&part(&target)?, &target)?;
            Ok(Some(target))
        }
        IfExists::Skip | IfExists::Rename => {
            if if_exists == IfExists::Skip && target.symlink_metadata().is_ok() {
                return Ok(None);
            }
            let part = part(&target)?;
            let mut result: Result<Option<PathBuf>> = Err(anyhow!("no free file name for {name}"));
            for n in 1..10_000u32 {
                let path = if n == 1 { target.clone() } else { dir.join(format!("{stem} ({n}){ext}")) };
                let claimed = !fallback || claims.claim(&path);
                let moved = if claimed { rename_new(&part, &path) } else { Err(std::io::ErrorKind::AlreadyExists.into()) };
                if fallback && claimed && moved.is_err() {
                    claims.release(&path);
                }
                match moved {
                    Ok(()) => result = Ok(Some(path)),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        if if_exists == IfExists::Rename {
                            continue;
                        }
                        result = Ok(None);
                    }
                    Err(e) => result = Err(e.into()),
                }
                break;
            }
            if !matches!(result, Ok(Some(_))) {
                let _ = std::fs::remove_file(&part);
            }
            result
        }
    }
}

#[derive(Clone, Debug)]
pub enum Outcome {
    Done(PathBuf),
    Kept(PathBuf, &'static str),
    Skipped,
    Failed(String),
}

fn make_pixel_bytes(src: &Source, p: &Params) -> Result<(u32, u32, Vec<u8>)> {
    if let Source::File(path) = src
        && p.animation
        && matches!(p.kind, Kind::Gif | Kind::Webp)
    {
        let mut out = std::io::Cursor::new(Vec::new());
        if let Some((w, h)) = crate::anim::convert(path, p, &mut out, &|| false)? {
            return Ok((w, h, out.into_inner()));
        }
    }
    make_still_bytes(src, p)
}

fn write_animation<'a>(src: &Source, p: &Params, dir: &Path, made: &'a Folders, stop: &Stop) -> Result<Option<(u32, u32, PartFile<'a>)>> {
    let Source::File(path) = src else { return Ok(None) };
    if !p.animation || !matches!(p.kind, Kind::Gif | Kind::Webp) {
        return Ok(None);
    }
    let mut part = PartFile::new(dir.join(format!("{}.{}", safe_name(&src.stem()), p.kind.ext())), made);
    Ok(crate::anim::convert(path, p, &mut part, stop)?.map(|(w, h)| (w, h, part)))
}

fn make_still_bytes(src: &Source, p: &Params) -> Result<(u32, u32, Vec<u8>)> {
    let pic = match src {
        Source::File(path) => decode_picture(path)?,
        Source::Pixels { rgba, w, h, .. } => plain(Decoded::Eight((**rgba).clone(), *w, *h)),
    };
    let icc = pic.icc;
    let (rgba, w, h, icc) = match pic.px {
        Decoded::Deep(mut d) if p.deep && crate::deep::can_hold(p.kind) => {
            d.icc = icc;
            let d = match p.resize {
                Some((mode, filter)) => crate::deep::resize(d, mode, filter)?,
                None => d,
            };
            let bytes = crate::deep::encode(p, &d)?;
            return Ok((d.w, d.h, bytes));
        }
        Decoded::Deep(mut d) => {
            d.icc = icc;
            if p.keep_profile && crate::color::embeds(p.kind) {
                if let Some(prof) = d.native_profile() {
                    (crate::deep::reduce(&d.px), d.w, d.h, Some(prof))
                } else {
                    (crate::deep::to_8bit(&d), d.w, d.h, None)
                }
            } else {
                (crate::deep::to_8bit(&d), d.w, d.h, None)
            }
        }
        Decoded::Eight(px, w, h) => (px, w, h, icc),
    };
    let (rgba, w, h) = match p.resize {
        Some((mode, filter)) => resize(rgba, w, h, mode, filter)?,
        None => (rgba, w, h),
    };
    let (rgba, w, h) = if p.kind == Kind::Ico && (w > 256 || h > 256) {
        resize(rgba, w, h, ResizeMode::Fit(256, 256), ResizeFilter::Lanczos)?
    } else {
        (rgba, w, h)
    };
    let mut no_resize = p.clone();
    no_resize.resize = None;
    let bytes = encode_in_profile(&no_resize, w, h, rgba, icc)?;
    Ok((w, h, bytes))
}

#[derive(Clone, Debug)]
pub struct Estimate {
    pub bytes: u64,
    pub original: Option<u64>,
    pub psnr: Option<f64>,
}

pub fn estimate(src: &Source, p: &Params) -> Result<Estimate> {
    guarded("estimating", || estimate_unguarded(src, p))
}

fn estimate_unguarded(src: &Source, p: &Params) -> Result<Estimate> {
    let (mut reference, mut w, mut h, icc) = match src {
        Source::File(path) => {
            let pic = decode_picture(path)?;
            let (px, w, h) = pic.px.into_8bit();
            (px, w, h, pic.icc)
        }
        Source::Pixels { rgba, w, h, .. } => ((**rgba).clone(), *w, *h, None),
    };
    if let Some((mode, filter)) = p.resize {
        (reference, w, h) = resize(reference, w, h, mode, filter)?;
    }
    if let Some(icc) = icc.filter(|_| !(p.keep_profile && crate::color::embeds(p.kind))) {
        reference = crate::color::to_srgb_8(&reference, w, &icc)?;
    }
    if p.kind == Kind::Jpeg {
        reference = rgb_of(&reference, p.background).as_chunks::<3>().0.iter().flat_map(|c| [c[0], c[1], c[2], 255]).collect();
    }
    let (ow, oh, bytes) = make_pixel_bytes(src, p)?;
    let original = match src {
        Source::File(path) => std::fs::metadata(path).ok().map(|m| m.len()),
        Source::Pixels { .. } => None,
    };
    let mut est = Estimate { bytes: bytes.len() as u64, original, psnr: None };
    if (ow, oh) != (w, h) {
        return Ok(est);
    }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let tmp = std::env::temp_dir().join(format!("snptu-estimate-{}-{nanos}.{}", std::process::id(), p.kind.ext()));
    std::fs::write(&tmp, &bytes)?;
    let back = decode(&tmp);
    let _ = std::fs::remove_file(&tmp);
    let (back, bw, bh) = back?;
    if (bw, bh) != (w, h) {
        return Ok(est);
    }
    let (mut sq, mut n) = (0f64, 0u64);
    for (a, b) in reference.as_chunks::<4>().0.iter().zip(back.as_chunks::<4>().0.iter()) {
        if a[3] == 0 && b[3] == 0 {
            continue;
        }
        for k in 0..3 {
            let d = a[k] as f64 - b[k] as f64;
            sq += d * d;
        }
        n += 3;
    }
    if sq > 0.0 && n > 0 {
        est.psnr = Some(10.0 * (255.0 * 255.0 / (sq / n as f64)).log10());
    }
    Ok(est)
}

pub fn run_one(src: &Source, n: usize, p: &Params, out: &Output, claims: &Claims, stop: &Stop) -> Option<Outcome> {
    let result = guarded("conversion", || {
        let dir = out.dir.clone().or_else(|| src.folder()).unwrap_or_else(|| PathBuf::from("."));
        let written = match write_animation(src, p, &dir, &claims.folders, stop)? {
            Some((w, h, part)) => place_result(&dir, &output_name(out, src, n, w, h, p.kind.ext()), out.if_exists, src, claims, move |_| part.keep())?,
            None => {
                let (w, h, bytes) = make_still_bytes(src, p)?;
                write_result(&dir, &output_name(out, src, n, w, h, p.kind.ext()), out.if_exists, &bytes, src, claims)?
            }
        };
        Ok(match written {
            Some(path) => match src {
                Source::File(orig) if out.delete_original => match recycle_original(orig, &path) {
                    Some(why) => Outcome::Kept(path, why),
                    None => Outcome::Done(path),
                },
                _ => Outcome::Done(path),
            },
            None => Outcome::Skipped,
        })
    });
    match result {
        Err(e) if e.is::<crate::anim::Stopped>() => None,
        result => Some(result.unwrap_or_else(|e| Outcome::Failed(format!("{e:#}")))),
    }
}

fn recycle_original(orig: &Path, result: &Path) -> Option<&'static str> {
    if same_file(orig, result) {
        return None;
    }
    if !has_recycle_bin(orig) {
        return Some("its drive has no Recycle Bin");
    }
    (!trash(orig)).then_some("it couldn't be moved to the Recycle Bin")
}

fn trash(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        Win32::UI::Shell::{FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING, SHFILEOPSTRUCTW, SHFileOperationW},
        core::PCWSTR,
    };
    let full = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut wide: Vec<u16> = full.as_os_str().encode_wide().collect();
    wide.extend([0, 0]);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(wide.as_ptr()),
        fFlags: (FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_NOERRORUI.0 | FOF_SILENT.0 | FOF_WANTNUKEWARNING.0) as u16,
        ..Default::default()
    };
    unsafe { SHFileOperationW(&mut op) == 0 && !op.fAnyOperationsAborted.as_bool() }
}

pub struct Progress {
    pub total: usize,
    pub finished: AtomicUsize,
    pub cancel: AtomicBool,
    pub ended: AtomicBool,
    pub outcomes: std::sync::Mutex<Vec<Option<Outcome>>>,
}

impl Progress {
    pub fn new(total: usize) -> Arc<Self> {
        Arc::new(Self { total, finished: AtomicUsize::new(0), cancel: AtomicBool::new(false), ended: AtomicBool::new(false), outcomes: std::sync::Mutex::new(vec![None; total]) })
    }
}

struct MemoryGate {
    budget: usize,
    used: std::sync::Mutex<usize>,
    freed: std::sync::Condvar,
}

struct Hold<'a>(&'a MemoryGate, usize);

impl MemoryGate {
    fn budget_for(free: u64) -> usize {
        usize::try_from(free / 5 * 2).unwrap_or(usize::MAX).max(2 << 30)
    }

    fn new() -> Self {
        use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut status = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        let free = if unsafe { GlobalMemoryStatusEx(&mut status) }.is_ok() { status.ullAvailPhys } else { 0 };
        Self { budget: Self::budget_for(free), used: std::sync::Mutex::new(0), freed: std::sync::Condvar::new() }
    }

    fn admit(&self, weight: usize, cancel: &AtomicBool) -> Option<Hold<'_>> {
        let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            if *used == 0 || used.saturating_add(weight) <= self.budget {
                break;
            }
            used = self.freed.wait(used).unwrap_or_else(|e| e.into_inner());
        }
        *used = used.saturating_add(weight);
        Some(Hold(self, weight))
    }
}

impl Drop for Hold<'_> {
    fn drop(&mut self) {
        *self.0.used.lock().unwrap_or_else(|e| e.into_inner()) -= self.1;
        self.0.freed.notify_all();
    }
}

const WEBP_ANIM_BYTES_PER_PIXEL: u64 = 96;
const GIF_ANIM_BYTES_PER_PIXEL: u64 = 64;
const ANIM_FIXED_BYTES: u64 = 64 << 20;

fn anim_weight(pixels: u64, p: &Params, file: u64) -> usize {
    let per_pixel = if p.kind == Kind::Webp { WEBP_ANIM_BYTES_PER_PIXEL } else { GIF_ANIM_BYTES_PER_PIXEL };
    usize::try_from(pixels.min(MAX_DECODE_PIXELS) * per_pixel + ANIM_FIXED_BYTES + file).unwrap_or(usize::MAX)
}

fn memory_weight(src: &Source, p: &Params) -> usize {
    if p.animation
        && let Source::File(path) = src
        && let Some((w, h)) = crate::anim::canvas_size(path)
    {
        let file = std::fs::metadata(path).map_or(0, |m| m.len());
        let (nw, nh) = p.resize.map_or((w, h), |(mode, _)| resized_size(w, h, mode));
        return anim_weight((w as u64 * h as u64).max(nw as u64 * nh as u64), p, file);
    }
    let (w, h, known, reader) = match src {
        Source::Pixels { w, h, .. } => (*w, *h, true, None),
        Source::File(path) => match pick_reader(path) {
            Ok(reader) => match still_size(reader, path) {
                Some((w, h)) => (w, h, true, Some(reader)),
                None => (0, 0, false, Some(reader)),
            },
            Err(_) => return 0,
        },
    };
    let mut pixels = if known { w as u64 * h as u64 } else { 0 };
    if let Some((mode, _)) = p.resize {
        let (nw, nh) = if known { resized_size(w, h, mode) } else { match mode { ResizeMode::Exact(nw, nh) => (nw, nh), _ => (0, 0) } };
        pixels = pixels.max(nw as u64 * nh as u64);
    }
    let weight = if known { pixels.min(MAX_DECODE_PIXELS) as usize * STILL_BYTES_PER_PIXEL } else { 0 };
    if !p.animation {
        return if known { weight } else { unknown_still_weight() };
    }
    let scaled = match p.resize {
        Some((ResizeMode::Exact(w, h) | ResizeMode::Fit(w, h), _)) => anim_weight(w as u64 * h as u64, p, 0),
        _ => 0,
    };
    if !known && reader.is_some_and(|r| !matches!(r, Reader::Webp | Reader::Avif | Reader::Image)) {
        return unknown_still_weight().max(scaled);
    }
    weight.max(256 << 20).max(scaled)
}

const STILL_BYTES_PER_PIXEL: usize = 16;

fn still_size(reader: Reader, path: &Path) -> Option<(u32, u32)> {
    match reader {
        Reader::Webp => crate::anim::webp_size(path),
        Reader::Ico => {
            let mut r = std::io::BufReader::new(std::fs::File::open(path).ok()?);
            ico_size(&mut r)
        }
        Reader::Avif => crate::anim::canvas_size(path),
        Reader::Image => {
            use image::ImageReader;
            let reader = ImageReader::open(path).ok()?.with_guessed_format().ok()?;
            if !image_format_allowed(reader.format()) {
                return None;
            }
            reader.into_dimensions().ok()
        }
        _ => None,
    }
}

fn unknown_still_weight() -> usize {
    usize::try_from(MAX_DECODE_PIXELS).unwrap_or(usize::MAX).saturating_mul(STILL_BYTES_PER_PIXEL)
}

pub fn run_batch(items: Vec<Source>, p: Params, out: Output, threads: usize, progress: Arc<Progress>, notify: impl Fn() + Sync) {
    let next = AtomicUsize::new(0);
    let threads = threads.clamp(1, items.len().max(1));
    let (gate, claims) = (MemoryGate::new(), Claims::new(&items, out.if_exists));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                crate::par::lower_priority();
                let stop = || progress.cancel.load(Ordering::Relaxed);
                loop {
                    if progress.cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(src) = items.get(i) else { break };
                    let weight = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| memory_weight(src, &p))).unwrap_or_else(|_| unknown_still_weight());
                    let Some(hold) = gate.admit(weight, &progress.cancel) else { break };
                    let outcome = run_one(src, i + 1, &p, &out, &claims, &stop);
                    drop(hold);
                    let Some(outcome) = outcome else { break };
                    if let Ok(mut o) = progress.outcomes.lock() {
                        o[i] = Some(outcome);
                    }
                    progress.finished.fetch_add(1, Ordering::Relaxed);
                    notify();
                }
            });
        }
    });
    claims.folders.take_back();
}


#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Png,
    Jpeg(u8),
    WebpLossless,
    Gif,
    Ico,
}

impl Format {
    pub fn ext(self) -> &'static str {
        self.params().kind.ext()
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Png => "PNG (lossless)",
            Format::Jpeg(q) if q >= 97 => "JPEG (maximum quality)",
            Format::Jpeg(q) if q >= 88 => "JPEG (high quality)",
            Format::Jpeg(_) => "JPEG (small file)",
            Format::WebpLossless => "WebP (lossless)",
            Format::Gif => "GIF (256 colours)",
            Format::Ico => "Icon (up to 256 px)",
        }
    }

    pub fn from_extension(ext: &str) -> Option<Format> {
        Some(match Kind::from_extension(ext)? {
            Kind::Png => Format::Png,
            Kind::Jpeg => Format::Jpeg(92),
            Kind::Webp => Format::WebpLossless,
            Kind::Gif => Format::Gif,
            Kind::Ico => Format::Ico,
        })
    }

    pub fn params(self) -> Params {
        match self {
            Format::Png => Params::new(Kind::Png),
            Format::Jpeg(q) => {
                let mut p = Params::new(Kind::Jpeg);
                p.quality = q;
                p
            }
            Format::WebpLossless => {
                let mut p = Params::new(Kind::Webp);
                p.lossless = true;
                p
            }
            Format::Gif => Params { animation: true, ..Params::new(Kind::Gif) },
            Format::Ico => Params::new(Kind::Ico),
        }
    }
}

pub fn convert_in_background(path: PathBuf, format: Format, w: u32, h: u32, rgba: Vec<u8>) {
    std::thread::spawn(move || {
        crate::par::lower_priority();
        let result = encode(&format.params(), w, h, &rgba).and_then(|bytes| Ok(write_atomic(&path, &bytes)?));
        if let Err(e) = result {
            crate::log(&format!("convert to {} ({}): {e:#}", format.ext(), path.display()));
            crate::beep();
        }
    });
}

pub fn run_cli(args: &[String]) -> Option<i32> {
    if args.get(1).map(String::as_str) != Some("--convert") {
        return None;
    }
    let (Some(fmt), files) = (args.get(2), args.get(3..).unwrap_or_default()) else { return Some(2) };
    let Some(kind) = Kind::from_extension(fmt) else { return Some(2) };
    let out = Output::new();
    let claims = Claims::default();
    let slot = cli_slot();
    let mut failures = Vec::new();
    for (i, file) in files.iter().enumerate() {
        let params = Params { animation: matches!(kind, Kind::Gif | Kind::Webp) && crate::anim::is_animated(Path::new(file)), ..Params::new(kind) };
        if let Some(Outcome::Failed(e)) = run_one(&Source::File(PathBuf::from(file)), i + 1, &params, &out, &claims, &|| false) {
            failures.push(format!("{}: {e}", Path::new(file).file_name().map_or_else(|| file.clone(), |n| n.to_string_lossy().into_owned())));
        }
    }
    drop(slot);
    if !failures.is_empty() {
        message_box(&format!("Snptu couldn't convert:\n\n{}", failures.join("\n")));
        return Some(1);
    }
    Some(0)
}

struct CliSlot(windows::Win32::Foundation::HANDLE);

impl Drop for CliSlot {
    fn drop(&mut self) {
        use windows::Win32::{Foundation::CloseHandle, System::Threading::ReleaseMutex};
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

fn cli_slot() -> Option<CliSlot> {
    use windows::{
        Win32::{
            Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED_0, WAIT_OBJECT_0},
            System::Threading::{CreateMutexW, INFINITE, WaitForMultipleObjects},
        },
        core::PCWSTR,
    };
    const SLOTS: usize = 2;
    let handles: Vec<HANDLE> = (0..SLOTS)
        .filter_map(|i| {
            let name = crate::wide(&format!("Local\\snptu-convert-slot-{i}"));
            unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }.ok()
        })
        .collect();
    if handles.is_empty() {
        return None;
    }
    let r = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) }.0;
    let won = if (WAIT_OBJECT_0.0..WAIT_OBJECT_0.0 + handles.len() as u32).contains(&r) {
        Some((r - WAIT_OBJECT_0.0) as usize)
    } else if (WAIT_ABANDONED_0.0..WAIT_ABANDONED_0.0 + handles.len() as u32).contains(&r) {
        Some((r - WAIT_ABANDONED_0.0) as usize)
    } else {
        None
    };
    let mut slot = None;
    for (i, h) in handles.into_iter().enumerate() {
        if Some(i) == won {
            slot = Some(CliSlot(h));
        } else {
            unsafe {
                let _ = CloseHandle(h);
            }
        }
    }
    slot
}

pub fn message_box(text: &str) {
    crate::msgbox::show(text);
}
