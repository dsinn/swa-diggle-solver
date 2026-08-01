//! Clearing a dungeon: every turn of it, then the reward, then back to the overworld.
//!
//! Extracted from `spike_combat`, which proved all of this live but kept it inside a `main` tangled
//! with its own launch, logging and reporting. Three callers need it now — the travel loop when a
//! combat node blocks the way, the shrine sequence (a shrine's fight has to be cleared before
//! `Visit` appears), and the spike itself.
//!
//! ## What it assumes, and what it refuses to assume
//!
//! It joins a fight already in progress: `combatSaveData` exists and the game is showing the board.
//! Starting one is `spike_enter_combat`'s job, because that is an overworld action.
//!
//! Everything else is checked rather than assumed, and each check is here because it failed once:
//!
//! - **The board is not ready just because `turnState` says `PlayerTurn`.** Resuming a save restores
//!   the state directly, skipping `PlayerPreTurn`'s `boardIsStatic()` gate, while tiles are still
//!   dropping. [`Board::wait_until_ready`] waits for occupancy *and* stillness — an empty board is
//!   perfectly static, which is how a click once landed on a slot with no tile in it.
//! - **`Finish` is clicked, verified, and retried.** Clicking once and hoping stalled a run for 45
//!   seconds; see [`Fight::finish`].
//! - **The reward is confirmed at the item, not at the button.** `Confirm` draws the chosen reward
//!   into its own background, so it has one appearance per item and no fixed fingerprint.
//! - **Nothing touches `combatSaveData` while the game is deleting it.** See [`Fight::take_reward`].

use crate::combat::Board;
use crate::game::save::{self, Table};
use crate::observe::feed::Feed;
use crate::search::{self, Dictionary, Goal, Modifiers};
use crate::win::input::{click_at, warp_cursor, Input, PostMessageInput, SC_SPACE, VK_SPACE};
use crate::win::window::{ButtonSpec, GameWindow};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// `Finish` / `Give up` share this slot; `Finish` is only safe in `WaitPhase`.
const FINISH: ButtonSpec =
    ButtonSpec { ss_x: 0.9, ss_y: 0.9, os_x: 0.0, os_y: 0.0, w: 300.0, h: 100.0 };
/// Somewhere with no hotspot, so the game's hover rendering is never inside a reading.
const NEUTRAL: (i32, i32) = (300, 300);
/// A dungeon that has not ended in this many player turns is not going to.
const MAX_TURNS: usize = 40;

/// How a fight ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Cleared, reward taken, postgame dismissed. `reward` is the item key we chose.
    Cleared { turns: usize, reward: Option<String> },
    /// The search found nothing playable on the board we were given.
    NoPlayableWord { turns: usize, board: String },
    /// A word the search liked could not actually be typed onto this board.
    NotRealizable { turns: usize, word: String },
    /// Tile selection did not land where it was aimed.
    SelectionFailed { turns: usize, detail: String },
    /// The board never filled and settled, so clicking would have been into a moving screen.
    BoardNeverSettled { turns: usize },
    /// `Finish` would not take.
    FinishRefused { turns: usize },
    /// The same `turnState` for long enough that nothing is happening.
    Stalled { turns: usize, state: String },
    /// Ran out of time or turns.
    Exhausted { turns: usize },
}

impl Outcome {
    pub fn cleared(&self) -> bool {
        matches!(self, Outcome::Cleared { .. })
    }
}

pub struct Fight<'a> {
    pub win: &'a GameWindow,
    pub dict: &'a Dictionary,
    pub scorer: &'a crate::score::Scorer,
    pub game_dir: PathBuf,
    pub combat_path: PathBuf,
    /// Where to drop diagnostic PNGs, if anywhere.
    pub frames: Option<PathBuf>,
}

