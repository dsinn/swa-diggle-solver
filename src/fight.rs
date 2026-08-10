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
//! - **The item screen is not ours.** `ui.itemselection` is built from seven places and only one is
//!   a fight, so picking an item lives in [`crate::itemchoice`]; what stays here is the postgame it
//!   opens into. See [`crate::itemchoice`] for why identification of the items is console-only.
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

/// Does this console line announce the avoidable-murder dialog?
///
/// Two substrings, not one. `Feed::seen_since`'s own doc warns that a bare substring test is wrong
/// for wordy announcements, because the console also carries item names, enemy names and event
/// prose — and this game's prose is exactly the sort that could contain "murder" innocently. The
/// header pins it to a dialog and the phrase pins it to *this* dialog.
///
/// Not the whole message, which would be brittle: `wordboard.lua:503` is the only source of it
/// today, but matching a full sentence makes a localisation or a reworded prompt look like no
/// dialog at all — and this check failing silently is what a stuck run looks like.
fn announces_murder_dialog(line: &str) -> bool {
    line.contains("Okay dialogue:") && line.contains("avoidable murder")
}

/// Is there a weaker word left to aim for, or is this kill unavoidable?
///
/// The condition for confirming an avoidable-murder warning rather than backing off again. **The
/// question is whether a lower band exists, not how many times we have been refused.** A count is
/// wrong at both ends: starting from a ceiling of 3 the band reaches its floor on the first refusal
/// and any further attempts replay the same word, while from a ceiling of 12 a count of six would
/// confirm the kill with genuinely weaker words still unexplored.
///
/// A goal that is not a scare is already trying to kill, so a warning on one is not a disagreement —
/// it is the game asking whether we meant it, and we did.
///
/// **The dev's call, recorded as an accepted risk:** at minimum damage, take the murder. Our model
/// carries no status, so a word inside the direct-damage band can still kill through the bleed or
/// toxin it applies, and no amount of aiming lower fixes a model that is missing a term. The cost is
/// a `Murder` flag; the alternative is replaying one word until `MAX_TURNS` — `turns += 1` runs on
/// this path, and a live run reached thirty attempts before it was stopped by hand.
/// `Goal::FrugalKill` reaches the `_` arm and should: it is a kill we chose to make, so a warning
/// that the kill is avoidable is not news, and there is no band to lower — its ceiling protects a
/// rest charge, not a life. Backing off it would trade the fight for a saving.
fn nothing_left_to_lower(goal: Goal) -> bool {
    match goal {
        Goal::Scare { need, below } => {
            need <= crate::search::MIN_MEANINGFUL_DAMAGE
                && below <= crate::search::MIN_MEANINGFUL_DAMAGE + 1
        }
        _ => true,
    }
}

/// How many times to press Backspace at the avoidable-murder modal before giving up on it.
///
/// More than one because a keystroke can be lost to focus, and few because each attempt already
/// waits two seconds for the plaque to fade out — three is enough to distinguish "the press was
/// dropped" from "this key does not dismiss this dialog", which is the only distinction worth
/// spending time on. If all three fail the word is still on the board, and the next turn reports
/// `BoardNeverSettled` rather than this inventing an outcome for it.
const MURDER_CANCEL_TRIES: usize = 3;

/// How long `combatSaveData` must stay unreadable before the fight is called over.
///
/// A failed read is **not** proof that it ended. Nothing deletes this file per fight — the only two
/// `love.filesystem.remove` calls for it are the abandoned-run eulogy (`overworld.lua:968`) and the
/// postgame (`:1154`), both of which end the whole run. What actually happens mid-fight is that the
/// game rewrites it (`rpg.lua:43`), and a read landing inside that window fails. Returning on the
/// first failure would end a live fight; this is the window a rewrite has to complete in.
///
/// It doubles as the grace period for `Item selection:` to print, since the fight resolving and the
/// reward screen being built are not the same instant.
const SAVE_SETTLE: Duration = Duration::from_secs(3);

/// Consecutive words that may fail to reach the board before the fight is called off.
///
/// Consecutive, not cumulative: a stray is a momentary misreading, so one every few turns is noise
/// and three in a row is a board we have stopped understanding. Reset by any word that lands.
const SELECT_ATTEMPTS: usize = 3;

/// What to do about a word that could not be placed.
#[derive(Debug, PartialEq, Eq)]
enum Recovery {
    /// Clear board, attempts left: plan again.
    PlayOn,
    /// Out of attempts, or a board we could not clear.
    Stop,
}

/// Whether a failed word ends the fight.
///
/// Split out from the turn so the judgement is testable without a game window — the code around it
/// is all input and captures, and this is the only part with a decision in it.
///
/// **A dirty board always stops**, however many attempts remain. Everything downstream assumes a
/// word is built from nothing; retrying onto a half-finished selection would submit a word that was
/// never scored, which is worse than the failure it is trying to recover from.
fn recover_from(consecutive_failures: usize, board_is_clean: bool) -> Recovery {
    if board_is_clean && consecutive_failures < SELECT_ATTEMPTS {
        Recovery::PlayOn
    } else {
        Recovery::Stop
    }
}

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
    /// Won, but no reward screen was offered.
    ///
    /// Whatever screen the game left up is deliberately **not** dismissed here — see
    /// [`Fight::run`]. The caller's own loop presses it, the same way it presses a lore screen.
    ClearedWithoutReward { turns: usize },
    /// The character died. The run is over and nothing after this means anything.
    ///
    /// Read from the console, never from the screen, and that is the whole point. The one existing
    /// death check is `act::Screen::Dead` via `slot_is_eulogise` — a template read on the affirmative
    /// slot at (1603,922). That slot is the worst-hit region under the hurt vignette (+58 measured),
    /// and `rpgview.lua:1938` sets the vignette to its **maximum** at exactly zero health:
    ///
    /// ```lua
    /// if healthN==0 then retinaPainValTarget = 1.5
    /// ```
    ///
    /// So the pixel check is least trustworthy precisely when it is being asked the one question it
    /// exists for. `game over` arrives as plain console text and no shader can touch it.
    ///
    /// A live run died at `l50` and reported `NoPlayableWord { turns: 9, board: "X" }` — true of the
    /// board it was handed, and completely beside the point. The board had collapsed to a single
    /// tile because the player was dying (`columns = {0,0,1,0,0}` in the game's own dump).
    Died { turns: usize },
}

impl Outcome {
    pub fn cleared(&self) -> bool {
        matches!(self, Outcome::Cleared { .. } | Outcome::ClearedWithoutReward { .. })
    }

