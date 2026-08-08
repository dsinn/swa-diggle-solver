//! Clear a dungeon that is already in progress, and take the reward.
//!
//! A thin harness now. Everything it used to do lives in [`diggle_solver::fight`], because three
//! callers need it: this spike, the travel loop when a combat node blocks the way, and the shrine
//! sequence, which has to clear a shrine's fight before `Visit` appears.
//!
//! Resumes whatever `combatSaveData` holds, so pair it with `spike_enter_combat` (or a checkpoint
//! taken mid-fight) to choose which fight is being tested.
//!
//! **This drives the real mouse and keyboard.**

use diggle_solver::config::Config;
use diggle_solver::fight::Fight;
use diggle_solver::observe::feed::Feed;
use diggle_solver::observe::log::{Console, LogMirror};
use diggle_solver::search::Dictionary;
use diggle_solver::win::input::{Input, PostMessageInput};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const REPORT: &str = "spike-combat.md";
const FRAMES: &str = "spike-frames-live";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load(Path::new("config.toml"))?;
    std::fs::create_dir_all(FRAMES)?;
    diggle_solver::win::process::refuse_if_running("lovec.exe", &[])?;
    let save_dir = diggle_solver::game::savedir::locate(cfg.save_dir.clone(), true)?;

    let mut log = String::from("# Spike: the combat loop\n\n");
    let scorer = diggle_solver::score::Scorer::new(&cfg.game_dir)?;
    let dict = Dictionary::load(&cfg.game_dir)?;
    let letters = diggle_solver::letters::Weights::load(&cfg.game_dir)?;
    log.push_str(&format!("loaded {} words\n\n", dict.len()));

    let console = Console::take()?;
    let mirror = LogMirror::create(Path::new("spike-combat-raw.log"))?;
    let mut game = diggle_solver::game::launch::GameProcess::launch(&cfg, &console)?;
    let win = game.wait_for_window(Duration::from_secs(20))?;
    std::thread::sleep(Duration::from_secs(3));
    let keys = PostMessageInput::new(win);
    keys.focus();
    let mut feed = Feed::new(console, Some(mirror));

    if diggle_solver::act::click_when_ready(
        &win,
        &diggle_solver::act::CONTINUE,
        Duration::from_secs(30),
    )
    .is_err()
    {
        log.push_str("ABORT: no Continue\n");
        game.close(Duration::from_secs(15));
        std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
        println!("{log}");
        return Ok(());
    }
    log.push_str("clicked Continue\n\n");

    let fight = Fight {
        win: &win,
        dict: &dict,
        scorer: &scorer,
        letters: &letters,
        game_dir: cfg.game_dir.clone(),
        combat_path: save_dir.join("combatSaveData"),
        frames: Some(PathBuf::from(FRAMES)),
    };
    // Deliberately shorter than any harness timeout wrapping this: a spike killed from outside
    // never writes its report, and the stale one on disk then reads as the current result.
    let outcome = fight.run(&mut feed, &keys, &mut log, Instant::now() + Duration::from_secs(150))?;
    log.push_str(&format!("\n## Result\n\n{outcome:?}\n"));

    game.close(Duration::from_secs(15));
    std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
    println!("{log}");
    Ok(())
}
