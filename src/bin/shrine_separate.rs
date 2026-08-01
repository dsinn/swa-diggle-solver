//! Does any legal guess tell a given set of candidates completely apart?
//!
//! The question that decides whether a losing endgame was a search failure or a genuine dead end.
//! If no single guess separates the set, no amount of lookahead at that point helps and the fix has
//! to come earlier in the game.
//!
//! ```text
//! cargo run --release --bin shrine_separate -- baaed eaved faded gaged jaded
//! ```

use diggle_solver::shrine::{feedback, solved, Baked, Band, WordSource};
use std::collections::{HashMap, HashSet};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let words: Vec<String> = std::env::args().skip(1).collect();
    if words.len() < 2 {
        return Err("give two or more candidate words".into());
    }
    let length = words[0].len();
    if words.iter().any(|w| w.len() != length) {
        return Err("all candidates must be the same length".into());
    }
    let guesses = Baked.guesses(length)?;
    let win = solved(length);

    let mut best = 0usize;
    let mut full: Vec<String> = Vec::new();
    for gi in 0..guesses.len() {
        let g = guesses.get(gi);
        let mut seen: HashMap<u16, usize> = HashMap::new();
        for w in &words {
            *seen.entry(feedback(g, w.as_bytes())).or_insert(0) += 1;
        }
        // A bucket that is all-green has already won, so it does not need separating further.
        let distinct = seen.len();
        if distinct > best {
            best = distinct;
        }
        if distinct == words.len() {
            full.push(String::from_utf8_lossy(g).into_owned());
        }
        let _ = win;
    }

    println!("{} candidates: {}", words.len(), words.join(" "));
    println!("best separation by any legal guess: {best} buckets");
    if full.is_empty() {
        println!("\nNo guess separates them completely — this position cannot be resolved in one.");
    } else {
        let show: Vec<&String> = full.iter().take(20).collect();
        println!("\n{} guesses separate them completely, e.g. {:?}", full.len(), show);
    }

    // Where the ambiguity lives: which pairs no guess can split.
    let mut stuck: Vec<(String, String)> = Vec::new();
    for i in 0..words.len() {
        for j in i + 1..words.len() {
            let splits = (0..guesses.len()).any(|gi| {
                let g = guesses.get(gi);
                feedback(g, words[i].as_bytes()) != feedback(g, words[j].as_bytes())
            });
            if !splits {
                stuck.push((words[i].clone(), words[j].clone()));
            }
        }
    }
    if !stuck.is_empty() {
        println!("\nindistinguishable pairs: {stuck:?}");
    }
    let _: HashSet<()> = HashSet::new();
    Ok(())
}