    /// Is the save unusable from here on?
    ///
    /// Separate from "did we win", because every caller that branches on [`Outcome::cleared`] has a
    /// not-cleared path that assumes another attempt is worth making. After death it is not.
    pub fn fatal(&self) -> bool {
        matches!(self, Outcome::Died { .. })
    }
}

/// The console line the game prints when the run is over.
///
/// `rpg.lua:7-9` — a bare `print("game over")`, the first statement of `rpg.gameOver`, before it
/// walks its objects. Unconditional, and it precedes any screen change, so it arrives before there
/// is anything to recognise by sight.
///
/// Matched as a whole trimmed line rather than a substring, so an enemy or item whose name happens
/// to contain the words cannot raise a false death.
pub const GAME_OVER: &str = "game over";

pub struct Fight<'a> {
    pub win: &'a GameWindow,
    pub dict: &'a Dictionary,
    pub scorer: &'a crate::score::Scorer,
    /// The game's letter frequencies, for the target distribution a played word is judged against.
    ///
    /// Loaded once and held, like [`Fight::dict`] and [`Fight::scorer`]: it is a parse of a file in
    /// the game's source, and re-reading it per turn would be the same waste.
    pub letters: &'a crate::letters::Weights,
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
        // The turn we last saw progress on. Paired with `last_state` so the stall timer resets on
        // either signal — see where it is compared.
        let mut last_turn = usize::MAX;
        // How far below our own ceiling to aim, after the game has called an attempt murder. One
        // point per refusal; see where it is applied.
        let mut murder_backoff = 0i64;
        // The enemy health and armour the backoff above was measured against. Impossible as a real
        // reading, so the first turn always records rather than compares.
        let mut last_seen_at = (i64::MIN, i64::MIN);
        // Words in a row that could not be placed. Zeroed by any word that lands, so this counts a
        // board we have stopped understanding rather than a fight's worth of occasional strays.
        let mut select_failures = 0usize;
        let mut last_change = Instant::now();
        let mut finished = false;
        // When `combatSaveData` first became unreadable, cleared on every successful read.
        let mut unreadable_since: Option<Instant> = None;
        let mut peak_health: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        // Everything below asks "since this fight began", never "ever". A driver reuses one [`Feed`]
        // across fights, and the previous fight's `Item selection:` is still in the buffer — matching
        // it would send us to collect a reward that was taken minutes ago.
        let began = feed.mark();

        while Instant::now() < deadline && turns < MAX_TURNS {
            feed.pump();
            // Before anything is read from the board, because after death the board is still there
            // and still parses — it just no longer means anything. A live run played three more
            // turns past this point against a board the game was collapsing under it, and reported
            // `NoPlayableWord { board: "X" }` as though the word search had been the problem.
            if feed.seen_line_since(began, GAME_OVER) {
                log.push_str(&format!("  **`{GAME_OVER}` on the console after {turns} turns**\n"));
                return Ok(Outcome::Died { turns });
            }
            let cs = match save::load(&self.combat_path) {
                Ok(cs) => {
                    unreadable_since = None;
                    cs
                }
                Err(_) => {
                    // A reward screen is the common next thing, but not the only one.
                    if feed.seen_since(began, "Item selection:") {
                        return self.take_reward(feed, keys, log, turns, deadline);
                    }
                    // Not proof of anything yet: see `SAVE_SETTLE`. Give the rewrite time to finish
                    // and the reward screen time to announce itself.
                    let since = *unreadable_since.get_or_insert(Instant::now());
                    if since.elapsed() < SAVE_SETTLE {
                        std::thread::sleep(Duration::from_millis(150));
                        continue;
                    }
                    log.push_str(&format!(
                        "  fight over after {turns} turns; no reward screen offered\n"
                    ));
                    // A rewardless win still raises the postgame, and it has to be cleared here.
                    //
                    // This used to be left to the caller's loop, on the reasoning that a leftover
                    // screen is the loop's business, the same as a lore screen. That reasoning was
                    // wrong about one thing: the loop finds a screen by reading the *arrow*
                    // artwork, and the postgame's `Continue` is a `default` text button. It scores
                    // 0.20 there and is simply never seen — so the run walked off looking for a map
                    // with the stats screen still up, and reported a subworld navigation failure.
                    self.clear_postgame(feed, keys, log, began, deadline);
                    return Ok(Outcome::ClearedWithoutReward { turns });
                }
            };
            // ## Progress is the turn counter, not the state label
            //
            // This watched `turnState` alone and called 45 s without a change a stall. But
            // `PlayerTurn` is precisely the state the game sits in *while waiting for us*, so the
            // label cannot tell "nothing is happening" from "it is our move" — and a label is
            // neither memory nor a monotone measure, which is the same mistake the navigation guards
            // made (`docs/superpowers/notes/navigation-loops.md`).
            //
            // Live, 2026-08-09, in a level 5 crypt: nine turns won cleanly, `Clavicular` down from
            // 81 to 11, `CAPITATES` played for 40 and accepted — the HUD read `Turn: 10` and the
            // word's definition was on screen. The board was down to seven tiles, the game printed
            // no `Player turn 10 start` (`tileboard.lua:1285` logs only once the turn-start
            // fall-and-respawn completes), and this reported `Stalled`. The fight was going fine;
            // the *report* was wrong, and it named the game as the culprit.
            //
            // **Do not read more into that board than was measured.** Seven tiles were on screen and
            // no turn-10 dump arrived within 45 s. Whether the board could not fill, or simply had
            // not yet, is unknown — we stopped looking. The game does have an answer if it turns out
            // to matter: `Refresh`, "clear the tile board of all selectable tiles and refill"
            // (`rpg.lua:407-412`), which costs health and has a separate gold-tile variant. It is
            // not implemented, and should stay that way until a run with this timer fixed gets
            // genuinely stuck — at which point there will be evidence about what the board does over
            // a longer wait, which is the thing nobody has yet.
            //
            // So the timer resets when either the state changes **or a turn goes by**. Turns only
            // ever increase, so this cannot be gamed by a state that flaps.
            let state = cs.str_at("rpg.player.turnState").unwrap_or("").to_string();
            if state != last_state || turns != last_turn {
                last_state = state.clone();
                last_turn = turns;
                last_change = Instant::now();
            } else if last_change.elapsed() > Duration::from_secs(45) {
                log.push_str(&format!("STALLED in {state:?} at turn {turns} for 45s\n"));
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
                    match self.play_turn(
                        feed,
                        keys,
                        log,
                        &cs,
                        turns,
                        &mut peak_health,
                        &mut murder_backoff,
                        &mut last_seen_at,
                        &mut select_failures,
                    )? {
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
    #[allow(clippy::too_many_arguments)]
    fn play_turn(
        &self, feed: &mut Feed, keys: &PostMessageInput, log: &mut String, cs: &Table, turns: usize,
        peak_health: &mut std::collections::HashMap<String, i64>, murder_backoff: &mut i64,
        last_seen_at: &mut (i64, i64), select_failures: &mut usize,
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

        // The enemy's maximum health is not in the save (`rpgview.lua:2513-2522` lists what is), and
        // the flee thresholds are all defined against it. The highest health we have seen this
        // enemy at is that maximum, because we start fights from the pregame with a fresh enemy and
        // health only falls within a fight. Keyed by name, so the next enemy in a queue starts its
        // own record rather than inheriting the last one's.
        let peak = peak_health.entry(name.clone()).or_insert(health);
        *peak = (*peak).max(health);
        let peak = *peak;

        // Overkill pays gold against a mimic or under a player curse, and the excess IS the reward,
        // so that fight wants the hardest hit rather than the quickest kill. Otherwise, an enemy
        // that can be frightened is frightened rather than killed.
        let player = player_state(cs, &mods);
        // **Health after the status already on it ticks**, not the health the save reports.
        //
        // The question every goal is really asking is whether the enemy gets to act, because most
        // enemy turns hurt us. The game asks the same question the same way: `enemyCanHit`
        // (`rpgview.lua:1046-1052`) is gated on the post-status health that also decides
        // `attackEstimatedToCauseEnemyDeath` (`:1045`). An enemy on 5 that is bleeding is, for our
        // purposes, an enemy on 4.
        //
        // This is also what a scare ceiling has to respect: a word inside the direct-damage band
        // still kills if the status finishes the job, which is how a live run collected thirty
        // avoidable-murder refusals for words its own arithmetic called safe.
        let deciding_health = (health - mods.enemy_status_tick).max(0);
        if mods.enemy_status_tick != 0 {
            log.push_str(&format!(
                "  {name} loses {} to status before acting; aiming against {deciding_health}hp
",
                mods.enemy_status_tick
            ));
        }
        let mut goal =
            Goal::for_enemy(&mods, deciding_health, armour, Some(peak), player.as_ref());
        // **Aim lower than last time if the game called the last attempt murder.**
        //
        // Our arithmetic said that hit was survivable and the game's estimate said it kills, so the
        // model is wrong by at least the amount we have backed off. Re-searching with the same
        // ceiling would choose the same word and be refused again, forever; each refusal takes one
        // more point off the top. `need` moves with it, or a band one point wide inverts.
        //
        // ## The backoff is discarded when the enemy's state changes
        //
        // It is a correction to a disagreement measured at *one* health and armour, not a standing
        // opinion about the enemy. A live run carried a backoff of 2 from a 5hp reading down to a
        // 2hp one, where the honest goal was already `Scare { 1, 3 }` — a single point of damage,
        // absorbed by armour, with the enemy at its flee threshold. The stale backoff dragged that
        // to `Scare { 0, 1 }`, which nothing can satisfy.
        if *murder_backoff > 0 && (health, armour) != *last_seen_at {
            log.push_str(&format!(
                "  {name} is now {health}+{armour}hp; dropping the murder backoff of {murder_backoff}\n"
            ));
            *murder_backoff = 0;
        }
        *last_seen_at = (health, armour);
        if *murder_backoff > 0 {
            if let Goal::Scare { need, below } = goal {
                // Never below a band of exactly one point of damage.
                //
                // A ceiling of 1 with a floor of 0 admits only a zero-scoring word — which is both
                // a wasted turn ([`crate::search::MIN_MEANINGFUL_DAMAGE`]) and, on a board with no
                // such word, an empty band that sends the search to its fallback. That is the whole
                // reason this loop ran thirty times.
                //
                // So the ceiling stops at 2 and the floor at 1: the narrowest honest band is "deal
                // exactly 1". If even that is refused as murder, no further lowering can help and
                // the backoff is the wrong tool for whatever is happening.
                let dropped = (below - *murder_backoff).max(crate::search::MIN_MEANINGFUL_DAMAGE + 1);
                goal = Goal::Scare {
                    need: need.min(dropped - 1).max(crate::search::MIN_MEANINGFUL_DAMAGE),
                    below: dropped,
                };
                log.push_str(&format!(
                    "  aiming {} lower after a murder warning: {goal:?}
", murder_backoff
                ));
            }
        }
        if let Goal::Scare { need, below } = goal {
            // Inclusive in the log, half-open in the code. `race_for_band` filters
            // `score >= need && score < below` (`search.rs:838`), so `below` is a ceiling the word
            // must stay under — but "aiming for 4..6" reads in English as "6 is allowed", and it
            // sent a reader hunting for a bug in the aim when the aim was correct and only this
            // sentence was wrong.
            log.push_str(&format!(
                "  {name} can be scared off ({:?}); aiming for {need}..={} damage, not a kill\n",
                mods.nerve,
                below - 1
            ));
        }
        // Its own line, because for one run this band borrowed the one above and the log said an
        // enemy with no nerve at all `can be scared off (None)` while we tried to kill it.
        if let Goal::FrugalKill { need, below } = goal {
            log.push_str(&format!(
                "  killing {name} frugally: {need}..={} damage keeps the rest charge\n",
                below - 1
            ));
        }
        // The enemy's statuses, whenever it has any.
        //
        // Recorded because a live murder warning could not be explained without them. The game's
        // estimate is of death *possibly by status* (`rpgview.lua:915`), and
        // `getStatusEffectHealthDeltasFor` (`:1027`) folds toxin, burn and bleed into the health it
        // predicts — none of which our arithmetic models. Without this line there is no way to tell
        // "the game scored the word higher than we did" from "the enemy was already dying".
        let statuses = crate::lexica::Lexica::statuses_from_save(cs);
        if !statuses.is_empty() {
            let mut shown: Vec<String> =
                statuses.iter().map(|(k, v)| format!("{k}={v}")).collect();
            shown.sort();
            log.push_str(&format!("  {name} statuses: {}\n", shown.join(" ")));
        }
        // Built per turn rather than per fight, because the board's tile count is what the target is
        // scaled to and a board can lose tiles mid-fight — the run that died at `l50` watched its
        // board collapse from 16 tiles to one. A target built once at 16 would have been describing a
        // board that no longer existed.
        // Room for more armour, which is what decides whether `onWoodKillGainArmour` is worth
        // constraining a word for. The cap is `maxHealth / (maxArmourHalved and 2 or 1)`
        // (`overworld.lua:18`), read from the same `combatSaveData` the gear flags come from.
        //
        // Unknown counts as **room available**, the opposite of how unknown health is treated when
        // deciding whether to fight. The asymmetry is deliberate: guessing wrong about health can
        // end the save, while guessing wrong here costs a slightly worse word.
        let armour_room = {
            let now = cs.int_at("rpg.player.armour");
            let max = cs.int_at("rpg.player.maxHealth").map(|m| {
                if cs.path("rpg.player.gearFlags.maxArmourHalved").is_some() { m / 2 } else { m }
            });
            match (now, max) {
                (Some(a), Some(m)) => a < m,
                _ => true,
            }
        };
        let picking = crate::pick::Context {
            target: self.letters.target(tiles.len()),
            prefs: crate::pick::Preferences::from_flags(
                |f| cs.path(&format!("rpg.player.gearFlags.{f}")).is_some(),
                // Any missing health counts as injured: the buckler heals 4, so at 19/20 the payout
                // is real and merely smaller than its cap. No threshold is invented here.
                player.as_ref().map(|p| p.vitals.missing() > 0).unwrap_or(false),
                // Bleeding cancels the heal (`rpgview.lua:1080`), and with it the reason to chase a
                // wood-only word. Read from the same `PlayerState` the rested goals already use, so
                // there is one reading of the status per turn rather than two that could disagree.
                player.as_ref().map(|p| p.bleeding).unwrap_or(false),
                armour_room,
            ),
        };
        let out = search::search(self.dict, self.scorer, &tiles, &geom, &mods, goal, &picking, 8);
        let letters: String = tiles.iter().map(|t| t.letter.as_str()).collect();
        let Some(found) = out.choice_for(goal).cloned() else {
            log.push_str(&format!("turn {turns}: nothing playable on {letters}\n"));
            return Ok(Some(Outcome::NoPlayableWord { turns, board: letters }));
        };
        let typist = crate::typist::Typist::new(&tiles, &geom);
        let Some(typed) = typist.type_word(&found.word) else {
            log.push_str(&format!("turn {turns}: {} is not realizable\n", found.word));
            return Ok(Some(Outcome::NotRealizable { turns, word: found.word }));
        };
        let steps: Vec<(usize, Option<char>)> = typed.steps().collect();
        log.push_str(&format!(
            "turn {turns}: {name} {health}+{armour}hp, board {letters}\n  \
             play **{}** (scores {}, tiles {:?}, {} corners{})\n",
            found.word,
            found.score,
            typed.tiles,
            typed.corners_used,
            if typed.uses_a_wildcard() {
                let w: Vec<String> = typed
                    .steps()
                    .filter_map(|(i, c)| c.map(|c| format!("{i}->{c}")))
                    .collect();
                format!(", wildcards {}", w.join(" "))
            } else {
                String::new()
            }
        ));

        let board = Board::new(self.win, &geom)?;
        self.park();
        if !board.wait_until_ready(Duration::from_secs(20))? {
            log.push_str("  board never filled/settled -- not clicking into a moving board\n");
            return Ok(Some(Outcome::BoardNeverSettled { turns }));
        }
        if let Err(e) = board.select_word(&steps) {
            *select_failures += 1;
            log.push_str(&format!(
                "  SELECTION FAILED ({} of {SELECT_ATTEMPTS}): {e}\n",
                *select_failures
            ));
            self.shot("combat-select-fail");
            return Ok(match recover_from(*select_failures, e.board_is_clean) {
                Recovery::PlayOn => {
                    // Back to the outer loop, which re-reads the save and plans again. Re-planning
                    // rather than re-typing the same word: the board may genuinely have moved under
                    // us, and a fresh search costs nothing against a turn we have already paid for.
                    log.push_str("  board is clear -- planning again\n");
                    None
                }
                Recovery::Stop => {
                    Some(Outcome::SelectionFailed { turns, detail: e.to_string() })
                }
            });
        }
        *select_failures = 0;
        self.park();
        std::thread::sleep(Duration::from_millis(150));
        keys.focus();
        std::thread::sleep(Duration::from_millis(150));
        keys.press_key(VK_SPACE, SC_SPACE)?;
        log.push_str("  selected and submitted\n");

        // Wait for the turn to move on: any other state, a changed board, or the reward screen.
        let mark = feed.mark();
        let until = Instant::now() + Duration::from_secs(20);
        // What this loop *saw*, not just what it concluded.
        //
        // A run stalled here for its full twenty seconds and reported
        // `Stalled { turns: 10, state: "PlayerTurn" }` — a verdict with none of the evidence behind
        // it. Ten observations were made and all ten discarded, so afterwards there was no way to
        // tell "found no word" from "submitted and nothing moved" from "the save stopped being
        // readable", and the diagnosis came down to guesswork against a single screenshot.
        //
        // Kept as a running summary rather than a line per poll: eighty near-identical lines is how
        // the affirmative slot came to fill a report. The health and status go in because the run
        // that prompted this was on 1 health with 3 stacks of toxin, which is a very different
        // situation from a board that simply will not move.
        let mut polls = 0usize;
        let mut read_errors = 0usize;
        let mut last = String::new();
        while Instant::now() < until {
            std::thread::sleep(Duration::from_millis(250));
            feed.pump();
            if feed.seen_since(mark, "Item selection:") {
                break;
            }
            // ## The game may refuse the submission as an avoidable murder
            //
            // `wordboard.startAttack` (`wordboard.lua:499-508`) opens a modal instead of attacking
            // when the enemy is about to die *and* carries `fear`, `terror` or `caution`. It firing
            // means our damage model and the game's estimate have disagreed — the game's is of
            // **death**, "possibly by status" (`rpgview.lua:915`), so an effect we do not model can
            // finish an enemy our arithmetic left alive. Back out rather than confirm: the dev's
            // rule is cancel, clear the word, play something weaker.
            //
            // ## Why this is read off the console and not the screen
            //
            // The first version looked for the `Cancel` plaque with a single `act::locate` straight
            // after the Space press, on the reasoning that the modal is drawn in the same frame the
            // attack is refused. That reasoning was wrong and it cost a live run.
            // `setActiveMode` (`main.lua:191-195`) starts a cross-fade of
            // `userConfig.interface.transitionDuration`, **0.625s** by default, so a look taken
            // immediately lands mid-fade on a half-transparent plaque and scores under the
            // threshold. Measured afterwards against the frame the run stopped on, the same
            // template scores **1.0000** at the same origin: right artwork, right coordinates,
            // wrong moment.
            //
            // Polling the pixels instead would have worked and is still the wrong shape. This check
            // is speculative — no dialog is the overwhelmingly common case — so any timeout long
            // enough to outlast the fade would be paid on every turn of every fight. The console
            // line is free: this loop is already pumping the feed.
            //
            // The line is emitted from the dialog's `onActive` (`ui/twobuttondialogue.lua:27-33`),
            // which `StartTransition` calls at `main.lua:163-165` — nine lines before it installs
            // the mode's `userFunctions` at `:170`, with no yield in between. Lua is single
            // threaded and LÖVE pumps events at the top of a frame, not re-entrantly inside a
            // `captureScreenshot` callback, so nothing can be dispatched in that gap. Our reading
            // of the line is later still. For once, the announcement *is* the readiness signal —
            // not because console lines can be trusted in general, but because this one shares an
            // uninterruptible block with the handler it announces.
            if feed.since(mark).iter().any(|l| announces_murder_dialog(l)) {
                log.push_str("  the game says this would be avoidable murder — backing out\n");
                self.shot("murder-warning");
                // Already at minimum damage, so backing off again would submit the same word. See
                // `nothing_left_to_lower`.
                if nothing_left_to_lower(goal) {
                    log.push_str(&format!(
                        "  {goal:?} has nothing weaker to aim for — taking the kill\n"
                    ));
                    self.accept_murder(keys, log)?;
                    return Ok(None);
                }
                if !self.back_out_of_murder(keys, log, steps.len())? {
                    // Left as it is rather than dressed up as an outcome. The word is still on the
                    // board and the modal may still be over it, so the next turn's
                    // `wait_until_ready` will report `BoardNeverSettled` — which is exactly what
                    // this situation is, and what it reported before the handler existed.
                    return Ok(None);
                }
                *murder_backoff += 1;
                log.push_str(&format!("  aiming {murder_backoff} lower next pass\n"));
                // `None`, not an outcome: the enemy is alive, the board is intact and the word is
                // off, so this is a retry rather than an ending. The loop reads everything again
                // from the top — including the enemy state that disagreed with us — and the backoff
                // makes the next search pick something weaker instead of the same word for ever.
                return Ok(None);
            }
            match save::load(&self.combat_path) {
                Ok(next) => {
                    polls += 1;
                    let s = next.str_at("rpg.player.turnState").unwrap_or("");
                    let now: String = tiles_of(&next).iter().map(|t| t.letter.clone()).collect();
                    let hp = next.int_at("rpg.player.health").unwrap_or(-1);
                    let toxin = next.int_at("rpg.player.statusEffects.toxin").unwrap_or(0);
                    last = format!(
                        "state {s}, board {now}, {hp}hp{}",
                        if toxin > 0 { format!(", toxin {toxin}") } else { String::new() }
                    );
                    if s != "PlayerTurn" || now != letters {
                        break;
                    }
                }
                Err(_) => {
                    read_errors += 1;
                    break; // the file went away: combat is over
                }
            }
        }
        if Instant::now() >= until {
            log.push_str(&format!(
                "  turn did not advance: {polls} looks over 20s, {read_errors} unreadable; last saw {last}
"
            ));
        }
        Ok(None)
    }

    /// Cancels the avoidable-murder modal and clears the word behind it.
    ///
    /// Returns whether the modal actually went away. `false` means every press was refused or lost,
    /// and the caller must not go on to spell anything — the letters would land on a dialog.
    ///
    /// ## Backspace, not a click
    ///
    /// `defaultbinds/keyboard.lua:74` binds `backspace` to `goBack` in the second bind layer, and
    /// `doUserFunctionWithBindsFromSet` (`main.lua:471-480`) walks the layers in order taking the
    /// first handler that returns truthy. While the dialog is up, layer 1's `backspace` action
    /// resolves to nothing — the dialog installs only `affirmative` and `goBack`
    /// (`ui/twobuttondialogue.lua:17-23`), and there is no `coreUserFunctions.backspace` — so the
    /// lookup falls through to layer 2 and cancels. The dialog's own `goBack` shadows
    /// `coreUserFunctions.goBack`, so `canGoBack()` never gates it.
    ///
    /// The same key is safe for the letter-clearing afterwards for the mirror-image reason: on the
    /// combat screen layer 1 *does* bind it (`rpg.lua:221`, `wordboard.backspace() or true`), it
    /// always returns truthy, and layer 2 is never reached. One key, opposite meanings, decided
    /// entirely by which handler set is installed — which is why both halves are checked here
    /// rather than assumed to be the same press.
    ///
    /// A click would also have to wait out the 0.625s fade before the plaque is solid enough to
    /// aim at; the key is live the moment the dialog is announced.
    fn back_out_of_murder(
        &self, keys: &PostMessageInput, log: &mut String, tiles: usize,
    ) -> Result<bool, crate::Error> {
        for attempt in 1..=MURDER_CANCEL_TRIES {
            keys.focus();
            keys.press_key(crate::win::input::VK_BACK, crate::win::input::SC_BACK)?;
            // Pressing once and assuming is the failure this project repeats. The press can be lost
            // to focus, so the dialog going away is checked rather than expected.
            if self.murder_plaque_gone(Duration::from_secs(2))? {
                log.push_str(&format!("  cancelled on press {attempt}\n"));
                // **The word survives the cancel** — confirmed by the dev — because the modal
                // refused the attack rather than clearing the board. So every letter has to come
                // back off before another can be spelled.
                //
                // Two more presses than tiles, deliberately. A `QU` tile is one selection but two
                // letters, so tile count and letter count are not the same number and the
                // difference depends on the word; over-pressing costs nothing on an already-empty
                // word, while under-pressing leaves a stub that the next word would be typed onto.
                keys.focus();
                for _ in 0..tiles + 2 {
                    keys.press_key(crate::win::input::VK_BACK, crate::win::input::SC_BACK)?;
                    std::thread::sleep(Duration::from_millis(60));
                }
                log.push_str(&format!("  cleared {tiles} tiles worth of letters\n"));
                return Ok(true);
            }
            log.push_str(&format!("  press {attempt} did not dismiss the warning\n"));
        }
        log.push_str("  WARNING the murder warning would not go away\n");
        self.shot("murder-stuck");
        Ok(false)
    }

    /// Confirms the avoidable-murder dialog, killing the enemy we meant to frighten off.
    ///
    /// Space, because layer 1 binds it to `affirmative` (`defaultbinds/keyboard.lua:13`) and the
    /// dialog installs `affirmative` as its first button — `Accept` (`ui/confirm.lua:4-9`). The same
    /// key that submits a word on the board, which is why this is only ever called with the dialog
    /// confirmed present on the console.
    ///
    /// Only reached when the band is already at minimum damage; see [`nothing_left_to_lower`] for
    /// why a kill is preferred to replaying the same word.
    fn accept_murder(
        &self, keys: &PostMessageInput, log: &mut String,
    ) -> Result<(), crate::Error> {
        for attempt in 1..=MURDER_CANCEL_TRIES {
            keys.focus();
            keys.press_key(VK_SPACE, SC_SPACE)?;
            if self.murder_plaque_gone(Duration::from_secs(2))? {
                log.push_str(&format!("  accepted on press {attempt}\n"));
                return Ok(());
            }
            log.push_str(&format!("  press {attempt} did not confirm the warning\n"));
        }
        // The board still has the word on it and the modal is still up, so the next turn's
        // `wait_until_ready` reports `BoardNeverSettled` rather than this inventing an outcome.
        log.push_str("  WARNING could not confirm the murder warning either\n");
        self.shot("murder-stuck");
        Ok(())
    }

    /// Polls until the murder plaque is no longer on screen, or `within` elapses.
    ///
    /// This is the one job the template is genuinely good for. Detection had to come off the
    /// console because it is speculative and the fade makes an immediate look useless; *confirmation*
    /// is neither — we already know the dialog was there, so waiting out the fade costs nothing that
    /// was not going to be spent anyway, and the plaque disappearing is direct evidence the press
    /// landed.
    ///
    /// A capture error is **not** counted as absence. `act::locate` folds a failed grab into `Err`,
    /// and reading that as "gone" is how a check that cannot see becomes a check that says yes — the
    /// confusion `probe_button` was written to expose.
    fn murder_plaque_gone(&self, within: Duration) -> Result<bool, crate::Error> {
        let deadline = Instant::now() + within;
        loop {
            if let Ok(None) = crate::act::locate(self.win, &crate::act::MURDER_CANCEL) {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// Clicks `Finish`, confirms it took, and retries if it did not.
    ///
    /// Clicking once and hoping is what stalled a live run: a click that lands before the button is
    /// live is simply lost, and `activeIf` is the *live* state while ours comes from the save, so
    /// the two are not simultaneous. The console is checked before the save because it has no flush
    /// delay — and re-clicking this coordinate once the reward screen is up would land on the item
    /// row.
    ///
    /// ## Refuses to click at zero health
    ///
    /// This coordinate is not always `Finish`. `rpg.lua:592-597` is one button whose label is a
    /// function — `Eulogise` when `gameover`, `Give up` when `getPlayerHealth() <= 0` and enemies
    /// remain, `Finish` otherwise — and there is a second `Give up` at the same anchor
    /// (`rpg.lua:573-576`) under the same health condition. Both of the wrong labels end the run.
    ///
    /// The template match is not what makes this safe. A different word on the same plank measures
    /// 0.7843 against our `Finish` crop (see [`crate::act::COMBAT_FINISH`]) — the artwork is
    /// identical and only the glyphs differ, so there is a real score for `Give up` that no
    /// threshold can be *proven* to exclude while we have never captured one. The condition the game
    /// itself branches on is in the save we already hold, so read that instead: above zero health
    /// this slot cannot say anything but `Finish`.
    ///
    /// **Conservative, not exact, and knowingly so.** The game's condition is health `<= 0` *and*
    /// `fixedEnemiesRemaining() > 0`; at zero health with the area's enemies all dead the label is
    /// still `Finish`, and this refuses it. Closing that gap needs `#areaData.enemies`, which is not
    /// in `combatSaveData` — the save carries `stats.kills` and `stats.skippedEnemies`, two of the
    /// three terms, but not the area's total. The nearby proxy, "is the current enemy still alive",
    /// is wrong in the dangerous direction: a dead current enemy with more queued behind it is
    /// exactly a `Give up` screen. So the refusal stands until the area total can be read, and it
    /// says so in the log rather than stalling silently.
    fn finish(&self, feed: &mut Feed, log: &mut String, state: &str) -> Result<bool, crate::Error> {
        // Read the word on the button before anything else.
        //
        // The health check below infers what the slot *should* say; this asks what it *does* say, by
        // comparing the `Finish` and `Eulogise` templates and taking the better match. A live death
        // screen finally gave us the second one, and the pair separates by 0.147 where any single
        // threshold had eleven thousandths to work with. Independent of the save, so it still holds
        // when the save is stale or unreadable — and it errs toward "yes, Eulogise" when the screen
        // cannot be read at all.
        if crate::act::slot_is_eulogise(self.win) {
            log.push_str(
                "  **not clicking (0.9,0.9)**: the slot reads `Eulogise`, not `Finish` — the \
                 character is dead and this would end the run\n",
            );
            return Ok(false);
        }

        // A save we cannot read is not permission to click. Refusing costs a loop iteration;
        // guessing costs the run.
        match save::load(&self.combat_path).ok().and_then(|cs| cs.int_at("rpg.player.health")) {
            Some(h) if h > 0 => {}
            Some(h) => {
                // Zero health is NOT automatically a refusal, because at zero health with the
                // area's enemies all dead the slot still says `Finish` — and that is a fight we won
                // and want to bank. A live run hit exactly this on its first try, so it is the
                // common case, not the corner one.
                //
                // The exact condition is `health <= 0 AND fixedEnemiesRemaining() > 0`
                // (`rpg.lua:594`), and `fixedEnemiesRemaining` is `#areaData.enemies` minus kills.
                // `combatSaveData` has the kills; neither save has the area total, so the condition
                // cannot be evaluated. What is left is the button itself.
                //
                // The button is readable *because it is greyed until the state settles*. Measured on
                // that live frame: the inactive plank scores 0.8380, while an active one scores
                // 1.0000 twice over. So waiting for an active match both avoids clicking a dead
                // button and gives a much stronger reading than the 0.90 used elsewhere.
                //
                // **Residual risk, stated plainly:** an active `Give up` has never been captured, so
                // nothing measured rules out its scoring above the bar. The estimate is that the
                // lettering is roughly 7% of the crop, which puts a word swap well below 0.97 — but
                // that is arithmetic, not a measurement. The best score is logged on failure so the
                // next run that lands here settles it with a number instead of an estimate.
                let seen = crate::act::wait_for(
                    self.win,
                    &crate::act::COMBAT_FINISH,
                    crate::act::COMBAT_FINISH_ACTIVE,
                    Duration::from_secs(6),
                );
                if !seen.found() {
                    log.push_str(&format!(
                        "  **not clicking (0.9,0.9) at {h} health**: waited for an active `Finish` \
                         and the best score was {:.4} over {} looks (need {:.2}). At zero health \
                         this slot reads `Give up` when enemies remain (`rpg.lua:594`), so an \
                         unconfirmed button is not pressed.\n",
                        seen.best,
                        seen.looks,
                        crate::act::COMBAT_FINISH_ACTIVE
                    ));
                    return Ok(false);
                }
                log.push_str(&format!(
                    "  at {h} health, but the slot is an active `Finish` ({:.4}) — the area is \
                     clear and this fight is won\n",
                    seen.best
                ));
            }
            None => {
                log.push_str(
                    "  **not clicking (0.9,0.9)**: could not read `rpg.player.health`, so whether \
                     the slot says `Finish` or `Give up` is unknown\n",
                );
                return Ok(false);
            }
        }
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

    /// Picks a reward and clears the postgame, for a screen found outside a fight.
    ///
    /// A reward screen outlives the fight that produced it: it is still up on the next iteration of
    /// whatever loop is driving, and until it is dismissed there is no map, no area buttons and no
    /// affirmative — every later step fails for want of a screen nobody looked at. That is exactly
    /// how a cleared crypt turned into four `Absent` readings and a run out of steps.
    pub fn claim_reward(
        &self, feed: &mut Feed, keys: &PostMessageInput, log: &mut String, deadline: Instant,
    ) -> Result<Outcome, crate::Error> {
        self.take_reward(feed, keys, log, 0, deadline)
    }

    /// Picks a reward, confirms it, and clears the postgame.
    fn take_reward(
        &self, feed: &mut Feed, keys: &PostMessageInput, log: &mut String, turns: usize,
        deadline: Instant,
    ) -> Result<Outcome, crate::Error> {
        // Marked BEFORE `choose`, because `choose` is what presses Confirm. A mark taken afterwards
        // races the game: `Postgame screen:` can already be in the feed by the time we start
        // watching for it, and `seen_since` would then wait out its whole timeout for a line that
        // had already arrived.
        let mark = feed.mark();
        match crate::itemchoice::choose(self.win, feed, keys, &self.game_dir, log, deadline)? {
            crate::itemchoice::Chosen::Took(key) => {
                return self.after_confirm(feed, keys, log, turns, deadline, Some(key), mark);
            }
            other => {
                log.push_str(&format!("  no reward taken: {other:?}
"));
                self.shot("reward-not-taken");
                return Ok(Outcome::Cleared { turns, reward: None });
            }
        }
    }

    /// Everything after `Confirm`: granting the item opens the postgame, which has to be cleared
    /// before the map comes back. Fight-specific, which is why it did not move out with the screen.
    /// **Confirm has already been pressed** — by [`crate::itemchoice::choose`], which owns it.
    ///
    /// This used to press it again, and that crashed the game. Extracting the item screen into its
    /// own module moved the Space press into `choose` without removing the one here, so Confirm was
    /// pressed twice: the first press ran `viewPostgameAndReturn`, which grants the item and then
    /// `assert(love.filesystem.remove('combatSaveData'))`; the second re-entered the same handler
    /// with the file already gone, `remove` returned false, and the assert took the process down
    /// with `overworld.lua:1154: Failed to clean dungeon run save data`.
    ///
    /// Worth being precise about, because the traceback points at a file-deletion failure and the
    /// project already has a hard-won rule about not touching `combatSaveData` while the game is
    /// deleting it. That rule is real, it is honoured below — and it was **not** what happened here.
    /// Nothing had the file open. It had simply already been deleted, by us, a moment earlier.
    fn after_confirm(
        &self, feed: &mut Feed, keys: &PostMessageInput, log: &mut String, turns: usize,
        deadline: Instant, key: Option<String>, mark: usize,
    ) -> Result<Outcome, crate::Error> {
        self.clear_postgame(feed, keys, log, mark, deadline);
        Ok(Outcome::Cleared { turns, reward: key })
    }

    /// Waits for the postgame to appear and dismisses it.
    ///
    /// Shared by both endings, because both raise it: granting a reward opens it via
    /// `viewPostgameAndReturn`, and a rewardless win opens it directly. Until it is dismissed there
    /// is no map behind it.
    ///
    /// Errors are swallowed on purpose. Every caller has already achieved the thing it set out to do
    /// — the fight is over and any reward is banked — so a failure to press Continue should degrade
    /// to "the next loop iteration sees a screen" rather than discard that outcome.
    fn clear_postgame(
        &self, feed: &mut Feed, keys: &PostMessageInput, log: &mut String, mark: usize,
        deadline: Instant,
    ) -> bool {
        // Watch the CONSOLE, not `combatSaveData`. `viewPostgameAndReturn` asserts on deleting that
        // file, and polling it across this window made the delete fail and crashed the game.
        // Two independent ways in, because they fail differently. The console line is definitive but
        // arrives through a screen-buffer scrape that can lag; the fingerprint is immediate but is
        // one number. Either is enough to know the screen is up.
        let until = deadline.min(Instant::now() + Duration::from_secs(20));
        let (mut postgame, mut how) = (false, "");
        while Instant::now() < until && !postgame {
            std::thread::sleep(Duration::from_millis(250));
            feed.pump();
            if feed.seen_since(mark, "Postgame screen:") {
                postgame = true;
                how = "console";
            } else if matches!(
                crate::act::score_exact(self.win, &crate::act::POSTGAME_CONTINUE),
                Ok(q) if q >= crate::act::POSTGAME_CONTINUE_PRESENT
            ) {
                postgame = true;
                how = "the Continue fingerprint";
            }
        }
        log.push_str(&format!(
            "  postgame reached: {postgame}{}\n",
            if postgame { format!(" (via {how})") } else { String::new() }
        ));
        if postgame {
            // Postgame's Continue is `affirmative` -> `goBack()`, guarded by `activeIf = backMode`.
            keys.focus();
            std::thread::sleep(Duration::from_millis(300));
            if keys.press_key(VK_SPACE, SC_SPACE).is_err() {
                log.push_str("  could not send Space to the postgame\n");
                return false;
            }
            // Verify it actually went, rather than trusting the keystroke. This screen is the one
            // that gates the map behind it, and a press that did not land looks identical to one
            // that did until something checks.
            let gone_by = Instant::now() + Duration::from_secs(6);
            let mut gone = false;
            while Instant::now() < gone_by && !gone {
                std::thread::sleep(Duration::from_millis(250));
                gone = !matches!(
                    crate::act::score_exact(self.win, &crate::act::POSTGAME_CONTINUE),
                    Ok(q) if q >= crate::act::POSTGAME_CONTINUE_PRESENT
                );
            }
            log.push_str(&format!("  postgame dismissed: {gone}\n"));
            feed.pump();
            return gone;
        }
        postgame
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
#[cfg(test)]
mod murder_accept_tests {
    use super::*;
    use crate::search::MIN_MEANINGFUL_DAMAGE as MIN;

    /// The narrowest band there is means the backoff is spent.
    #[test]
    fn a_band_at_minimum_damage_has_nowhere_left_to_go() {
        assert!(nothing_left_to_lower(Goal::Scare { need: MIN, below: MIN + 1 }));
    }

    /// Anything wider still has a weaker word to try, however many times we have been refused.
    ///
    /// The rule this replaced counted refusals, which confirmed the kill after six even when the
    /// band still had room — from a ceiling of 12 that abandons five untried steps.
    #[test]
    fn a_band_with_room_keeps_backing_off() {
        assert!(!nothing_left_to_lower(Goal::Scare { need: 4, below: 6 }));
        assert!(!nothing_left_to_lower(Goal::Scare { need: 1, below: 12 }));
        assert!(!nothing_left_to_lower(Goal::Scare { need: MIN, below: MIN + 2 }));
    }

    /// A goal that is not a scare already intends the kill, so the warning is not a disagreement.
    #[test]
    fn a_kill_goal_confirms_immediately() {
        assert!(nothing_left_to_lower(Goal::FirstKill { need: 9 }));
        assert!(nothing_left_to_lower(Goal::RankedKill { need: 9 }));
        assert!(nothing_left_to_lower(Goal::MaxDamage));
    }
}

#[cfg(test)]
mod murder_dialog_tests {
    use super::*;

    /// The line as the game actually printed it, copied from `spike-run-raw.log` after the live run
    /// that met this dialog. A positive control taken from real output rather than from the format
    /// string in `twobuttondialogue.lua` — if the two ever disagree, this is the one that matters.
    const REAL: &str =
        "Okay dialogue:\tIt looks like you're about to commit avoidable murder, are you sure?";

    #[test]
    fn the_line_the_game_printed_is_recognised() {
        assert!(announces_murder_dialog(REAL), "the captured line must match: {REAL}");
    }

    #[test]
    fn the_option_lines_under_it_are_not_mistaken_for_the_announcement() {
        // The two lines that follow carry the shortcut names, and firing on those would run the
        // handler three times for one dialog.
        assert!(!announces_murder_dialog("\t\tOption1:\tAccept\tShortcut:\taffirmative"));
        assert!(!announces_murder_dialog("\t\tOption2:\tCancel\tShortcut:\tgoBack"));
    }

    #[test]
    fn prose_about_murder_that_is_not_this_dialog_is_ignored() {
        // Both halves are load-bearing: the game prints event text and a dictionary full of words.
        assert!(!announces_murder_dialog("patricide=\"[[murder|Murder]] of one's [[father]].\""));
        assert!(!announces_murder_dialog("\t[Murderer][Combat] - \"Are you alone?\""));
        // A different dialog must not trigger the murder handler.
        assert!(!announces_murder_dialog("Okay dialogue:\tAbandon this run?"));
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

/// The player's side of the killing-blow decision, from `combatSaveData`.
///
/// Returns `None` when nothing about overkill matters, so the search keeps its ordinary goal.
fn player_state(
    cs: &Table, mods: &crate::search::Modifiers,
) -> Option<crate::search::PlayerState> {
    let current = cs.int_at("rpg.player.health")?;
    let max = cs.int_at("rpg.player.maxHealth")?;
    let status = |k: &str| cs.path(&format!("rpg.player.statusEffects.{k}")).is_some();
    let gear = |k: &str| cs.path(&format!("rpg.player.gearFlags.{k}")).is_some();

    // The charge lives in statusEffects; the gear flag grants the same heal permanently
    // (`rpgview.lua:1082-1083` accepts either source).
    let consumes_charge = status("wellRestedCampfire") || status("wellRestedInn");
    let granted = consumes_charge || gear("wellRestedCampfire") || gear("wellRestedInn");
    if !granted {
        return None;
    }
    // Either the player's gear or the enemy's own status can cancel it (`:1084-1085`).
    let cancelled = gear("overkillNoHeal")
        || gear("overkillHealToGold")
        || mods.overkill_no_heal
        || mods.overkill_heal_to_gold;
    Some(crate::search::PlayerState {
        vitals: crate::rested::Vitals { current, max },
        heals: !cancelled,
        consumes_charge,
        bleeding: status("bleed"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_that_would_not_go_on_a_clean_board_is_played_again() {
        // The whole point of #30. The run that prompted it died at `l1` on its first stray, with a
        // playable board in front of it and the enemy still standing.
        assert_eq!(recover_from(1, true), Recovery::PlayOn);
        assert_eq!(recover_from(SELECT_ATTEMPTS - 1, true), Recovery::PlayOn);
    }

    #[test]
    fn a_board_that_would_not_clear_stops_even_with_attempts_left() {
        // Retrying onto a half-selected board submits a word nothing scored, which is a worse
        // failure than the one being recovered from. Attempts remaining must not buy their way past
        // this.
        assert_eq!(recover_from(1, false), Recovery::Stop);
        assert_eq!(recover_from(0, false), Recovery::Stop);
    }

    #[test]
    fn failing_every_time_still_ends_the_fight() {
        // Consecutive failures mean the board is no longer understood; playing on forever would
        // turn a diagnosable stop into a hang.
        assert_eq!(recover_from(SELECT_ATTEMPTS, true), Recovery::Stop);
        assert_eq!(recover_from(SELECT_ATTEMPTS + 1, true), Recovery::Stop);
    }
}
