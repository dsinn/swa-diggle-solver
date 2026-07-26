use crate::win::window::GameWindow;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};

/// PW_RENDERFULLCONTENT. Required for hardware-composited windows; a plain BitBlt
/// against this game's OpenGL surface returns black.
const PW_RENDERFULLCONTENT: PRINT_WINDOW_FLAGS = PRINT_WINDOW_FLAGS(0x0000_0002);

/// A pixel is "non-black" if any channel exceeds this. Guards against a capture
/// that technically succeeds but returns an empty surface.
const BLACK_THRESHOLD: u8 = 8;

#[derive(Debug, Clone)]
pub struct Frame {
    pub width: i32,
    pub height: i32,
    /// Top-down BGRA, 4 bytes per pixel.
    pub bgra: Vec<u8>,
}

/// A rectangle in normalized (0..1) coordinates.
#[derive(Debug, Clone, Copy)]
pub struct Region {
    pub nx: f64,
    pub ny: f64,
    pub nw: f64,
    pub nh: f64,
}

/// The region hashed for screen identity. Deliberately excludes the bottom strip,
/// where animated elements and notifications live.
pub const FINGERPRINT_REGION: Region = Region { nx: 0.25, ny: 0.05, nw: 0.5, nh: 0.5 };

impl Frame {
    fn region_px(&self, r: Region) -> (i32, i32, i32, i32) {
        let x0 = (r.nx * self.width as f64).max(0.0) as i32;
        let y0 = (r.ny * self.height as f64).max(0.0) as i32;
        let x1 = ((r.nx + r.nw) * self.width as f64).min(self.width as f64) as i32;
        let y1 = ((r.ny + r.nh) * self.height as f64).min(self.height as f64) as i32;
        (x0, y0, x1.max(x0), y1.max(y0))
    }

    fn pixel(&self, x: i32, y: i32) -> &[u8] {
        let i = ((y * self.width + x) * 4) as usize;
        &self.bgra[i..i + 4]
    }

