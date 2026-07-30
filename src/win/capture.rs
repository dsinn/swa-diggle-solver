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

/// Bounds of the START MENU's Continue + Start buttons, derived from their ButtonSpecs.
/// Measured idle noise floor 0.0000 across 20 frames; a centre-screen region scored
/// 0.5228 because it overlapped the animated mascot.
///
/// There is deliberately NO single global fingerprint region. This one lands on empty
/// background on the hero-select screen, where it hashes blank gradient and looks
/// convincingly stable while measuring nothing characteristic of that screen. Every
/// screen carries its own region, chosen from its own controls. See design v2 §6.1.
pub const START_MENU_REGION: Region =
    Region { nx: 0.0326, ny: 0.7037, nw: 0.2930, nh: 0.0926 };

/// The bottom-right **progress button** — the wooden plaque with a right-pointing arrow that
/// advances a cutscene or dialogue. Measured from a live arrival cutscene at 1920x1080:
/// client (1800,905)-(1910,1015).
///
/// Chosen deliberately *inside the plaque face*, well clear of its ornate scroll edge and of the
/// screen edge that clips the button on the right. The whole box is button material: byte-identical
/// across three cutscene frames 1.2 s apart while the scene behind it animated. That is the
/// property the F1 grid probe lacked — it hashed (0,860)-(320,975), which included map and sea
/// around a panel, so panning alone changed the hash and 15 probes produced 7 phantom states.
///
/// **Scope of the verification, stated because it has bitten this project before:** stable within
/// one cutscene, on one screen, at one resolution. "Stable across three samples in one session" is
/// not "stable across sessions" (design v2 §6.1) — a heading region passed exactly that bar and
/// then failed, because the parallax behind it is re-rolled per run. Before relying on this to
/// identify a *screen*, re-measure it on the other screens that carry the button. For merely
/// deciding whether the button is PRESENT, prefer template-matching its face: a match tolerates
/// whatever is behind and around it, where a hash requires the entire box to be invariant.
pub const PROGRESS_BUTTON_REGION: Region =
    Region { nx: 0.9375, ny: 0.8380, nw: 0.0573, nh: 0.1019 };

/// Where to click the progress button, in client pixels at 1920x1080: the centre of its visible
/// face. Not the centre of the *button* — it is clipped by the right screen edge, so a geometric
/// centre would fall off-screen.
pub const PROGRESS_BUTTON_CLICK: (i32, i32) = (1855, 960);

impl Region {
    /// Builds a Region from a pixel rectangle on a frame of the given size, so candidate
    /// fingerprint regions can be measured against a live screen.
    pub fn from_px(x0: i32, y0: i32, x1: i32, y1: i32, w: i32, h: i32) -> Region {
        Region {
            nx: x0 as f64 / w as f64,
            ny: y0 as f64 / h as f64,
            nw: (x1 - x0) as f64 / w as f64,
            nh: (y1 - y0) as f64 / h as f64,
        }
    }
}

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

    /// Writes a PNG. Lives here rather than in a binary so spikes can produce frames that are
    /// directly viewable — measuring a UI bounding box means looking at it, not guessing at it.
    pub fn write_png(&self, path: &std::path::Path) -> Result<(), crate::Error> {
        let file = std::fs::File::create(path)?;
        let mut enc =
            png::Encoder::new(std::io::BufWriter::new(file), self.width as u32, self.height as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer =
            enc.write_header().map_err(|e| crate::Error::Win32(e.to_string()))?;
        // Frame is top-down BGRA; PNG wants RGBA.
        let mut rgba = Vec::with_capacity(self.bgra.len());
        for px in self.bgra.chunks_exact(4) {
            rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
        }
        writer.write_image_data(&rgba).map_err(|e| crate::Error::Win32(e.to_string()))?;
        Ok(())
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
