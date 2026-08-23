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
//! precomputed rather than re-derived at every shrine. It is derived here rather than read from
//! `shrine::OPENERS`, and disagreement is reported — this run is what regenerates that table, so
//! consulting it would let the constant confirm itself.
//!
//! Touches no game files and drives no input.
//!
//! ## Result, v52.3 — 71,268 puzzles, zero failures
//!
//! | L | budget | easy | hard | wild | opener (wild) |
//! |---|---|---|---|---|---|
//! | 4 | 8 | 3.954 | 4.389 | 4.568 | `lare` |
//! | 5 | 6 | 3.340 | 3.822 | 3.917 | `raise` |
//! | 6 | 6 | 3.040 | 3.475 | 3.528 | `tanier` |
//! | 7 | 6 | 2.815 | 3.148 | 3.199 | `saltier` |
//!
//! Means, over every word in the band. Worst case never exceeds the budget. Slowest single turn
//! measured at 30 ms. Longer words come out *easier* — 3^7 patterns collapse a candidate set far
//! faster than 3^4 — and the L=5 opener derived independently as `raise`, one of the literature's
//! known-good openers, which is a free positive control.
//!
//! ## Getting to zero took two fixes, both found here rather than foreseen
//!
//! 1. First run failed on `coved`, `wowed`, `zagging` — all one-letter clusters. The cause was the
//!    *prefilter*, not the score: ranking probes by how much of the live set their letters cover
//!    promotes words containing `owed` against the `_owed` family, which are exactly the letters
//!    that cannot separate them. Fixed by scoring the full pool below `EXACT_POOL_MAX` candidates.
//! 2. `faded` survived that. `shrine_separate` showed no legal guess separates its five-candidate
//!    endgame *at all*, so the exact search was right to decline and the fix had to come earlier.
//!    Searching from larger sets worked but cost 35 s on one puzzle; the expense is depth, not
//!    breadth, hence `ENDGAME_DEPTH`.
//!
//! ## Not run
//!
//! The published-Wordle control — self-play against the real 2,315 / 12,972 lists, expecting a
//! 3.43–3.47 mean — needs word lists this repo does not ship. The duplicate-letter rules are pinned
//! instead to `shrineview.lua:164-175` by hand-worked unit tests, which is the more direct check.
//! The Wordle control would add independent confirmation and is worth running if `feedback` is ever
//! touched.

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
    // Lines for `data/shrine-hardest.txt` — see `Tally::at_the_limit`.
    let mut hardest: Vec<String> = Vec::new();
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
                    let mut solver =
                        Solver::new(&Baked, length, band).map_err(|e| e.to_string())?;
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
            // The zero-slack set, in the format `data/shrine-hardest.txt` uses. Sorted so the file
            // is stable across runs — the work is threaded, so arrival order is not.
            tally.at_the_limit.sort();
            for w in &tally.at_the_limit {
                hardest.push(format!("{length}\t{band:?}\t{w}"));
            }
            let _ = started;
        }
    }

    // Written rather than printed: it is generated data with a consumer
    // (`shrine::tests::the_hardest_word_of_each_shrine_is_still_solved_in_time`), and a file nobody
    // has to copy out of a terminal is a file that stays current.
    let path = std::path::Path::new("data/shrine-hardest.txt");
    let mut out = String::from(
        "# Shrine answers that win on the LAST allowed guess — zero slack for a misread.\n\
         # Generated by `cargo run --release --bin shrine_selfplay`. Do not hand-edit.\n\
         # length<TAB>band<TAB>word\n",
    );
    for line in &hardest {
        out.push_str(line);
        out.push('\n');
    }
    std::fs::write(path, out)?;
    println!("\nwrote {} zero-slack answers to {}", hardest.len(), path.display());

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
    /// Every answer that took the **whole** budget, for `data/shrine-hardest.txt`.
    ///
    /// The tail this binary already measures says nothing goes over. What it did not say is how many
    /// land exactly *on* the line, and that is the number that matters live: a shrine won on the last
    /// guess has no room for a misread colouring, so one wrong classification is not a lost turn, it
    /// is a lost shrine. 346 answers across eight configurations are in that position.
    ///
    /// Collected here rather than in a test of its own because this binary is already the exhaustive
    /// sweep — it plays every word in every band — and two sweeps would be two things to keep true.
    at_the_limit: Vec<String>,
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
            if used == budget {
                self.at_the_limit.push(answer.to_string());
            }
        }
    }

    fn merge(&mut self, other: Tally) {
        self.games += other.games;
        self.guesses += other.guesses;
        self.worst = self.worst.max(other.worst);
        self.over += other.over;
        self.at_the_limit.extend(other.at_the_limit);
        for e in other.examples {
            if self.examples.len() < 12 {
                self.examples.push(e);
            }
        }
    }

    fn mean(&self) -> f64 {
        if self.games == 0 {
            0.0
        } else {
            self.guesses as f64 / self.games as f64
        }
    }
}
