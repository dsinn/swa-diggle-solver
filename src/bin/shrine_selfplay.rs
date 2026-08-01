//! Self-play control: does the shrine solver actually fit the guess budget?
//!
//! Plays **every word** in each `(length, band)` answer list against the solver, feeding it real
//! colourings computed by the same `feedback` the tests pin to `shrineview.lua`. Reports the mean,
//! the worst case, and — the number that decides whether this is shippable — how many puzzles the
//! solver fails to finish within `maxGuesses`.
//!
//! This is the positive control for the whole shrine effort. A solver that averages 4 guesses is
//! useless if its tail runs past the budget, and the tail is exactly what an average hides.
//!
//! The opening guess is computed once per configuration and reused, which is both a large speedup
//! and the honest model of how it will run live: the opener depends only on the word list, so it is
//! precomputed rather than re-derived at every shrine.
//!
//! Touches no game files and drives no input.

use diggle_solver::shrine::{feedback, max_guesses, solved, Baked, Band, Solver};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    println!("# Shrine solver self-play\n\n{threads} threads\n");
    println!("| L | band | answers | budget | mean | worst | over budget | opener |");
    println!("|---|---|---|---|---|---|---|---|");

    let mut total_failures = 0usize;
    for length in 4usize..=7 {
        for band in [Band::Easy, Band::Hard, Band::Wild] {
            let started = Instant::now();
            let budget = max_guesses(length);

            // Derived from the word list, deliberately not read from the baked OPENERS table: this
            // run is what regenerates that table, so consulting it would make it self-confirming.
            let probe = Solver::new(&Baked, length, band)?;
            let n = probe.remaining();
            let opener = probe.compute_opener().ok_or("no opener")?;
            let baked = probe.propose().ok_or("no opener")?;
            if baked != opener {
                println!("\n  **OPENERS is stale**: baked `{baked}`, derived `{opener}`\n");
            }
            drop(probe);

            let cursor = Arc::new(AtomicUsize::new(0));
            let opener = Arc::new(opener);
            let mut handles = Vec::new();
            for _ in 0..threads {
                let cursor = Arc::clone(&cursor);
                let opener = Arc::clone(&opener);
                handles.push(std::thread::spawn(move || -> Result<Tally, String> {
                    let mut solver = Solver::new(&Baked, length, band).map_err(|e| e.to_string())?;
                    let answers = solver.candidates();
                    let mut tally = Tally::default();
                    loop {
                        let i = cursor.fetch_add(1, Ordering::Relaxed);
                        if i >= answers.len() {
                            break;
                        }
                        solver.reset();
                        let used = play(&mut solver, &answers[i], &opener, budget);
                        tally.record(used, &answers[i], budget);
                    }
                    Ok(tally)
                }));
            }
            let mut tally = Tally::default();
            for h in handles {
                tally.merge(h.join().map_err(|_| "worker panicked")??);
            }

            total_failures += tally.over;
            println!(
                "| {length} | {band:?} | {n} | {budget} | {:.3} | {} | {} | `{opener}` |",
                tally.mean(),
                tally.worst,
                if tally.over == 0 { "**0**".to_string() } else { format!("**{}**", tally.over) },
            );
            if !tally.examples.is_empty() {
                println!("\n  over budget: {}\n", tally.examples.join(", "));
            }
            let _ = started;
        }
    }

    println!(
        "\n{}",
        if total_failures == 0 {
            "**Every word in every band is solved within the game's budget.**".to_string()
        } else {
            format!("**{total_failures} puzzles exceed the budget** — the tail needs an exact endgame search.")
        }
    );
    Ok(())
}

/// Plays one puzzle, returning how many guesses it took, or `budget + 1` if it ran out.
fn play(solver: &mut Solver, answer: &str, opener: &str, budget: usize) -> usize {
    let target = answer.as_bytes();
    let win = solved(solver.length());
    for turn in 1..=budget {
        let guess = if turn == 1 {
            opener.to_string()
        } else {
            match solver.propose() {
                Some(g) => g,
                // The candidate set emptied, which can only mean a bug -- the true answer must
                // survive every filter derived from its own colourings.
                None => return budget + 1,
            }
        };
        let pattern = feedback(guess.as_bytes(), target);
        if pattern == win {
            return turn;
        }
        solver.observe(&guess, pattern);
    }
    budget + 1
}

#[derive(Default)]
struct Tally {
    games: usize,
    guesses: usize,
    worst: usize,
    over: usize,
    examples: Vec<String>,
}

impl Tally {
    fn record(&mut self, used: usize, answer: &str, budget: usize) {
        self.games += 1;
        self.guesses += used.min(budget);
        if used > budget {
            self.over += 1;
            if self.examples.len() < 12 {
                self.examples.push(answer.to_string());
            }
        } else {
            self.worst = self.worst.max(used);
        }
    }

    fn merge(&mut self, other: Tally) {
        self.games += other.games;
        self.guesses += other.guesses;
        self.worst = self.worst.max(other.worst);
        self.over += other.over;
        for e in other.examples {
            if self.examples.len() < 12 {
                self.examples.push(e);
            }
        }
    }

    fn mean(&self) -> f64 {
        if self.games == 0 { 0.0 } else { self.guesses as f64 / self.games as f64 }
    }
}
