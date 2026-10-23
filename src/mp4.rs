
use anyhow::{Result, anyhow, bail};
use std::{
    path::PathBuf,
    sync::{Mutex, PoisonError},
    time::Duration,
};
use windows::{
    Win32::{
        Media::MediaFoundation::{
            IMFMediaType, IMFSinkWriter, MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
            MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_MT_TRANSFER_FUNCTION, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_VIDEO_PRIMARIES, MF_MT_YUV_MATRIX,
            MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MF_TRANSCODE_CONTAINERTYPE, MF_VERSION, MFCreateAttributes, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
            MFCreateSinkWriterFromURL, MFMediaType_Video, MFNominalRange_16_235, MFSTARTUP_FULL, MFStartup, MFTranscodeContainerType_MPEG4, MFVideoFormat_H264,
            MFVideoFormat_NV12, MFVideoInterlace_Progressive, MFVideoPrimaries_BT709, MFVideoTransFunc_709, MFVideoTransferMatrix_BT709,
        },
        System::{
            Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize},
            LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW},
        },
    },
    core::{HSTRING, w},
};

const BITS_PER_PIXEL: [f64; 3] = [0.05, 0.10, 0.20];

const TICKS_PER_SEC: u64 = 10_000_000;

const MIN_SIDE: u32 = 34;

const ENCODER_THREADS: usize = 4;

fn encoder_threads(cpus: usize) -> u32 {
    cpus.clamp(1, ENCODER_THREADS) as u32
}

fn pack(hi: u32, lo: u32) -> u64 {
    (hi as u64) << 32 | lo as u64
}

fn frame_of(prev: Option<u64>, at: Duration, fps: u32) -> u64 {
    let nearest = ((at.as_nanos() * fps as u128 + 500_000_000) / 1_000_000_000) as u64;
    prev.map_or(nearest, |p| nearest.max(p + 1))
}

fn start_of(n: u64, fps: u32) -> i64 {
    (n as u128 * TICKS_PER_SEC as u128 / fps as u128) as i64
}

pub fn too_small(size: (u32, u32)) -> Option<String> {
    let (w, h) = (size.0 & !1, size.1 & !1);
    (w < MIN_SIDE || h < MIN_SIDE).then(|| {
        format!("an MP4 video must be at least {MIN_SIDE} x {MIN_SIDE} pixels, and this one would be {} x {}. Choose a larger region or scale, or GIF or WebP.", size.0, size.1)
    })
}

pub struct Mp4Writer {
    path: PathBuf,
    in_size: (u32, u32),
    size: (u32, u32),
    fps: u32,
    bitrate: u32,
    inner: Option<Inner>,
    frame: Option<u64>,
}

struct Inner {
    writer: IMFSinkWriter,
    stream: u32,
    _mf: MfStarted,
}

const MAX_WAITING: u64 = 24;

fn waiting(inner: &Inner) -> u64 {
    use windows::Win32::Media::MediaFoundation::MF_SINK_WRITER_STATISTICS;
    let mut s = MF_SINK_WRITER_STATISTICS { cb: size_of::<MF_SINK_WRITER_STATISTICS>() as u32, ..Default::default() };
    match unsafe { inner.writer.GetStatistics(inner.stream, &mut s) } {
        Ok(()) => s.qwNumSamplesReceived.saturating_sub(s.qwNumSamplesEncoded),
        Err(_) => 0,
    }
}

struct MfStarted {
    com: bool,
}

impl MfStarted {
    fn new() -> Result<Self> {
        let started = Self { com: unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok() };
        start_media_foundation()?;
        Ok(started)
    }
}

impl Drop for MfStarted {
    fn drop(&mut self) {
        if self.com {
            unsafe { CoUninitialize() };
        }
    }
}

static MF_STARTED: Mutex<bool> = Mutex::new(false);

fn start_media_foundation() -> Result<()> {
    let mut started = MF_STARTED.lock().unwrap_or_else(PoisonError::into_inner);
    if !*started {
        for dll in [w!("mfplat.dll"), w!("mfreadwrite.dll")] {
            unsafe { LoadLibraryExW(dll, None, LOAD_LIBRARY_SEARCH_SYSTEM32) }.map_err(|e| anyhow!("MP4: Media Foundation is not available: {e}"))?;
        }
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(|e| anyhow!("MP4: Media Foundation is not available: {e}"))?;
        *started = true;
    }
    Ok(())
}

unsafe impl Send for Mp4Writer {}

impl Mp4Writer {
    pub fn new(path: PathBuf, size: (u32, u32), fps: u32, quality: u8) -> Result<Self> {
        if let Some(why) = too_small(size) {
            bail!(why);
        }
        let (w, h) = (size.0 & !1, size.1 & !1);
        let bpp = BITS_PER_PIXEL[quality.min(2) as usize];
        let bitrate = (w as f64 * h as f64 * fps as f64 * bpp).clamp(500_000.0, 100_000_000.0) as u32;
        Ok(Self { path, in_size: size, size: (w, h), fps: fps.max(1), bitrate, inner: None, frame: None })
    }

    fn open(&self) -> Result<Inner> {
        use windows::Win32::Media::MediaFoundation::CODECAPI_AVEncNumWorkerThreads;
        let (w, h) = self.size;
        let ctx = |what: &str| format!("MP4: {what}");
        let mf = MfStarted::new()?;
        unsafe {
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 2)?;
            let attrs = attrs.ok_or_else(|| anyhow!("MP4: no attributes"))?;
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(self.path.as_os_str()), None, &attrs).map_err(|e| anyhow!("{}: {e}", ctx("can't create the file")))?;

