//! Sweeps item icons taken straight from the game's own art against a hero-select frame.
//!
//! The question this answers is narrow and offline: **is a champion card's `Passives:` row drawn
//! from the icon PNGs at 1:1, so that a class can be named without OCR?**
//!
//! `ui/elements/herodisplay.lua:196` draws each item as `love.graphics.draw(imageCache(item.icon),
//! x, y)` — no scale arguments — and `hero.passives` is never appended to by the roll
//! (`ui/heroselect.lua:86-175` only ever touches `potentialBoons` and `potentialCurses`), so the
//! first icon of that row is the class's own first starting passive and nothing else.
//!
//! ```text
//! cargo run --bin spike_card_class -- <frame.png> <x0,y0,x1,y1> <icon.png>...
//! ```

use diggle_solver::observe::template::{sweep_in, Template};
use diggle_solver::win::capture::Frame;
use std::path::Path;

fn load(path: &Path) -> Result<Frame, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut rdr = png::Decoder::new(file)
        .read_info()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = vec![0; rdr.output_buffer_size()];
    let info = rdr.next_frame(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
    let n = info.color_type.samples();
    let mut bgra = Vec::with_capacity((info.width * info.height * 4) as usize);
    for px in buf.chunks_exact(n) {
        bgra.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    Ok(Frame { width: info.width as i32, height: info.height as i32, bgra })
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        return Err("usage: spike_card_class <frame.png> <x0,y0,x1,y1> <icon.png>...".into());
    }
    let frame = load(Path::new(&args[0]))?;
    let r: Vec<i32> = args[1].split(',').filter_map(|s| s.trim().parse().ok()).collect();
    if r.len() != 4 {
        return Err("the rectangle wants four numbers".into());
    }
    let bounds = Some((r[0], r[1], r[2], r[3]));
    println!("frame {}x{}  search {:?}", frame.width, frame.height, bounds.unwrap());

    // One scale, because that is the claim: at 1920x1080 the card is drawn 1:1 and the icon file is
    // the pixels. Sweeping other scales only offers the matcher more chances to find a near-miss
    // where it should be reporting an absence.
    let scales = [1.0];
    for icon in &args[2..] {
        let tpl = Template::load(Path::new(icon)).map_err(|e| format!("{icon}: {e}"))?;
        let opaque = tpl.opaque_fraction(diggle_solver::observe::template::ALPHA_MIN);
        let hits = sweep_in(&frame, &tpl, &scales, 1, bounds);
        print!("{:<34} {}x{} opaque {opaque:.2}", tpl.name, tpl.width, tpl.height);
        match hits.first() {
            Some(m) => println!(
                "  best {:.4} err {:.4} at ({},{}) scale {:.3}",
                m.inliers, m.error, m.x, m.y, m.scale
            ),
            None => println!("  no match"),
        }
        for m in hits.iter().skip(1).take(2) {
            println!("{:34}   next {:.4} at ({},{}) scale {:.3}", "", m.inliers, m.x, m.y, m.scale);
        }
    }
    Ok(())
}
