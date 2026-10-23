
use anyhow::{Result, anyhow, bail};
use std::ffi::c_void;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub struct Frame {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    bitmap: HBITMAP,
    bits: *const u8,
}

impl Frame {
    pub fn bgra(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.bits, self.w as usize * self.h as usize * 4) }
    }

    pub fn crop_rgba(&self, x0: u32, y0: u32, x1: u32, y1: u32) -> (u32, u32, Vec<u8>) {
        let (x1, y1) = (x1.min(self.w), y1.min(self.h));
        let (cw, ch) = (x1.saturating_sub(x0) as usize, y1.saturating_sub(y0) as usize);
        let src = self.bgra();
        let mut out = vec![255u8; cw * ch * 4];
        for row in 0..ch {
            let s = ((y0 as usize + row) * self.w as usize + x0 as usize) * 4;
            let line = &src[s..s + cw * 4];
            let dst = &mut out[row * cw * 4..(row + 1) * cw * 4];
            for (d, p) in dst.as_chunks_mut::<4>().0.iter_mut().zip(line.as_chunks::<4>().0) {
                d[0] = p[2];
                d[1] = p[1];
                d[2] = p[0];
            }
        }
        (cw as u32, ch as u32, out)
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
        }
    }
}

pub fn capture_virtual_screen() -> Result<Frame> {
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if w <= 0 || h <= 0 {
            bail!("virtual screen has no area");
        }

        let screen = GetDC(None);
        if screen.is_invalid() {
            bail!("GetDC failed");
        }
        let mem = CreateCompatibleDC(Some(screen));

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bitmap = CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0);

        let result = (|| -> Result<Frame> {
            let bitmap = bitmap.map_err(|e| anyhow!("CreateDIBSection: {e}"))?;
            if bits.is_null() {
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
                bail!("DIB section has no pixels");
            }
            let old = SelectObject(mem, HGDIOBJ(bitmap.0));
            let blt = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY | CAPTUREBLT);
            SelectObject(mem, old);
            if let Err(e) = blt {
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
                bail!("BitBlt: {e}");
            }
            Ok(Frame { x, y, w: w as u32, h: h as u32, bitmap, bits: bits as *const u8 })
        })();

        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        result
    }
}