impl Fight<'_> {
    /// Plays the fight through to the overworld.
    ///
    /// `log` collects a human-readable account; it is the only output besides the [`Outcome`].
    pub fn run(
        &self, feed: &mut Feed, keys: &PostMessageInput, log: &mut String, deadline: Instant,
    ) -> Result<Outcome, crate::Error> {
        let mut turns = 0usize;
        let mut last_state = String::new();
        let mut last_change = Instant::now();
        let mut finished = false;
        // Everything below asks "since this fight began", never "ever". A driver reuses one [`Feed`]
        // across fights, and the previous fight's `Item selection:` is still in the buffer — matching
        // it would send us to collect a reward that was taken minutes ago.
        let began = feed.mark();

        while Instant::now() < deadline && turns < MAX_TURNS {
            feed.pump();
            let Ok(cs) = save::load(&self.combat_path) else {
                // The file going away means the fight resolved; the reward screen is next.
                if feed.seen_since(began, "Item selection:") {
                    return self.take_reward(feed, keys, log, turns, deadline);
                }
                std::thread::sleep(Duration::from_millis(300));
                continue;
            };
            let state = cs.str_at("rpg.player.turnState").unwrap_or("").to_string();
            if state != last_state {
                last_state = state.clone();
                last_change = Instant::now();
            } else if last_change.elapsed() > Duration::from_secs(45) {
                log.push_str(&format!("STALLED in {state:?} for 45s\n"));
                self.shot("combat-stalled");
                return Ok(Outcome::Stalled { turns, state });
            }

            match state.as_str() {
                "WaitPhase" | "SmokebombWaitPhase" => {
                    if !finished {
                        match self.finish(feed, log, &state)? {
                            true => finished = true,
                            false => return Ok(Outcome::FinishRefused { turns }),
                        }
                    }
                    if feed.seen_since(began, "Item selection:") {
                        return self.take_reward(feed, keys, log, turns, deadline);
                    }
                    std::thread::sleep(Duration::from_millis(400));
                }
                "PlayerTurn" => {
                    turns += 1;
                    match self.play_turn(feed, keys, log, &cs, turns)? {
                        Some(bad) => return Ok(bad),
                        None => {}
                    }
                }
                _ => {
                    // PlayerPreTurn, EnemyTurn, EnemyDying: the game is animating, and PlayerTurn
                    // only begins once the board is static. Waiting on the save is enough.
                    std::thread::sleep(Duration::from_millis(300));
                }
            }

            if feed.seen_since(began, "Item selection:") {
                return self.take_reward(feed, keys, log, turns, deadline);
            }
        }
        Ok(Outcome::Exhausted { turns })
    }

    /// One player turn: read the board, choose a word, type it, submit, wait for the turn to move.
    fn play_turn(
        &self, feed: &mut Feed, keys: &PostMessageInput, log: &mut String, cs: &Table, turns: usize,
    ) -> Result<Option<Outcome>, crate::Error> {
        let tiles = tiles_of(cs);
        if tiles.is_empty() {
            std::thread::sleep(Duration::from_millis(300));
            return Ok(None);
        }
        let health = cs.int_at("rpg.enemy.health").unwrap_or(0);
        let armour = cs.int_at("rpg.enemy.armour").unwrap_or(0);
        let name = cs.str_at("rpg.enemy.name").unwrap_or("?").to_string();
        let (mods, geom) = Modifiers::from_save(&self.game_dir, cs, tiles.len())?;
        for p in &mods.problems {
            log.push_str(&format!("  WARNING {p}\n"));
        }

        // Overkill pays gold against a mimic or under a player curse, and the excess IS the reward,
        // so that fight wants the hardest hit rather than the quickest kill.
        let goal = Goal::for_enemy(&mods, health, armour);
        let out = search::search(self.dict, self.scorer, &tiles, &geom, &mods, goal, 8);
        let letters: String = tiles.iter().map(|t| t.letter.as_str()).collect();
        let Some(found) = out.choice().cloned() else {
            log.push_str(&format!("turn {turns}: nothing playable on {letters}\n"));
            return Ok(Some(Outcome::NoPlayableWord { turns, board: letters }));
        };
        let typist = crate::typist::Typist::new(&tiles, &geom);
        let Some(typed) = typist.type_word(&found.word) else {
            log.push_str(&format!("turn {turns}: {} is not realizable\n", found.word));
            return Ok(Some(Outcome::NotRealizable { turns, word: found.word }));
        };
        log.push_str(&format!(
            "turn {turns}: {name} {health}+{armour}hp, board {letters}\n  \
             play **{}** (scores {}, tiles {:?}, {} corners)\n",
            found.word, found.score, typed.tiles, typed.corners_used
        ));

        let board = Board::new(self.win, &geom)?;
        self.park();
        if !board.wait_until_ready(Duration::from_secs(20))? {
            log.push_str("  board never filled/settled -- not clicking into a moving board\n");
            return Ok(Some(Outcome::BoardNeverSettled { turns }));
        }
        if let Err(e) = board.select_word(&typed.tiles) {
            log.push_str(&format!("  SELECTION FAILED: {e}\n"));
            self.shot("combat-select-fail");
            return Ok(Some(Outcome::SelectionFailed { turns, detail: e.to_string() }));
        }
        self.park();
        std::thread::sleep(Duration::from_millis(150));
        keys.focus();
        std::thread::sleep(Duration::from_millis(150));
        keys.press_key(VK_SPACE, SC_SPACE)?;
        log.push_str("  selected and submitted\n");

        // Wait for the turn to move on: any other state, a changed board, or the reward screen.
        let mark = feed.mark();
        let until = Instant::now() + Duration::from_secs(20);
        while Instant::now() < until {
            std::thread::sleep(Duration::from_millis(250));
            feed.pump();
            if feed.seen_since(mark, "Item selection:") {
                break;
            }
            match save::load(&self.combat_path) {
                Ok(next) => {
                    let s = next.str_at("rpg.player.turnState").unwrap_or("");
                    let now: String = tiles_of(&next).iter().map(|t| t.letter.clone()).collect();
                    if s != "PlayerTurn" || now != letters {
                        break;
                    }
                }
                Err(_) => break, // the file went away: combat is over
            }
        }
        Ok(None)
    }

    /// Clicks `Finish`, confirms it took, and retries if it did not.
    ///
    /// Clicking once and hoping is what stalled a live run: a click that lands before the button is
    /// live is simply lost, and `activeIf` is the *live* state while ours comes from the save, so
    /// the two are not simultaneous. The console is checked before the save because it has no flush
    /// delay — and re-clicking this coordinate once the reward screen is up would land on the item
    /// row.
    fn finish(&self, feed: &mut Feed, log: &mut String, state: &str) -> Result<bool, crate::Error> {
        let _ = crate::observe::settle::wait_for_quiescence(self.win, 0.02, Duration::from_secs(6));
        let (fx, fy) = self.win.button_center(&FINISH)?;
        let (sx, sy) = self.win.client_to_screen(fx, fy)?;
        for attempt in 1..=3 {
            log.push_str(&format!("WaitPhase -> Finish at ({fx},{fy}), attempt {attempt}\n"));
            let mark = feed.mark();
            click_at(sx, sy)?;
            self.park();
            let until = Instant::now() + Duration::from_secs(4);
            while Instant::now() < until {
                std::thread::sleep(Duration::from_millis(250));
                feed.pump();
                if feed.seen_since(mark, "Item selection:") {
                    log.push_str("  Finish took effect\n");
                    return Ok(true);
                }
                if let Ok(now) = save::load(&self.combat_path) {
                    if now.str_at("rpg.player.turnState").unwrap_or("") != state {
                        log.push_str("  Finish took effect\n");
                        return Ok(true);
                    }
                } else {
                    log.push_str("  Finish took effect (combat save gone)\n");
                    return Ok(true);
                }
            }
            log.push_str("  no state change; the button was not live yet\n");
        }
        log.push_str("  **Finish did not take after 3 attempts**\n");
        Ok(false)
    }

    /// Picks a reward, confirms it, and clears the postgame.
    fn take_reward(
        &self, feed: &mut Feed, keys: &PostMessageInput, log: &mut String, turns: usize,
        deadline: Instant,
    ) -> Result<Outcome, crate::Error> {
        feed.pump();
        let offers = reward_offers(feed.lines());
        if offers.is_empty() {
            log.push_str("reward screen announced but no offers parsed\n");
            return Ok(Outcome::Cleared { turns, reward: None });
        }
        // Let the screen settle before measuring anything on it.
        let _ = crate::observe::settle::wait_for_quiescence(self.win, 0.02, Duration::from_secs(8));

        // MVP: any reward will do. Seeded from the clock so repeated runs do not always take the
        // same position, which would hide a click that only works on one of them.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as usize)
            .unwrap_or(0);
        let (key, ix, iy) = offers[nanos % offers.len()].clone();
        log.push_str(&format!("picking **{key}** at ({ix},{iy})\n"));

        // Verify at the ITEM, not at Confirm: Confirm draws the chosen reward into its own
        // background, so it has one appearance per item and no fixed fingerprint. Park before
        // measuring or the hover highlight alone clears the threshold.
        let probe = (ix - 90, iy - 90, 180, 180);
        let before = crate::win::capture::capture_client_rect(self.win, probe.0, probe.1, probe.2, probe.3)
            .map(|f| crate::combat::luma(&f, probe.2 / 2, probe.3 / 2, 60))?;
        let (sx, sy) = self.win.client_to_screen(ix, iy)?;
        click_at(sx, sy)?;
        self.park();
        std::thread::sleep(Duration::from_millis(600));
        let after = crate::win::capture::capture_client_rect(self.win, probe.0, probe.1, probe.2, probe.3)
            .map(|f| crate::combat::luma(&f, probe.2 / 2, probe.3 / 2, 60))?;
        if (after - before).abs() <= crate::combat::CHANGED {
            log.push_str(&format!("  item luma {before:.1} -> {after:.1}: NOT selected\n"));
            return Ok(Outcome::Cleared { turns, reward: None });
        }
        log.push_str(&format!("  item luma {before:.1} -> {after:.1}, selected\n"));

        // Confirm with Space: the button declares `userFunctionName = 'affirmative'` and
        // `activeIf = selection`, so it does nothing until something IS selected -- the guard is the
        // game's own.
        let mark = feed.mark();
        keys.focus();
        std::thread::sleep(Duration::from_millis(300));
        keys.press_key(VK_SPACE, SC_SPACE)?;

        // `viewPostgameAndReturn` grants the item, opens the postgame, and asserts on deleting
        // `combatSaveData`. Watch the CONSOLE, and do not touch that file while it is going: polling
        // it across this window made the delete fail and crashed the game.
        let until = deadline.min(Instant::now() + Duration::from_secs(20));
        let mut postgame = false;
        while Instant::now() < until && !postgame {
            std::thread::sleep(Duration::from_millis(250));
            feed.pump();
            postgame = feed.seen_since(mark, "Postgame screen:");
        }
        log.push_str(&format!("  postgame reached: {postgame}\n"));
        if postgame {
            // Postgame's Continue is `affirmative` -> `goBack()`, guarded by `activeIf = backMode`.
            keys.focus();
            std::thread::sleep(Duration::from_millis(300));
            keys.press_key(VK_SPACE, SC_SPACE)?;
            std::thread::sleep(Duration::from_secs(2));
            feed.pump();
        }
        Ok(Outcome::Cleared { turns, reward: Some(key) })
    }

    fn park(&self) {
        if let Ok((x, y)) = self.win.client_to_screen(NEUTRAL.0, NEUTRAL.1) {
            let _ = warp_cursor(x, y);
        }
    }

    fn shot(&self, name: &str) {
        if let (Some(dir), Ok(f)) = (self.frames.as_ref(), crate::win::capture::capture_window(self.win))
        {
            let _ = f.write_png(&dir.join(format!("{name}.png")));
        }
    }
}

