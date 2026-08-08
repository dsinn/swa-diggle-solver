//! One-shot generator: extracts the shrine word lists from the game and writes them to `data/`.
//!
//! Run when the game version changes. The output is embedded with `include_str!` (see
//! [`diggle_solver::shrine::words`]), so the runtime never parses a 345,000-entry Lua file to learn
//! which words exist.
//!
//! ## Two different sources, because the game uses two
//!
//! - **Answers** come from `utils/lexica/shrine{4,5,6,7}.lua` — `word = ngram` pairs. The ngram is a
//!   commonness score, and `shrineDifficulty` (`utils/words.lua:28-32`) bands it with *strict*
//!   inequalities: `easy = {1, huge}`, `hard = {0, 1}`. Every ngram is positive and none is exactly
//!   1, so the two bands partition the file and `wild` is simply both concatenated.
//! - **Guesses** are validated against the *whole* dictionary (`shrineview.lua:261`), not the shrine
//!   list. So legal probe words vastly outnumber possible answers, which is what makes the endgame
//!   tractable.
//!
//! ## Why the dictionary is walked rather than parsed
//!
//! `utils/dictionary.lua` is 345,215 lines of `word="definition"`. Evaluating it to get 40,000 keys
//! would be absurd, but a naive line scan is *wrong*: a handful of definitions are Lua multi-line
//! strings continued with a trailing backslash, and their continuation lines are not entries. The
//! walk below tracks that state, and the run is asserted against a known entry count so a format
//! change fails loudly instead of silently dropping words.
//!
//! Being conservative here is the safe direction. A word we miss costs us a probe option; a word we
//! *invent* would be rejected at the shrine and waste a round trip.

use std::collections::BTreeMap;
use std::path::Path;

/// Total `key = value` entries in `utils/dictionary.lua` at v52.4.
///
/// Not a checksum of the words we keep — a check that the *walk* still understands the file. If the
/// serialiser changes quoting or line breaking, this moves and the generator refuses to run.
///
/// 345,217 lines, less `return {` and `}`, less the 5 continuation lines belonging to the two
/// multi-line definitions (`achalasia` at 2227 and `roon` at 255581). Counted directly:
/// `awk '{if (substr($0, length($0), 1) == "\\") n++} END {print n}'` reports 5 backslash-terminated
/// lines, so 5 of the following lines are string continuations rather than entries.
///
/// Was 345,208 at v52.3. The bump to 345,210 is two added entries, not a format change: the line
/// count rose by the same two, the continuation count is still 5, and the walk reported no duplicate
/// keys. Re-derive those three before moving this number again — a serialiser change would shift the
/// count without shifting the lines, and the walk would then be silently wrong.
const EXPECTED_DICT_ENTRIES: usize = 345_210;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = diggle_solver::config::Config::load(Path::new("config.toml"))?;
    let g = &cfg.game_dir;
    let out = Path::new("data");
    std::fs::create_dir_all(out)?;

    // ---- answers, banded ----
    for len in 4usize..=7 {
        let src = std::fs::read_to_string(g.join(format!("utils/lexica/shrine{len}.lua")))?;
        let mut easy = Vec::new();
        let mut hard = Vec::new();
        for (word, ngram) in read_shrine_lexicon(&src) {
            if word.len() != len {
                return Err(format!("shrine{len}.lua has a {}-letter word {word:?}", word.len()).into());
            }
            // Strict bounds, exactly as `words.getRandomShrine` applies them. A word sitting exactly
            // on a bound would belong to no band and could never be the answer -- worth knowing about
            // rather than silently filing under one side.
            if ngram > 1.0 {
                easy.push(word);
            } else if ngram > 0.0 && ngram < 1.0 {
                hard.push(word);
            } else {
                return Err(format!("{word:?} has ngram {ngram}, which no difficulty band admits").into());
            }
        }
        write_list(&out.join(format!("shrine-answers-{len}-easy.txt")), &easy)?;
        write_list(&out.join(format!("shrine-answers-{len}-hard.txt")), &hard)?;
        println!("answers {len}: easy {} hard {} total {}", easy.len(), hard.len(), easy.len() + hard.len());
    }

    // ---- guesses ----
    let dict = std::fs::read_to_string(g.join("utils/dictionary.lua"))?;
    let keys = read_dictionary_keys(&dict)?;
    for len in 4usize..=7 {
        let words: Vec<String> = keys
            .iter()
            .filter(|w| w.len() == len && w.bytes().all(|b| b.is_ascii_lowercase()))
            .cloned()
            .collect();
        write_list(&out.join(format!("shrine-guesses-{len}.txt")), &words)?;
        println!("guesses {len}: {}", words.len());
    }
    Ok(())
}

fn write_list(path: &Path, words: &[String]) -> std::io::Result<()> {
    let mut sorted: Vec<&String> = words.iter().collect();
    sorted.sort();
    let mut body = String::with_capacity(words.iter().map(|w| w.len() + 1).sum());
    for w in sorted {
        body.push_str(w);
        body.push('\n');
    }
    std::fs::write(path, body)
}

/// `word=0.028659,` lines. Plain data, one entry per line, no strings to escape.
fn read_shrine_lexicon(src: &str) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for line in src.lines() {
        let line = line.trim();
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim_end_matches(',');
        let (Ok(ngram), true) = (v.parse::<f64>(), k.bytes().all(|b| b.is_ascii_lowercase())) else {
            continue;
        };
        out.push((k.to_string(), ngram));
    }
    out
}

/// Every key in `utils/dictionary.lua`, skipping continuation lines of multi-line string values.
fn read_dictionary_keys(src: &str) -> Result<Vec<String>, String> {
    let mut keys = Vec::new();
    let mut entries = 0usize;
    // A value string continued onto the next line ends with a backslash, so the following line is
    // part of that string and must not be read as an entry.
    let mut continuing = false;
    for line in src.lines() {
        let ends_open = line.ends_with('\\');
        if continuing {
            continuing = ends_open;
            continue;
        }
        continuing = ends_open;
        if line == "return {" || line == "}" || line.is_empty() {
            continue;
        }
        let Some(eq) = line.find('=') else { continue };
        entries += 1;
        let raw = &line[..eq];
        // Identifier-safe keys are bare; anything else -- reserved words like `["else"]`, and keys
        // with apostrophes or spaces -- is bracket-quoted by the serialiser.
        let key = match raw.strip_prefix("[\"").and_then(|r| r.strip_suffix("\"]")) {
            Some(quoted) => quoted,
            None if !raw.starts_with('[') => raw,
            None => continue, // some other bracketed form; skip rather than guess
        };
        keys.push(key.to_string());
    }
    if entries != EXPECTED_DICT_ENTRIES {
        return Err(format!(
            "read {entries} dictionary entries, expected {EXPECTED_DICT_ENTRIES}; \
             the file format changed and this walk can no longer be trusted"
        ));
    }
    // Duplicate keys would mean the walk mis-split a line.
    let unique: BTreeMap<&String, ()> = keys.iter().map(|k| (k, ())).collect();
    if unique.len() != keys.len() {
        return Err(format!("{} duplicate keys in the dictionary walk", keys.len() - unique.len()));
    }
    Ok(keys)
}
