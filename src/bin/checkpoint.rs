//! Save and restore sandbox checkpoints, so a once-per-island moment can be rehearsed.
//!
//! ```text
//! cargo run --bin checkpoint -- list
//! cargo run --bin checkpoint -- save pre-anomaly
//! cargo run --bin checkpoint -- restore pre-anomaly
//! cargo run --bin checkpoint -- clear          # wipe the profile, unlocks and all
//! ```
//!
//! Operates only on `%APPDATA%\LOVE\SternlyWordedAdventures` and refuses to run while the game is
//! open — see [`diggle_solver::game::checkpoint`] for why both are non-negotiable.

use diggle_solver::game::checkpoint;
use std::path::{Path, PathBuf};

const STORE: &str = "checkpoints";

/// Where `clear` puts the profile it is about to destroy.
const RESCUE: &str = "before-clear";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_else(|| "list".into());
    let store = PathBuf::from(STORE);
    // No config needed: the sandbox location is derived, and the guard rejects anything else.
    let save_dir = diggle_solver::game::savedir::locate(None, true)?;

    match cmd.as_str() {
        "list" => {
            let items = checkpoint::list(&store)?;
            if items.is_empty() {
                println!("no checkpoints in {STORE}/");
            }
            for (name, _) in items {
                println!("{name}\n    {}", checkpoint::describe(&store.join(&name)));
            }
            println!("\nlive save: {}", checkpoint::describe(&save_dir));
        }
        "save" => {
            let name = args.next().ok_or("usage: checkpoint save <name>")?;
            let dest = checkpoint::save(&save_dir, &store, &name)?;
            println!("saved {name:?} <- {}", save_dir.display());
            println!("    {}", checkpoint::describe(&dest));
        }
        "restore" => {
            let name = args.next().ok_or("usage: checkpoint restore <name>")?;
            // Destructive and unundoable, so say what is being replaced before doing it.
            println!("replacing: {}", checkpoint::describe(&save_dir));
            checkpoint::restore(&store, &name, &save_dir)?;
            println!("restored {name:?} -> {}", save_dir.display());
            println!("    {}", checkpoint::describe(&save_dir));
        }
        // Empties the sandbox save entirely — run, unlocks, history. See
        // [`checkpoint::clear`] for what each of those costs.
        //
        // **A rescue copy is taken first, and not offered as an option.** The whole profile is
        // about to go and there is no undo; `checkpoints/` is gitignored, so the copy costs a few
        // hundred kilobytes and nothing else. It always lands under the same name, so repeated
        // clears keep the most recent thing worth getting back rather than a pile of them.
        "clear" => {
            println!("clearing: {}", checkpoint::describe(&save_dir));
            match checkpoint::save(&save_dir, &store, RESCUE) {
                Ok(_) => println!("rescue copy: {STORE}/{RESCUE} (restore it to undo this)"),
                // The save dir being absent is the one failure that is not a problem: there is
                // nothing to rescue because there is nothing there.
                Err(e) if !save_dir.is_dir() => println!("nothing to rescue ({e})"),
                Err(e) => return Err(e.into()),
            }
            let removed = checkpoint::clear(
                &save_dir,
                std::path::Path::new(diggle_solver::navigate::MAP_CACHE),
            )?;
            if removed.is_empty() {
                println!("the sandbox save was already empty");
            } else {
                println!("removed from {}:", save_dir.display());
                for name in &removed {
                    println!("    {name}");
                }
            }
            println!("\nthe next launch starts a fresh profile: `Start`, not `Restart`");
        }
        other => {
            return Err(format!(
                "unknown command {other:?}; expected list, save <name>, restore <name>, or clear"
            )
            .into())
        }
    }
    let _: &Path = &store;
    Ok(())
}
