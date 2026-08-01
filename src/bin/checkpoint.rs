//! Save and restore sandbox checkpoints, so a once-per-island moment can be rehearsed.
//!
//! ```text
//! cargo run --bin checkpoint -- list
//! cargo run --bin checkpoint -- save pre-anomaly
//! cargo run --bin checkpoint -- restore pre-anomaly
//! ```
//!
//! Operates only on `%APPDATA%\LOVE\SternlyWordedAdventures` and refuses to run while the game is
//! open — see [`diggle_solver::game::checkpoint`] for why both are non-negotiable.

use diggle_solver::game::checkpoint;
use std::path::{Path, PathBuf};

const STORE: &str = "checkpoints";

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
        other => {
            return Err(format!(
                "unknown command {other:?}; expected list, save <name>, or restore <name>"
            )
            .into())
        }
    }
    let _: &Path = &store;
    Ok(())
}
