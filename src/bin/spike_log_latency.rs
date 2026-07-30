//! Spike 1: does the game's stdout reach us promptly, or is it block-buffered?
//!
//! Self-verifying: runs unattended for a fixed window, then prints a VERDICT
//! line and writes its findings document. No human observation required.
//!
//! Run: cargo run --bin spike_log_latency -- config.toml

use diggle_solver::{config::Config, game::launch::PipedGameProcess};
use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const OBSERVE: Duration = Duration::from_secs(25);
const FIRST_LINE_LIMIT: Duration = Duration::from_secs(5);
const MIN_LINES: usize = 20;
/// A gap this long or longer is treated as a potential buffer-flush boundary.
const GAP_THRESHOLD: Duration = Duration::from_millis(250);

struct Arrival {
    at: Duration,
    cumulative_bytes: usize,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: spike_log_latency <config.toml>");
    let cfg = Config::load(std::path::Path::new(&path))?;
    let mut game = PipedGameProcess::launch(&cfg)?;
    let stdout = game.stdout().expect("stdout pipe");

    let start = Instant::now();
    let (tx, rx) = mpsc::channel::<(Duration, usize)>();
    std::thread::spawn(move || {
        let mut cumulative = 0usize;
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            cumulative += line.len() + 1; // +1 for the newline the reader stripped
            if tx.send((start.elapsed(), cumulative)).is_err() {
                break;
            }
        }
    });

    let mut arrivals: Vec<Arrival> = Vec::new();
    while start.elapsed() < OBSERVE {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok((at, cumulative_bytes)) => arrivals.push(Arrival { at, cumulative_bytes }),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // Analyse.
    let first = arrivals.first().map(|a| a.at);
    let count = arrivals.len();
    let mut long_gaps = 0usize;
    let mut quantized = 0usize;
    let mut max_gap = Duration::ZERO;
    for w in arrivals.windows(2) {
        let gap = w[1].at.saturating_sub(w[0].at);
        if gap > max_gap {
            max_gap = gap;
        }
        if gap >= GAP_THRESHOLD {
            long_gaps += 1;
            // How far is the byte count at this boundary from a 4096 multiple?
            let rem = (w[0].cumulative_bytes % 4096) as i64;
            let dist = rem.min(4096 - rem);
            if dist <= 256 {
                quantized += 1;
            }
        }
    }

    let ok_first = first.map_or(false, |f| f <= FIRST_LINE_LIMIT);
    let ok_count = count >= MIN_LINES;
    let ok_unbuffered = long_gaps == 0 || (quantized * 2) < long_gaps;
    let pass = ok_first && ok_count && ok_unbuffered;

    let report = format!(
        "# Spike 1 — stdout buffering\n\n\
         VERDICT: {}\n\n\
         - first line at: {}\n\
         - lines in {}s window: {} (need >= {})\n\
         - max inter-arrival gap: {:.0} ms\n\
         - gaps >= {}ms: {}\n\
         - of those, near a 4096-byte boundary: {} {}\n\n\
         Criteria: first line <= {}s ({}), line count ({}), no 4KB quantization ({}).\n\n\
         If FAIL, the log cannot be trusted for timing: make \"saves are the clock\"\n\
         (design spec §5) mandatory and lengthen settle timeouts on save-less screens.\n",
        if pass { "PASS" } else { "FAIL" },
        first.map_or("never".to_string(), |f| format!("{:.2}s", f.as_secs_f64())),
        OBSERVE.as_secs(), count, MIN_LINES,
        max_gap.as_secs_f64() * 1000.0,
        GAP_THRESHOLD.as_millis(), long_gaps,
        quantized,
        if long_gaps > 0 && (quantized * 2) >= long_gaps { "<-- BUFFERING SIGNATURE" } else { "" },
        FIRST_LINE_LIMIT.as_secs(),
        if ok_first { "ok" } else { "FAILED" },
        if ok_count { "ok" } else { "FAILED" },
        if ok_unbuffered { "ok" } else { "FAILED" },
    );

    std::fs::create_dir_all("docs/superpowers/spikes")?;
    let mut f = std::fs::File::create("docs/superpowers/spikes/01-stdout-buffering.md")?;
    f.write_all(report.as_bytes())?;
    print!("{report}");

    // Leave no stray game process behind.
    let _ = game.kill();
    if !pass {
        std::process::exit(1);
    }
    Ok(())
}
