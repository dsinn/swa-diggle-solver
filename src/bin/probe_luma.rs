//! Prints the luminance at each tile centre of a saved frame.
//!
//! Used to find the separation between an occupied tile face and the bare backboard, so the
//! "board is full" gate rests on a measured number rather than a guessed one.
use diggle_solver::geometry::Geometry;
use diggle_solver::layout;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: probe_luma <frame.png>")?;
    let dec = png::Decoder::new(std::fs::File::open(Path::new(&path))?);
    let mut rdr = dec.read_info()?;
    let mut buf = vec![0; rdr.output_buffer_size()];
    let info = rdr.next_frame(&mut buf)?;
    let (w, h) = (info.width as i32, info.height as i32);
    let ch = info.color_type.samples();

    let g = Geometry::default();
    let centres = layout::tile_centres(&g, w, h);
    let radius = (layout::tile_radius(w, h) * 0.55).round() as i32;

    println!("frame {w}x{h}, {} channels, radius {radius}", ch);
    for (i, &(cx, cy)) in centres.iter().enumerate() {
        let mut sum = 0f64;
        let mut n = 0f64;
        for y in (cy - radius).max(0)..=(cy + radius).min(h - 1) {
            for x in (cx - radius).max(0)..=(cx + radius).min(w - 1) {
                let p = ((y * w + x) as usize) * ch;
                let (r, gg, b) = (buf[p] as f64, buf[p + 1] as f64, buf[p + 2] as f64);
                sum += 0.299 * r + 0.587 * gg + 0.114 * b;
                n += 1.0;
            }
        }
        let (col, row) = g.position(i).unwrap();
        println!("  tile {i:>2} (col {col}, row {row}) at ({cx},{cy}): luma {:.1}", sum / n);
    }
    Ok(())
}
