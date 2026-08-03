//! Searched vs exact scoring, measured offline over the saved frame corpus.
//!
//! Every `Button` in `act`'s registry carries a `search` box that is only ever its `origin` grown by
//! 8 px on each side. `act::locate` and the threshold tests hunt the template *anywhere* inside that
//! box and report the best of 17x17 = 289 candidate offsets; `act::score_exact` compares at `origin`
//! and nowhere else. A maximum over 289 tries is not the same statistic as a single comparison, and
//! against arbitrary background it is systematically higher — which is how a combat screen's
//! bottom-left statistics panel (wood grain in a wooden frame) matched the main menu's `Continue`
//! well enough to identify a live fight as the main menu.
//!
//! This binary measures the size of that inflation, per button and per frame, so a conversion to
//! exact scoring can be costed instead of guessed. It changes nothing: no thresholds move, no call
//! site is converted.
//!
//! Run with `cargo run --bin score_compare` from the repo root (the paths are relative to it).

use diggle_solver::act::{self, Button};
use diggle_solver::observe::template::{find_at_scale_in, Template};
use diggle_solver::win::capture::Frame;
use std::path::{Path, PathBuf};

/// The registry, plus `CONTINUE_HOT` — which `act::ALL` omits even though `act::identify` asks it.
fn buttons() -> Vec<(&'static Button, f64, &'static str)> {
    // (button, the threshold it is read against, the constant's name)
    vec![
        (&act::CONTINUE, act::CONTINUE_PRESENT, "CONTINUE_PRESENT"),
        (&act::CONTINUE_HOT, act::CONTINUE_PRESENT, "CONTINUE_PRESENT"),
        // `PROGRESS` has no `*_PRESENT` of its own; it is read through `locate`, whose bar is
        // `MIN_INLIERS`.
        (&act::PROGRESS, act::MIN_INLIERS, "MIN_INLIERS"),
        (&act::COMBAT_FINISH, act::COMBAT_FINISH_PRESENT, "COMBAT_FINISH_PRESENT"),
        (&act::COMBAT_EULOGISE, act::COMBAT_EULOGISE_PRESENT, "COMBAT_EULOGISE_PRESENT"),
        (&act::REWARD_CONFIRM, act::REWARD_SCREEN_PRESENT, "REWARD_SCREEN_PRESENT"),
        (&act::POSTGAME_CONTINUE, act::POSTGAME_CONTINUE_PRESENT, "POSTGAME_CONTINUE_PRESENT"),
        (&act::MENU_START, act::MENU_START_PRESENT, "MENU_START_PRESENT"),
        (&act::HEROSELECT_CONFIRM, act::HEROSELECT_CONFIRM_PRESENT, "HEROSELECT_CONFIRM_PRESENT"),
        (&act::CHARACTER_STATS, act::CHARACTER_STATS_PRESENT, "CHARACTER_STATS_PRESENT"),
        (&act::CHARACTER_BACK, act::CHARACTER_BACK_PRESENT, "CHARACTER_BACK_PRESENT"),
        (&act::PREGAME_START, act::PREGAME_START_PRESENT, "PREGAME_START_PRESENT"),
        (&act::SHRINE_PRAY, act::SHRINE_PRAY_PRESENT, "SHRINE_PRAY_PRESENT"),
        (&act::SHRINE_GOBACK, act::SHRINE_GOBACK_PRESENT, "SHRINE_GOBACK_PRESENT"),
        (&act::STATS_BACK, act::STATS_BACK_PRESENT, "STATS_BACK_PRESENT"),
        (&act::HEROSELECT_HEADER, act::HEROSELECT_HEADER_PRESENT, "HEROSELECT_HEADER_PRESENT"),
        (&act::UNLOCK_CONTINUE, act::UNLOCK_CONTINUE_PRESENT, "UNLOCK_CONTINUE_PRESENT"),
    ]
}

/// What is really on each corpus frame, read off the images by eye.
///
/// Needed for the false-positive verdict: a high score is only alarming on a frame where the button
/// is genuinely absent, and nothing in the data itself knows which those are.
///
/// `Variant` is deliberately neither: the same slot holds the same button in a different rendering
/// (an *active* `Confirm` where the template is the greyed one), so neither "should match" nor
/// "must not match" is the right expectation.
#[derive(PartialEq, Clone, Copy)]
enum Truth {
    Present,
    Absent,
    Variant,
}