            let out = MFCreateMediaType()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, self.bitrate)?;
            out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            out.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h))?;
            out.SetUINT64(&MF_MT_FRAME_RATE, pack(self.fps, 1))?;
            out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            bt709(&out)?;
            let stream = writer.AddStream(&out)?;

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h))?;
            input.SetUINT64(&MF_MT_FRAME_RATE, pack(self.fps, 1))?;
            input.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            input.SetUINT32(&MF_MT_DEFAULT_STRIDE, w)?;
            bt709(&input)?;
            let mut params = None;
            MFCreateAttributes(&mut params, 1)?;
            let params = params.ok_or_else(|| anyhow!("MP4: no attributes"))?;
            params.SetUINT32(&CODECAPI_AVEncNumWorkerThreads, encoder_threads(std::thread::available_parallelism().map_or(ENCODER_THREADS, |n| n.get())))?;
            if writer.SetInputMediaType(stream, &input, &params).is_err() {
                writer.SetInputMediaType(stream, &input, None).map_err(|e| anyhow!("{}: {e}", ctx("the H.264 encoder can't take these pictures")))?;
            }
            writer.BeginWriting().map_err(|e| anyhow!("{}: {e}", ctx("the H.264 encoder did not start")))?;
            Ok(Inner { writer, stream, _mf: mf })
        }
    }

    pub fn write(&mut self, at: Duration, bgra: &[u8]) -> Result<()> {
        let (iw, ih) = self.in_size;
        if bgra.len() != iw as usize * ih as usize * 4 {
            bail!("a picture of the wrong size");
        }
        let inner = match self.inner {
            Some(ref inner) => inner,
            None => self.inner.insert(self.open()?),
        };
        if waiting(inner) >= MAX_WAITING {
            return Ok(());
        }
        let (w, h) = self.size;
        let bytes = w as usize * h as usize * 3 / 2;
        let frame = frame_of(self.frame, at, self.fps);
        let (start, end) = (start_of(frame, self.fps), start_of(frame + 1, self.fps));
        unsafe {
            let buf = MFCreateMemoryBuffer(bytes as u32)?;
            let mut dst = std::ptr::null_mut();
            buf.Lock(&mut dst, None, None)?;
            to_nv12(bgra, iw as usize, (w as usize, h as usize), std::slice::from_raw_parts_mut(dst, bytes));
            buf.Unlock()?;
            buf.SetCurrentLength(bytes as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buf)?;
            sample.SetSampleTime(start)?;
            sample.SetSampleDuration(end - start)?;
            inner.writer.WriteSample(inner.stream, &sample).map_err(|e| anyhow!("MP4: {e}"))?;
        }
        self.frame = Some(frame);
        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        let Some(inner) = self.inner.take() else { bail!("nothing was recorded") };
        unsafe { inner.writer.Finalize() }.map_err(|e| anyhow!("MP4: {e}"))
    }
}

fn bt709(t: &IMFMediaType) -> Result<()> {
    unsafe {
        t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
    }
    Ok(())
}

const TO_Y: [i32; 3] = [11966, 40254, 4064];
const TO_CB: [i32; 3] = [-6596, -22188, 28784];
const TO_CR: [i32; 3] = [28784, -26145, -2639];

fn to_nv12(bgra: &[u8], width: usize, (w, h): (usize, usize), out: &mut [u8]) {
    debug_assert!(w % 2 == 0 && h % 2 == 0 && w <= width && out.len() == w * h * 3 / 2);
    let y_of = |p: &[u8; 4]| ((TO_Y[0] * p[2] as i32 + TO_Y[1] * p[1] as i32 + TO_Y[2] * p[0] as i32 + (16 << 16) + (1 << 15)) >> 16) as u8;
    let row = |y: usize| &bgra[y * width * 4..][..w * 4];
    let (luma, chroma) = out.split_at_mut(w * h);
    for (r, (ys, cbcr)) in luma.chunks_exact_mut(2 * w).zip(chroma.chunks_exact_mut(w)).enumerate() {
        let (top, bottom) = (row(2 * r), row(2 * r + 1));
        let (y_top, y_bottom) = ys.split_at_mut(w);
        for (d, p) in y_top.iter_mut().zip(top.as_chunks::<4>().0) {
            *d = y_of(p);
        }
        for (d, p) in y_bottom.iter_mut().zip(bottom.as_chunks::<4>().0) {
            *d = y_of(p);
        }
        for ((d, t), b) in cbcr.as_chunks_mut::<2>().0.iter_mut().zip(top.as_chunks::<8>().0).zip(bottom.as_chunks::<8>().0) {
            let sum = |i: usize| t[i] as i32 + t[i + 4] as i32 + b[i] as i32 + b[i + 4] as i32;
            let (b4, g4, r4) = (sum(0), sum(1), sum(2));
            *d = [
                ((TO_CB[0] * r4 + TO_CB[1] * g4 + TO_CB[2] * b4 + (128 << 18) + (1 << 17)) >> 18) as u8,
                ((TO_CR[0] * r4 + TO_CR[1] * g4 + TO_CR[2] * b4 + (128 << 18) + (1 << 17)) >> 18) as u8,
            ];
        }
    }
}
