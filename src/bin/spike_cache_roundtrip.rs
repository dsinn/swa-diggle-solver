//! **#108's decisive test, and it needs no game.** Absorb a cache file into a fresh `WorldMap`,
//! write it straight back, and compare row for row. If the coordinates survive, the writer is
//! exonerated and the loss happened during the run; if they do not, the file is the crime scene.
//!
//! Scratch, and deleted once it has answered.
use diggle_solver::overworld::WorldMap;
use std::collections::BTreeMap;

fn rows(text: &str) -> (BTreeMap<String, Vec<String>>, usize) {
    let mut p = BTreeMap::new();
    let mut e = 0;
    for line in text.lines() {
        let f: Vec<String> = line.split('\t').map(str::to_string).collect();
        match f.first().map(String::as_str) {
            Some("p") => {
                p.insert(f[1].clone(), f.clone());
            }
            Some("e") => e += 1,
            _ => {}
        }
    }
    (p, e)
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: spike_cache_roundtrip <cache file>");
    let text = std::fs::read_to_string(&path).expect("readable");

    let mut map = WorldMap::default();
    let edges_in = map.absorb_cache(&text);
    let back = map.cache_text();

    let (before, e_before) = rows(&text);
    let (after, e_after) = rows(&back);

    println!("version in : {:?}", text.lines().next());
    println!("version out: {:?}", back.lines().next());
    println!("places {} -> {}", before.len(), after.len());
    println!("edge rows {e_before} -> {e_after} (absorb reported {edges_in})");

    let placed = |m: &BTreeMap<String, Vec<String>>| {
        m.values().filter(|f| f.get(3).map(|x| x != "-").unwrap_or(false)).count()
    };
    println!("placed {} -> {}", placed(&before), placed(&after));

    let mut lost = 0;
    let mut differ = 0;
    let mut missing = 0;
    for (key, b) in &before {
        let Some(a) = after.get(key) else {
            missing += 1;
            if missing <= 5 {
                println!("  MISSING after round trip: {key}");
            }
            continue;
        };
        if b[3] != "-" && a[3] == "-" {
            lost += 1;
            if lost <= 5 {
                println!("  LOST position: {key} was ({}, {})", b[3], b[4]);
            }
        }
        if b != a {
            differ += 1;
            if differ <= 8 {
                println!("  DIFFERS: {key}");
                println!("    in : {b:?}");
                println!("    out: {a:?}");
            }
        }
    }
    let added = after.keys().filter(|k| !before.contains_key(*k)).count();
    println!(
        "\nmissing {missing}, invented {added}, positions lost {lost}, rows differing {differ}"
    );
    println!(
        "VERDICT: {}",
        match (missing, added, lost, differ) {
            (0, 0, 0, 0) => "identical — the writer is exonerated, the loss happened in the run",
            _ => "the round trip is lossy — see above",
        }
    );
}