fn truth(button: &Button, frame: &str) -> Truth {
    use Truth::*;
    match (button.name, frame) {
        ("combat Finish", "now.png") => Present,
        ("combat Finish", "waitphase-with-fighton.png") => Present,
        // Same object, same plank, different word — `rpg.lua:592-597`.
        ("combat Finish", "eulogise-at-death.png") => Variant,
        ("combat Eulogise", "eulogise-at-death.png") => Present,
        ("combat Eulogise", "now.png") => Variant,
        ("combat Eulogise", "waitphase-with-fighton.png") => Variant,
        ("reward Confirm", "post-crypt.png") => Present,
        // The active state of the same button.
        ("reward Confirm", "reward-selected.png") => Variant,
        ("hero select heading", "16-selected.png") => Present,
        // Genuinely on screen, wearing the `Adventure!` caption rather than the template's `Fight!`.
        ("hero select confirm", "16-selected.png") => Present,
        // Hero select draws a `back.png` plaque in the stats page's slot. Same art, same rect: this
        // is a real second occupant, not a coincidence of wood grain.
        ("stats history back", "16-selected.png") => Variant,
        // The overworld's options button shares the stats page's slot with a different icon.
        ("stats history back", "overworld-campfire.png") => Variant,
        _ => Absent,
    }
}