/// Tiles as the combat save records them.
/// The board from `combatSaveData`.
///
/// Shares [`crate::observe::board::tiles_from`] with the console reader, because the two formats are
/// the same and the copy here did not know it. It did `filter_map(as_str)`, which silently dropped
/// every structured tile -- `{ "W", { bg = "wood" } }` -- shortening the board and shifting every
/// index after it.
///
/// A live fight showed the whole failure in one line. Sixteen tiles became fifteen, the solver chose
/// indices [4, 10, 2, 6, 12, 13, 8, 1, 14, 0] meaning NATURALITY on its shortened board, and the
/// game read those same indices against the real board and answered
/// `YDIWYRIYAW  not recognised`. Nothing was submitted, the board never changed, and the fight
/// stalled replaying the same word.
///
/// The dropped quality mattered too: a wood or gold tile scored as an ordinary letter, so even a
/// board that happened to parse to the right length would have been mis-scored.
fn tiles_of(save: &Table) -> Vec<crate::observe::board::Tile> {
    save.table_at("tileboard")
        .map(crate::observe::board::tiles_from)
        .unwrap_or_default()
}

/// The rewards on offer, from the `Item selection:` block.
///
/// `ui/itemselection.lua:419-430` prints each item with its name and **screen coordinates**, so the
/// row never has to be located visually.
fn reward_offers(lines: &[String]) -> Vec<(String, i32, i32)> {
    let mut out = Vec::new();
    let start = lines.iter().rposition(|l| l.contains("Item selection:"));
    let Some(start) = start else { return out };
    for line in lines.iter().skip(start + 1) {
        let parts: Vec<&str> = line.split('\t').map(|p| p.trim()).filter(|p| !p.is_empty()).collect();
        if parts.len() < 4 {
            break; // the block has ended
        }
        let (Ok(x), Ok(y)) = (parts[parts.len() - 2].parse(), parts[parts.len() - 1].parse()) else {
            break;
        };
        out.push((parts[0].to_string(), x, y));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_reward_row_with_its_coordinates() {
        // Real output from a live run.
        let lines: Vec<String> = "Item selection:\n\
             \tphoenixFeather\tPhoenix feather\t480\t540\n\
             \tarmourLeatherGloves\tBlacksmith gloves\t960\t540\n\
             \tgildedTetraTeabag\tGilded tetra teabag\t1440\t540\n\
             ach_something\talready achieved"
            .lines()
            .map(|s| s.to_string())
            .collect();
        let offers = reward_offers(&lines);
        assert_eq!(offers.len(), 3);
        assert_eq!(offers[0], ("phoenixFeather".into(), 480, 540));
        assert_eq!(offers[2], ("gildedTetraTeabag".into(), 1440, 540));
    }

    #[test]
    fn takes_the_latest_block_not_the_first() {
        // A second fight must not pick up the previous fight's offers.
        let lines: Vec<String> = "Item selection:\n\
             \toldThing\tOld\t480\t540\n\
             noise\n\
             Item selection:\n\
             \tnewThing\tNew\t960\t540"
            .lines()
            .map(|s| s.to_string())
            .collect();
        let offers = reward_offers(&lines);
        assert_eq!(offers, vec![("newThing".to_string(), 960, 540)]);
    }

    #[test]
    fn an_announcement_with_no_rows_yields_nothing() {
        let lines = vec!["Item selection:".to_string()];
        assert!(reward_offers(&lines).is_empty());
    }
}

#[cfg(test)]
mod tileboard_tests {
    use super::*;
    use crate::game::save::parse;

    /// The save form of the board that stalled a village inn fight.
    ///
    /// The first tile is structured; the reader used to drop it, shortening a 16-tile board to 15
    /// and shifting every index after it by one.
    #[test]
    fn a_structured_tile_in_the_save_is_kept_with_its_material() {
        let save = parse(
            "return {\n\
             \x20 tileboard = {\n\
             \x20   { \"W\", { bg = \"wood\" } },\n\
             \x20   \"Y\", \"I\", \"T\",\n\
             \x20 },\n\
             }\n",
        )
        .expect("parses");
        let tiles = tiles_of(&save);
        assert_eq!(tiles.len(), 4, "the wood tile counts toward the board");
        assert_eq!(tiles[0].letter, "W");
        assert_eq!(tiles[0].quality.material.as_deref(), Some("wood"));
        // Index 1 must still be Y. When the wood tile was dropped this was I, and every later index
        // was wrong by one -- which is how NATURALITY was submitted as YDIWYRIYAW.
        assert_eq!(tiles[1].letter, "Y");
    }
}