    /// FNV-1a over the pixels inside `r`.
    pub fn region_hash(&self, r: Region) -> u64 {
        let (x0, y0, x1, y1) = self.region_px(r);
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for y in y0..y1 {
            let row = (y * self.width * 4) as usize;
            let from = row + (x0 * 4) as usize;
            let to = row + (x1 * 4) as usize;
            for b in &self.bgra[from..to] {
                h ^= *b as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        h
    }

    pub fn identical_region(&self, other: &Frame, r: Region) -> bool {
        self.width == other.width
            && self.height == other.height
            && self.region_hash(r) == other.region_hash(r)
    }

    /// Fraction of PIXELS inside `r` that differ from `other`. Used to distinguish
    /// a real screen change from ambient animation: compare a post-action delta
    /// against an idle baseline delta over the same interval.
    ///
    /// Returns 1.0 for mismatched dimensions — maximally different.
    pub fn diff_fraction(&self, other: &Frame, r: Region) -> f64 {
        if self.width != other.width || self.height != other.height {
            return 1.0;
        }
        let (x0, y0, x1, y1) = self.region_px(r);
        let total = ((x1 - x0) as i64 * (y1 - y0) as i64).max(1);
        let mut differing = 0i64;
        for y in y0..y1 {
            for x in x0..x1 {
                if self.pixel(x, y) != other.pixel(x, y) {
                    differing += 1;
                }
            }
        }
        differing as f64 / total as f64
    }

    /// Fraction of pixels with any channel above BLACK_THRESHOLD. A value near
    /// zero means PrintWindow returned an empty surface.
    pub fn nonblack_fraction(&self) -> f64 {
        let total = (self.bgra.len() / 4).max(1);
        let lit = self
            .bgra
            .chunks_exact(4)
            .filter(|p| p[0] > BLACK_THRESHOLD || p[1] > BLACK_THRESHOLD || p[2] > BLACK_THRESHOLD)
            .count();
        lit as f64 / total as f64
    }

    /// Writes a 32-bit BGRA bottom-up BMP. No image crate needed; opens in any viewer.
    pub fn write_bmp(&self, path: &std::path::Path) -> Result<(), crate::Error> {
        let stride = (self.width * 4) as usize;
        let pixels = stride * self.height as usize;
        let mut out = Vec::with_capacity(54 + pixels);
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&((54 + pixels) as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes()); // positive => bottom-up
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(pixels as u32).to_le_bytes());
        out.extend_from_slice(&[0u8; 16]);
        // Our buffer is top-down; BMP bottom-up wants rows reversed.
        for y in (0..self.height as usize).rev() {
            out.extend_from_slice(&self.bgra[y * stride..(y + 1) * stride]);
        }
        std::fs::write(path, out)?;
        Ok(())
    }
}

pub fn capture_window(win: &GameWindow) -> Result<Frame, crate::Error> {
    let (w, h) = win.client_size()?;
    if w <= 0 || h <= 0 {
        return Err(crate::Error::Win32("window has no client area".into()));
    }
    unsafe {
        let screen_dc = GetDC(HWND(std::ptr::null_mut()));
        let mem_dc = CreateCompatibleDC(screen_dc);
        let bmp: HBITMAP = CreateCompatibleBitmap(screen_dc, w, h);
        let old = SelectObject(mem_dc, bmp);

        let printed = PrintWindow(win.hwnd, mem_dc, PW_RENDERFULLCONTENT).as_bool();

        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // negative => top-down rows
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        let scanned = GetDIBits(
            mem_dc, bmp, 0, h as u32,
            Some(bgra.as_mut_ptr() as *mut _),
            &mut info, DIB_RGB_COLORS,
        );

        SelectObject(mem_dc, old);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(mem_dc);
        ReleaseDC(HWND(std::ptr::null_mut()), screen_dc);

        if !printed || scanned == 0 {
            return Err(crate::Error::Win32("PrintWindow capture failed".into()));
        }
        Ok(Frame { width: w, height: h, bgra })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: i32, h: i32, fill: u8) -> Frame {
        Frame { width: w, height: h, bgra: vec![fill; (w * h * 4) as usize] }
    }

    const FULL: Region = Region { nx: 0.0, ny: 0.0, nw: 1.0, nh: 1.0 };

    #[test]
    fn identical_frames_hash_identically() {
        let a = frame(8, 8, 7);
        let b = frame(8, 8, 7);
        assert_eq!(a.region_hash(FULL), b.region_hash(FULL));
        assert!(a.identical_region(&b, FULL));
        assert_eq!(a.diff_fraction(&b, FULL), 0.0);
    }

    #[test]
    fn a_change_inside_the_region_changes_the_hash() {
        let a = frame(8, 8, 7);
        let mut b = frame(8, 8, 7);
        b.bgra[0] = 9;
        assert_ne!(a.region_hash(FULL), b.region_hash(FULL));
        assert!(!a.identical_region(&b, FULL));
    }

    #[test]
    fn a_change_outside_the_region_is_ignored() {
        let a = frame(8, 8, 7);
        let mut b = frame(8, 8, 7);
        b.bgra[(7 * 8 + 7) * 4] = 9; // bottom-right pixel
        let quarter = Region { nx: 0.0, ny: 0.0, nw: 0.5, nh: 0.5 };
        assert_eq!(a.region_hash(quarter), b.region_hash(quarter));
    }

    #[test]
    fn diff_fraction_counts_differing_pixels_not_bytes() {
        let a = frame(4, 4, 0);
        let mut b = frame(4, 4, 0);
        // Change all four bytes of exactly two pixels: 2 of 16 => 0.125
        for p in [0usize, 5usize] {
            for c in 0..4 {
                b.bgra[p * 4 + c] = 200;
            }
        }
        assert_eq!(a.diff_fraction(&b, FULL), 2.0 / 16.0);
    }

    #[test]
    fn nonblack_fraction_ignores_near_black_pixels() {
        let mut f = frame(2, 2, 0);
        f.bgra[0] = 255; // one bright pixel of four
        assert_eq!(f.nonblack_fraction(), 0.25);
        assert_eq!(frame(2, 2, 0).nonblack_fraction(), 0.0);
    }

    #[test]
    fn mismatched_sizes_are_never_identical() {
        assert!(!frame(8, 8, 7).identical_region(&frame(4, 4, 7), FULL));
    }
}