/// The corpus loader, duplicated from `act::threshold_tests::frame` rather than made `pub`.
fn frame(name: &str) -> Option<Frame> {
    let path = PathBuf::from("tests").join("frames").join(name);
    let dec = png::Decoder::new(std::fs::File::open(path).ok()?);
    let mut rdr = dec.read_info().ok()?;
    let mut buf = vec![0; rdr.output_buffer_size()];
    let info = rdr.next_frame(&mut buf).ok()?;
    let n = info.color_type.samples();
    let mut bgra = Vec::with_capacity((info.width * info.height * 4) as usize);
    for px in buf.chunks_exact(n) {
        bgra.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    Some(Frame { width: info.width as i32, height: info.height as i32, bgra })
}

fn crop(f: &Frame, (x0, y0, x1, y1): (i32, i32, i32, i32)) -> Frame {
    let (w, h) = (x1 - x0, y1 - y0);
    let mut bgra = Vec::with_capacity((w * h * 4) as usize);
    for y in y0..y1 {
        let row = (y * f.width + x0) as usize * 4;
        bgra.extend_from_slice(&f.bgra[row..row + (w as usize) * 4]);
    }
    Frame { width: w, height: h, bgra }
}

fn searched(f: &Frame, b: &Button, tpl: &Template) -> f64 {
    find_at_scale_in(&crop(f, b.search), tpl, 1.0, 1, None).map(|m| m.inliers).unwrap_or(0.0)
}

/// The offline twin of `act::score_exact`: the template-sized rect at `origin`, one candidate offset.
fn exact(f: &Frame, b: &Button, tpl: &Template) -> f64 {
    let (ox, oy) = b.origin;
    let rect = (ox, oy, ox + tpl.width as i32, oy + tpl.height as i32);
    find_at_scale_in(&crop(f, rect), tpl, 1.0, 1, None).map(|m| m.inliers).unwrap_or(0.0)
}

fn main() {
    // Fixed order, so the output diffs cleanly between runs.
    let frames = [
        // The frame that actually caused the misidentification: a live fight against a chest, 16
        // tiles settled, which `identify` called the main menu. Kept as a corpus frame so the claim
        // can be checked rather than asserted.
        "combat-chest.png",
        "16-selected.png",
        "eulogise-at-death.png",
        "now.png",
        "overworld-campfire.png",
        "post-crypt.png",
        "reward-selected.png",
        "waitphase-with-fighton.png",
        // Lost: it lived in the git-excluded live spike directory and a stalled run overwrote it.
        // Listed so its absence is reported rather than silently narrowing the corpus.
        "combat-stalled.png",
    ];
    let reg = buttons();
    let templates: Vec<Template> = reg
        .iter()
        .map(|(b, _, _)| {
            Template::load(&Path::new("templates").join(b.template))
                .unwrap_or_else(|e| panic!("{}: {e}", b.template))
        })
        .collect();

    println!(
        "{:<26} {:<28} {:>8} {:>8} {:>8} {:>7}  {}",
        "frame", "button", "searched", "exact", "delta", "thresh", "verdict"
    );
    println!("{}", "-".repeat(110));

    let mut identical = vec![];
    let mut inflated = vec![];
    let mut false_pos = vec![];
    let mut missing = vec![];
    // Per button: (max delta over all frames, max delta over frames where it IS present, how many
    // present frames it had, the worst `exact - threshold` margin over those present frames).
    let mut per_button: Vec<(f64, f64, usize, f64)> = vec![(0.0, 0.0, 0, f64::MAX); reg.len()];

    for fname in frames {
        let Some(f) = frame(fname) else {
            missing.push(fname);
            continue;
        };
        for (i, (b, thresh, _)) in reg.iter().enumerate() {
            let tpl = &templates[i];
            let s = searched(&f, b, tpl);
            let e = exact(&f, b, tpl);
            let d = s - e;
            let t = truth(b, fname);
            let mut flags: Vec<String> = vec![];
            if s >= *thresh && e < *thresh {
                flags.push("INFLATED".into());
                inflated.push((fname, b.name, s, e, *thresh, t));
            }
            if s >= *thresh && t == Truth::Absent {
                flags.push("FALSE-POS-RISK".into());
                false_pos.push((fname, b.name, s, e, *thresh));
            }
            if s >= *thresh && t == Truth::Variant {
                flags.push("variant-slot".into());
            }
            if d.abs() < 1e-9 {
                identical.push((fname, b.name));
            }
            per_button[i].0 = per_button[i].0.max(d);
            if t == Truth::Present {
                per_button[i].1 = per_button[i].1.max(d);
                per_button[i].2 += 1;
                per_button[i].3 = per_button[i].3.min(e - *thresh);
            }
            let mark = match t {
                Truth::Present => "[present]",
                Truth::Variant => "[variant]",
                Truth::Absent => "",
            };
            println!(
                "{:<26} {:<28} {:>8.4} {:>8.4} {:>8.4} {:>7.2}  {} {}",
                fname,
                b.name,
                s,
                e,
                d,
                thresh,
                flags.join(" "),
                mark
            );
        }
        println!();
    }

    println!("{}", "=".repeat(110));
    for m in &missing {
        println!("MISSING FROM CORPUS: {m} (not measured, not estimated)");
    }
    println!(
        "\nidentical searched==exact: {} of {} cells",
        identical.len(),
        reg.len() * (frames.len() - missing.len())
    );
    println!("\nINFLATED cells ({}):", inflated.len());
    for (f, b, s, e, t, truth) in &inflated {
        let note = match truth {
            Truth::Present => "the button IS present — exact conversion would BREAK this",
            Truth::Variant => "a variant of the button occupies the slot",
            Truth::Absent => "the button is absent — search inflation was the false positive",
        };
        println!("  {f:<26} {b:<28} searched {s:.4} >= {t:.2} > exact {e:.4}   {note}");
    }
    println!("\nFALSE-POS-RISK cells ({}):", false_pos.len());
    for (f, b, s, e, t) in &false_pos {
        println!("  {f:<26} {b:<28} searched {s:.4} >= {t:.2}, exact {e:.4}");
    }

    println!("\nPer button, over the {} frames measured:", frames.len() - missing.len());
    println!(
        "{:<28} {:>10} {:>14} {:>8}  {}",
        "button", "max delta", "delta on true+", "true+ n", "conversion cost"
    );
    println!("{}", "-".repeat(100));
    for (i, (b, _, _)) in reg.iter().enumerate() {
        let (max_d, present_d, n, margin) = per_button[i];
        // The question a conversion has to answer is not "did the number move" but "does the true
        // positive still clear its bar". With no positive control in the corpus the honest answer is
        // that it is unmeasured, not that it is safe.
        let cost = if n == 0 {
            "UNMEASURED - no positive control in corpus".to_string()
        } else {
            format!(
                "free: {n} positive control(s), worst exact margin +{margin:.4} over threshold"
            )
        };
        println!("{:<28} {:>10.4} {:>14.4} {:>8}  {cost}", b.name, max_d, present_d, n);
    }
}
