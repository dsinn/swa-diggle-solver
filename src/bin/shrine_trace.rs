//! Plays one shrine word and shows the solver's reasoning turn by turn.
//!
//! For diagnosing a specific puzzle rather than measuring the whole list — when self-play reports a
//! word that runs past the budget, this is what says why.
//!
//! ```text
//! cargo run --release --bin shrine_trace -- faded wild
//! ```

use diggle_solver::shrine::{feedback, max_guesses, show, solved, Baked, Band, Solver};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let answer = args.next().ok_or("usage: shrine_trace <word> [easy|hard|wild]")?;
    let band = match args.next().as_deref() {
        Some("easy") => Band::Easy,
        Some("hard") => Band::Hard,
        _ => Band::Wild,
    };
    let length = answer.len();
    let budget = max_guesses(length);
    let mut solver = Solver::new(&Baked, length, band)?;
    let win = solved(length);

    println!("answer {answer:?}, {band:?}, {} candidates, {budget} guesses\n", solver.remaining());
    let mut worst = std::time::Duration::ZERO;
    for turn in 1..=budget {
        let started = std::time::Instant::now();
        let Some(guess) = solver.propose() else {
            println!("  candidate set emptied — a colouring was misread or the list is stale");
            return Ok(());
        };
        let thought = started.elapsed();
        worst = worst.max(thought);
        let pattern = feedback(guess.as_bytes(), answer.as_bytes());
        let before = solver.remaining();
        if pattern == win {
            println!(
                "{turn}. {guess}  {}  solved (from {before})  [{:.0} ms]",
                show(pattern, length),
                thought.as_secs_f64() * 1000.0
            );
            println!("\nslowest turn {:.0} ms", worst.as_secs_f64() * 1000.0);
            return Ok(());
        }
        solver.observe(&guess, pattern);
        println!(
            "{turn}. {guess}  {}  {before} -> {} left, {} guesses after this  [{:.0} ms]",
            show(pattern, length),
            solver.remaining(),
            solver.budget_left(),
            thought.as_secs_f64() * 1000.0
        );
        if solver.remaining() <= 20 {
            println!("     {}", solver.candidates().join(" "));
        }
    }
    println!("\nOUT OF GUESSES");
    Ok(())
}
