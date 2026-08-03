//! What should we guess next, given the colourings we have already seen?
//!
//! [`shrine_trace`](../shrine_trace/index.html) plays a puzzle whose answer is known, which is what
//! measures the solver. This is the other half: the answer is *not* known, the colourings come off a
//! real screen, and the question is only ever "what now". That makes it the offline twin of the live
//! driver — the same [`Solver`] calls in the same order — so a disagreement between this and a live
//! run isolates the fault to the screen reading rather than the search.
//!
//! ```text
//! cargo run --release --bin shrine_next -- 4 wild lare:.G.. away:Y...
//! ```
//!
//! Patterns are `G` green, `Y` yellow, `.` grey, in [`show`]'s rendering, left to right.

use diggle_solver::shrine::{max_guesses, parse_pattern, show, solved, Baked, Band, Solver};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let length: usize = args
        .next()
        .ok_or("usage: shrine_next <length> <easy|hard|wild> [guess:pattern ...]")?
        .parse()?;
    // Wild is the safe default and the reason is asymmetric: a band that excludes the true answer
    // empties the candidate set and the shrine becomes unwinnable, while a band that is too wide
    // only costs guess quality. `shrine.lua:497` picks by the `shrineWordHard` gear flag, which we
    // do not read yet.
    let band = match args.next().as_deref() {
        Some("easy") => Band::Easy,
        Some("hard") => Band::Hard,
        _ => Band::Wild,
    };

    let mut solver = Solver::new(&Baked, length, band)?;
    let win = solved(length);
    println!("{length} letters, {band:?}, {} candidates", solver.remaining());

    for arg in args {
        let (guess, pattern) = arg.split_once(':').ok_or(format!("expected guess:pattern, got {arg:?}"))?;
        if guess.len() != length {
            return Err(format!("{guess:?} is not {length} letters").into());
        }
        let p = parse_pattern(pattern).ok_or(format!("bad pattern {pattern:?}"))?;
        if pattern.len() != length {
            return Err(format!("pattern {pattern:?} is not {length} long").into());
        }
        solver.observe(guess, p);
        println!("  {guess}  {}  -> {} candidates", show(p, length), solver.remaining());
        if p == win {
            println!("\nalready solved");
            return Ok(());
        }
    }

    match solver.remaining() {
        0 => {
            // The one failure that is silent in play: every candidate eliminated means a colouring
            // was misread or the baked list is stale, and neither is recoverable by guessing on.
            println!("\nNO CANDIDATES LEFT — a colouring was misread, or the word list is stale");
        }
        n => {
            if n <= 12 {
                println!("\ncandidates: {}", solver.candidates().join(" "));
            }
            match solver.propose() {
                Some(g) => println!(
                    "\nguess: {g}   ({n} candidates, {} of {} guesses left)",
                    solver.budget_left(),
                    max_guesses(length)
                ),
                None => println!("\nno proposal"),
            }
        }
    }
    Ok(())
}
