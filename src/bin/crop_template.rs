//! Cuts a template out of a saved frame: `crop_template <in.png> <out.png> <x> <y> <w> <h>`.
//!
//! Templates are normally captured live, straight from the game window. That is not possible for a
//! state we cannot summon on demand — the main menu's `Continue` renders highlighted when it is
//! reached through the options menu, and only then, which is why the run kept meeting it and no
//! capture of it existed. The evidence survives as a full-window frame, so the crop comes out of the
//! file rather than off the screen.
use diggle_solver::observe::template::Template;
use diggle_solver::win::capture::Frame;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() != 6 {
        return Err("usage: crop_template <in.png> <out.png> <x> <y> <w> <h>".into());
    }
    let (x, y, w, h): (i32, i32, i32, i32) =
        (a[2].parse()?, a[3].parse()?, a[4].parse()?, a[5].parse()?);
    let src = Template::load(std::path::Path::new(&a[0]))?;
    let (sw, sh) = (src.width as i32, src.height as i32);
    if x < 0 || y < 0 || x + w > sw || y + h > sh {
        return Err(format!("({x},{y}) {w}x{h} does not fit inside {sw}x{sh}").into());
    }
    // `Template` holds RGBA and `Frame` holds BGRA, so the channels are swapped on the way across
    // rather than assumed compatible — they are the same four bytes in a different order, and a
    // template written with them transposed matches nothing and looks merely "wrong colour".
    let mut bgra = Vec::with_capacity((w * h * 4) as usize);
    for row in 0..h {
        let start = (((y + row) * sw + x) * 4) as usize;
        for px in src.rgba[start..start + (w * 4) as usize].chunks_exact(4) {
            bgra.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    Frame { width: w, height: h, bgra }.write_png(std::path::Path::new(&a[1]))?;
    println!("wrote {} ({w}x{h}) from ({x},{y}) of {}", a[1], a[0]);
    Ok(())
}
