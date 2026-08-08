//! Finding a word that kills the enemy.
//!
//! Per the MVP scope: **a race, not an optimisation.** Threads take contiguous alphabetical slices of
//! the dictionary and the first to find a lethal word wins; the rest stop. Optimal play is not the
//! goal, and for non-boss enemies a killing word is usually easy to find.
//!
//! Contiguous slices are deliberately not a balanced partition — a slice whose initial letters are
//! absent from the board yields nothing at all. That is fine for a race (any thread can win) and it
//! keeps the split trivial, but it is worth knowing that thread load is wildly uneven by design.
//!
//! ## What makes a word playable
//!
//! Not "does the board hold these letters" but "does typing this word work" — see [`crate::typist`].
//! The game's `textinput` is a specific greedy consumer that decides which tile each character
//! takes, and that choice sets both the tile state that scores the word and the corner count that
//! `resistCornerless` scales it by. Adjacency is not required unless the player carries
//! `wordRequirementAdjacent` gear, which [`Modifiers::from_save`] refuses to search under rather
//! than quietly assuming away.
//!
//! ## Word length
//!
//! There is **no minimum**: "a" and "I" are ordinary English words and the game accepts them. An
//! earlier version guessed at a 3-letter floor and silently dropped every one- and two-letter entry,
//! which both shrank the dictionary and hid legitimate plays. Note a single wood letter scores
//! `floor(1 * 0.4 + 0.5) = 0`, so short words are usually worthless rather than illegal — the search
//! rejects them on score, which is the honest reason, not on length.

use crate::geometry::Geometry;
use crate::observe::board::Tile;
use crate::score::Scorer;
use crate::typist::Typist;
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Shortest word the search will consider. One: the game has no minimum, and "a"/"I" are real words.
pub const MIN_WORD_LEN: usize = 1;

pub struct Dictionary {
    /// Uppercased, A-Z only, in file order — which is roughly alphabetical, so a contiguous slice is
    /// a contiguous alphabetical range.
    words: Vec<String>,
}

impl Dictionary {
    /// Reads the game's dictionary.
    ///
    /// `utils/dictionary.lua` is 345k lines of `word="definition",`, with keys occasionally bracketed
    /// (`["'un"]="…"`) when they are not valid Lua identifiers. Scanned line-by-line in Rust rather
    /// than evaluated as Lua: the definitions are megabytes of text we have no use for, and building
    /// a 345k-entry Lua table to throw away the values would be pure waste.
    pub fn load(game_dir: &Path) -> Result<Self, crate::Error> {
        let path = game_dir.join("utils/dictionary.lua");
        let text = std::fs::read_to_string(&path)?;
        let mut words = Vec::with_capacity(350_000);
        for line in text.lines() {
            let line = line.trim();
            let key = if let Some(rest) = line.strip_prefix("[\"") {
                rest.split_once("\"]").map(|(k, _)| k)
            } else {
                line.split_once('=').map(|(k, _)| k.trim())
            };
            let Some(key) = key else { continue };
            // Only pure alphabetic words are playable: the dictionary also holds entries with
            // apostrophes, hyphens and spaces, none of which are on the board.
            if key.len() >= MIN_WORD_LEN && key.chars().all(|c| c.is_ascii_alphabetic()) {
                words.push(key.to_ascii_uppercase());
            }
        }
        words.sort_unstable();
        words.dedup();
        if words.is_empty() {
            return Err(crate::Error::Config(format!(
                "no words parsed from {}",
                path.display()
            )));
        }
        Ok(Dictionary { words })
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn words(&self) -> &[String] {
        &self.words
    }
}

/// Everything about the enemy that changes what a word is worth.
///
/// Assembled from `combatSaveData` once per turn. The two members are opposite in character:
/// `excluded` throws words away, `resist_cornerless` changes what the survivors score.
pub struct Modifiers {
    /// Words from a lexicon this enemy resists or is immune to. `nerf * 0` is a zero-damage turn, so
    /// these are dropped outright rather than scored down (see [`crate::lexica`]).
    pub excluded: HashSet<String>,
    /// `resistCornerless` (`utils/words.lua:238-240`): the score is scaled by
    /// `cornersUsed / cornerCount`. Zero corners means zero damage.
    pub resist_cornerless: bool,
    /// Damage **beyond** what kills is paid out as gold (`rpgview.lua:1211-1214`), granted either by
    /// the enemy — the mimic carries `statusEffects.overkillGold` — or by a player curse from
    /// `items/curses.lua`. When set, the first killing word is the wrong answer: gold scales
    /// linearly with the excess, so the turn is worth the *hardest* hit we can find.
    pub overkill_gold: bool,
    /// Reasons the search should not be trusted. Non-empty means a score here is a guess.
    pub problems: Vec<String>,
    /// The quitting status this enemy carries, if any (`fear`, `terror`, `caution`).
    ///
    /// Read from `rpg.enemy.statusEffects`, which the save does carry
    /// (`rpgview.lua:2513-2522`). Its presence is the whole trigger for preferring a scare: the
    /// enemies that have it are mostly human, and killing a human is recorded as murder
    /// (`onDeathFlags = {'Murder', ...}`, `rpg/enemies/humans.lua:58`).
    pub nerve: Option<crate::flee::Nerve>,
    /// `immobile` — it cannot run, so no threshold will make it leave (`rpgview.lua:1646`).
    pub immobile: bool,
    /// Lexicons this enemy takes extra damage from, and by how much. Applied per word, because
    /// membership is a property of the word rather than of the board.
    pub bonuses: Vec<(HashSet<String>, f64)>,
    /// This enemy cancels overkill healing outright (`rpgview.lua:1084`).
    pub overkill_no_heal: bool,
    /// This enemy turns overkill healing into gold instead (`:1085`), which is the curse described
    /// in `items/curses.lua:111`. Healing does not happen, so there is no charge to conserve.
    pub overkill_heal_to_gold: bool,
}

impl Modifiers {
    /// Reads the enemy's statuses and the board's shape out of a save table.
    ///
    /// `tile_count` is the board dump's length, used to check the derived shape fits.
    pub fn from_save(
        game_dir: &Path,
        save: &crate::game::save::Table,
        tile_count: usize,
    ) -> Result<(Self, Geometry), crate::Error> {
        let statuses = crate::lexica::Lexica::statuses_from_save(save);
        let lexica = crate::lexica::Lexica::load(game_dir)?;
        let resolved = Geometry::from_save(save, tile_count);

        let mut problems = resolved.problems.clone();
        problems.extend(lexica.problems().iter().cloned());
        problems.extend(lexica.unmodelled(&statuses));
        if resolved.geometry.adjacency_required {
            // Every pick would be confined to the 3x3 around the last one. Searching as if the whole
            // board were reachable would produce words that cannot be typed at all.
            problems.push("wordRequirementAdjacent gear is not modelled".into());
        }

        // Either source grants it, so both are checked (`rpgview.lua:1211`).
        let overkill_gold = statuses.contains_key("overkillGold")
            || save.path("rpg.player.gearFlags.overkillGold").is_some();

        Ok((
            Modifiers {
                excluded: lexica.excluded_words(&statuses),
                resist_cornerless: statuses.contains_key("resistCornerless"),
                overkill_gold,
                problems,
                nerve: ["fear", "terror", "caution"]
                    .iter()
                    .find(|k| statuses.contains_key(**k))
                    .and_then(|k| crate::flee::Nerve::from_status(k)),
                immobile: save.path("rpg.enemy.immobile").is_some(),
                bonuses: lexica.bonus_sets(&statuses),
                overkill_no_heal: statuses.contains_key("overkillNoHeal"),
                overkill_heal_to_gold: statuses.contains_key("overkillHealToGold"),
            },
            resolved.geometry,
        ))
    }

    /// A modifier set for an ordinary enemy on an ordinary board.
    pub fn none() -> Self {
        Modifiers {
            excluded: HashSet::new(),
            resist_cornerless: false,
            overkill_gold: false,
            problems: Vec::new(),
            nerve: None,
            immobile: false,
            bonuses: Vec::new(),
            overkill_no_heal: false,
            overkill_heal_to_gold: false,
        }
    }

    /// The `mult * nerf` a word earns, given how many corners it used.
    ///
    /// `getWordBonusModifier` only ever multiplies, so an enemy without `resistCornerless` gets a
    /// flat 1. Bonuses above 1 are deliberately not claimed: we never want to call a word lethal
    /// because of a bonus we mis-read.
    pub fn modifier(&self, corners_used: usize, corner_count: usize) -> f64 {
        if !self.resist_cornerless || corner_count == 0 {
            return 1.0;
        }
        corners_used as f64 / corner_count as f64
    }

    /// The full multiplier for one word: the corner nerf, times this enemy's lexicon bonuses.
    ///
    /// `utils/words.lua:219-242` builds the bonus additively — `mult = mult + val - 1` per matching
    /// lexicon — so two 1.5x lexicons make 2.0x, not 2.25x.
    pub fn modifier_for(&self, word: &str, corners_used: usize, corner_count: usize) -> f64 {
        let mut mult = 1.0;
        for (words, val) in &self.bonuses {
            if words.contains(word) {
                mult += val - 1.0;
            }
        }
        self.modifier(corners_used, corner_count) * mult
    }
}

/// A word the search found worth reporting.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub word: String,
    pub score: i64,
    /// Which dictionary slice found it — useful for seeing whether the split is pulling its weight.
    pub slice: usize,
}

/// The result of a search.
///
/// ## Open, post-MVP: these three fields should probably be one ranked candidate
///
/// Recorded because it is a real design argument that was had and deferred, not an oversight.
///
/// The shape below tracks three things separately, and each costs a local, a `Mutex` and a merge
/// block in every search function — which is most of what makes [`ranked_kill`] long. The claim
/// against it: lethality is being expressed as a **branch** (`if score >= need { rank it }`) when it
/// could be a **field**, leaving one comparable key and no special case.
///
/// The defence offered at the time was cost — ranking every candidate rather than only lethal ones.
/// That was overstated. [`crate::pick::rank`] is only reachable once [`crate::typist`] has already
/// accepted the word, so the real comparison is "every typeable word" against "every lethal word",
/// which is a far smaller ratio than the dictionary's size suggests, on a function that is roughly
/// `tiles × removed`. It was never measured.
///
/// What does survive the argument is that the two regimes optimise **different things**, not the
/// same thing either side of a threshold:
///
/// - lethal → rank by payout and board hygiene; score past the threshold is irrelevant
/// - not lethal → [`Outcome::best`], which is highest **score**, because if we cannot kill we want
///   to hit hardest
///
/// So a flat uniform key does not express the intent: put `lethal` on top and let the existing terms
/// fall through, and non-lethal words end up ranked by board tidiness, which is wrong.
///
/// The shape that takes both points is one `Rank` carrying `lethal` plus both sub-orderings, with
/// `better_than` switching on it:
///
/// ```text
/// Rank { lethal, wood_only, hazard_fall, deviation,  // when lethal
///                score }                             // when not
/// ```
///
/// That collapses `lethal` and `best` into one tracked candidate and one mutex, keeps the differing
/// objectives visible rather than hidden in a branch, and takes the special case out of the hot loop.
/// [`Outcome::longest`] stays separate regardless — the refresh rule is about word *length*, which is
/// a third question and not an ordering of the same kind.
///
/// Deferred rather than done because it touches [`Outcome::choice`], [`Outcome::should_refresh`] and
/// every test that reads these fields, and the ranking it would restructure has not yet been watched
/// working in a real fight. Restructuring an unproven thing is how you end up unable to tell which
/// change broke it.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    /// The lethal word the search settled on.
    ///
    /// **Which one that is depends on the goal**, and the difference matters:
    /// [`Goal::FirstKill`] reports whichever thread got there first and stops everything, while
    /// [`Goal::RankedKill`] reports the best by [`crate::pick::Rank`] after seeing them all.
    pub lethal: Option<Found>,
    /// Highest-scoring word seen, for when nothing is lethal.
    pub best: Option<Found>,
    /// Longest makeable word seen. Tracked separately because the refresh rule is about LENGTH, and
    /// the highest-scoring word is not necessarily the longest — a short word of gold tiles can
    /// outscore a long one of wood. Free to collect: the only time it matters is when no lethal word
    /// was found, and that case already scans the whole dictionary.
    pub longest: Option<Found>,
    pub words_considered: usize,
}

impl Outcome {
    /// Should the board be refreshed rather than played?
    ///
    /// The user's MVP rule: refresh when the longest word found is shorter than half the board. Cheap
    /// to evaluate because the `--verbose` board dump is truncated to `totalTileCount`, so the dump's
    /// own length is the threshold ([`crate::observe::board::BoardDump::refresh_threshold`]).
    ///
    /// A lethal word always wins over refreshing: killing the enemy ends the exchange, which beats any
    /// board-quality consideration.
    pub fn should_refresh(&self, threshold: usize) -> bool {
        if self.lethal.is_some() {
            return false;
        }
        match &self.longest {
            Some(b) => b.word.chars().count() < threshold,
            // Nothing playable at all is the strongest case for a refresh.
            None => true,
        }
    }

    /// What to actually play: the lethal word if there is one, else the best found.
    pub fn choice(&self) -> Option<&Found> {
        self.lethal.as_ref().or(self.best.as_ref())
    }
}

/// Races `threads` slices of the dictionary for a word that kills.
///
/// `need` is the damage required — enemy health plus armour, with an absent armour key treated as
/// zero by the caller (the captured save omits it entirely).
///
/// Stops every thread as soon as one finds a lethal word, so the cost is usually a small fraction of
/// the dictionary. The best-scoring word is still tracked, because the fallback needs it and the scan
/// that produced it was already paid for.
/// What this turn is trying to achieve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Goal {
    /// Kill the enemy. The **first** lethal word wins and the rest stop — optimal play is not the
    /// point, ending the exchange is.
    ///
    /// **Parked for the ordinary kill, and still live for one branch.** [`Goal::RankedKill`] has
    /// taken over the general case while it is being evaluated against this; if the ranking does not
    /// earn the full dictionary scan it costs, this is what we come back to.
    ///
    /// The exception is deliberate and is `killing_blow`'s [`crate::rested::Aim::HealFully`]: a
    /// player who heals on overkill wants the *deepest* overkill that still lands, and ranking a
    /// board's leftover letters would pull against that. That branch is outside the ranking's remit
    /// anyway — it is the well-rested case, and the ranking was asked for on the general one.
    FirstKill { need: i64 },
    /// Kill the enemy, and among the words that do, play the best one.
    ///
    /// Scans the whole dictionary rather than stopping at the first kill, because "best" is not
    /// knowable until the candidates are in hand. That is the same cost [`Goal::MaxDamage`] already
    /// pays, which is what makes it affordable rather than merely desirable.
    ///
    /// What "best" means is [`crate::pick::Rank`]: a wood-only kill, then hazard tiles driven toward
    /// the bottom of the board, then the letters left behind. Damage beyond the kill is deliberately
    /// **not** a criterion — overkill is worth gold only when the enemy carries it, and that case is
    /// [`Goal::MaxDamage`]'s.
    RankedKill { need: i64 },
    /// Hit as hard as possible. Required when overkill pays gold (`rpgview.lua:1211-1214`): the
    /// excess damage *is* the reward, so stopping at the first kill leaves money on the table.
    MaxDamage,
    /// Frighten the enemy off instead of killing it: damage of at least `need`, but strictly less
    /// than `below` so it survives to run.
    ///
    /// Worth more than the kill it replaces. The flee test is also evaluated inside `enemyCanHit`
    /// (`rpgview.lua:1046-1052`), against the health the enemy *would* have -- so the turn that
    /// pushes it past the line is also a turn it does not get to attack. And for the enemies that
    /// carry `fear`, mostly humans, the kill would be recorded as murder.
    Scare { need: i64, below: i64 },
}

/// Extra damage aimed for beyond a threshold, to absorb error in our own damage model.
///
/// Our score is a reimplementation of the game's, not the game's, and it can be optimistic. Live:
/// `Neil Patrick Harrow 19+2hp` needed 21, the search played `CRACKS` believing it did 20 or more,
/// and the next turn showed the enemy on **1 hp**. One point short turned a kill into another turn
/// of being hit back — and against a fearable enemy the same error is the difference between it
/// fleeing and it staying.
///
/// One, not more: every point of buffer discards words that would in fact have been enough, and the
/// observed error is one. This is a hedge against a model that is slightly wrong, not a licence to
/// ignore what it says.
pub const DAMAGE_BUFFER: i64 = 1;

/// The buffer actually affordable inside a band of `slack` spare damage.
///
/// A band has two edges and both want protecting, so a full buffer costs `2 * DAMAGE_BUFFER` of
/// width. Where the band is narrower than that, the buffer shrinks rather than inverting the band —
/// which matters precisely for the enemies the band exists for. A `fear` enemy on low maximum health
/// has almost no room between "hurt enough to run" and "dead": at 4 health from a maximum of 6 the
/// whole non-lethal band is 2..3, and a fixed buffer would empty it and send us back to killing
/// something we could have frightened off.
///
/// Splitting the slack evenly is the graceful version of the same idea: as much margin as exists,
/// shared between the two ways of being wrong, and zero when there is none to share.
fn affordable_buffer(slack: i64) -> i64 {
    DAMAGE_BUFFER.min((slack / 2).max(0))
}

impl Goal {
    /// Picks the goal for an enemy, given what modifies it.
    ///
    /// `max_health` is inferred rather than read: the save's enemy block carries
    /// `health, armour, attacksCycle, statusEffects, name, state2, immobile, disguise`
    /// (`rpgview.lua:2513-2522`) and no maximum, so the caller supplies the largest health it has
    /// seen this enemy at. Both directions of error are safe -- guess low and the required damage
    /// exceeds what would kill, so no scare is offered; guess high and the hit lands short of the
    /// threshold and the fight simply continues.
    pub fn for_enemy(
        mods: &Modifiers, health: i64, armour: i64, max_health: Option<i64>,
        player: Option<&PlayerState>,
    ) -> Goal {
        // What actually kills. Kept exact, because it is also the ceiling a scare must stay under —
        // buffering it in one direction would license the very kill we are avoiding.
        let lethal = health + armour;
        // What we AIM for to kill. Overshooting a kill costs nothing but a slightly worse word;
        // undershooting costs a whole turn, and a turn of being hit back.
        let need = lethal + DAMAGE_BUFFER;
        // Ranked rather than first-past-the-post: among the words that kill, which one leaves the
        // best board and collects whatever the gear pays. See [`Goal::RankedKill`], and
        // [`Goal::FirstKill`] for what this replaced and why it is still here.
        let kill = Goal::RankedKill { need };
        if mods.overkill_gold {
            // Gold scales with the excess, so this one really does want the corpse.
            return Goal::MaxDamage;
        }
        // `immobile` suppresses fleeing outright (`rpgview.lua:1646`) -- it cannot run, so trying to
        // frighten it just leaves it alive and swinging.
        if mods.immobile {
            return Self::killing_blow(kill, lethal, player);
        }
        let (Some(nerve), Some(max)) = (mods.nerve, max_health) else {
            return Self::killing_blow(kill, lethal, player);
        };
        let Some(top) = nerve.leaves_at_or_below(max) else {
            return Self::killing_blow(kill, lethal, player);
        };
        let below = lethal;
        // Armour absorbs first, so reaching `top` health costs the difference plus the armour.
        let scare_need = ((health - top) + armour).max(0);
        if scare_need >= below {
            // No non-lethal hit reaches the threshold.
            return Self::killing_blow(kill, lethal, player);
        }
        // Buffer both edges by as much as the band can spare. `slack` is the width of the raw
        // non-lethal band `scare_need ..= below - 1`; see `affordable_buffer` for why this shrinks
        // instead of inverting.
        let b = affordable_buffer(below - 1 - scare_need);
        Goal::Scare { need: scare_need + b, below: below - b }
    }

    /// The kill, adjusted for what a well-rested charge is worth on this particular wound.
    ///
    /// Overkill on a killing blow heals `floor(overkill/2)` and spends a charge, but **only if the
    /// heal is positive** (`rpgview.lua:1086, 1204-1210`) — so an overkill of 1 is free. See
    /// [`crate::rested`] for the mechanic in full.
    /// `lethal` is the EXACT damage that kills, unbuffered, because every branch here is arithmetic
    /// about overkill and a padded threshold would silently shift the free window.
    fn killing_blow(kill: Goal, lethal: i64, player: Option<&PlayerState>) -> Goal {
        let Some(p) = player else { return kill };
        if !p.heals {
            return kill;
        }
        match crate::rested::aim(p.vitals, true, p.bleeding) {
            crate::rested::Aim::Best => kill,
            // Keep the charge: kill with an overkill of 0 or 1, which heals nothing. Only worth
            // doing when a charge is actually consumable -- a gear flag heals for free, so there is
            // nothing to protect and a scratch may as well be topped up.
            //
            // Deliberately unbuffered, and consistently so: the free window is two wide, and
            // `affordable_buffer(1)` is 0 — there is no slack here to spend on safety. Falling one
            // short costs a turn; buffering would cost the charge this branch exists to protect.
            crate::rested::Aim::Frugal if p.consumes_charge => {
                Goal::Scare { need: lethal, below: lethal + 2 }
            }
            crate::rested::Aim::Frugal => kill,
            // Spend it well: the first answer whose half-overkill covers the deficit. If none does,
            // the search falls back to its best-scoring word, which is exactly "if we cannot heal
            // fully, use the best scoring answer".
            //
            // Buffered, because this is a kill with no upper bound: overshooting heals a little more
            // than needed, undershooting fails to kill at all.
            crate::rested::Aim::HealFully => {
                Goal::FirstKill { need: lethal + 2 * p.vitals.missing() + DAMAGE_BUFFER }
            }
        }
    }
}

/// What the player brings to the choice of killing blow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerState {
    pub vitals: crate::rested::Vitals,
    /// Would overkill heal at all? Either a `wellRested*` status or the matching gear flag grants
    /// it, and `overkillNoHeal` / `overkillHealToGold` cancel it (`rpgview.lua:1081-1085`).
    pub heals: bool,
    /// Is a limited charge spent doing so? True for the status, false for a gear flag — the
    /// decrement at `:1204-1210` only touches `statusEffects`, so a gear flag heals for free and
    /// there is nothing to conserve.
    pub consumes_charge: bool,
    /// `bleed` skips the heal branch entirely (`:1080`).
    pub bleeding: bool,
}

/// How many words a thread claims at a time under [`Goal::MaxDamage`].
///
/// Large enough that the atomic fetch is lost in the noise, small enough that the final claims
/// cannot leave one thread working alone for long.
const CLAIM: usize = 512;

/// Searches for the word to play.
///
/// ## Two goals, two partitioning strategies, deliberately
///
/// **`FirstKill` takes contiguous alphabetical slices.** They are wildly unbalanced — a slice whose
/// initial letters are absent from the board yields nothing — and that is fine, because any thread
/// can win and everything stops the moment one does. Balancing would cost more than it saves.
///
/// **`MaxDamage` cannot stop early.** Every word must be examined, so the slowest thread sets the
/// wall time and an unbalanced split leaves one thread grinding the long-word tail while the rest
/// idle. Threads therefore claim [`CLAIM`]-word blocks from a shared cursor: whoever finishes takes
/// more.
///
/// Work-stealing rather than a length-weighted static split, because a word's cost is not simply its
/// length: [`crate::typist`] rescans the board per character, the ligature clauses rescan again, and
/// a word that fails on its first letter costs almost nothing. Weighting by length models one term
/// and mis-models the rest. Claiming on demand needs no cost model at all, and stays balanced if the
/// cost profile later changes.
pub fn search(
    dict: &Dictionary,
    scorer: &Scorer,
    tiles: &[Tile],
    geometry: &Geometry,
    mods: &Modifiers,
    goal: Goal,
    picking: &crate::pick::Context,
    threads: usize,
) -> Outcome {
    match goal {
        Goal::FirstKill { need } => {
            race_for_band(dict, scorer, tiles, geometry, mods, need, None, threads)
        }
        Goal::RankedKill { need } => {
            ranked_kill(dict, scorer, tiles, geometry, mods, need, picking, threads)
        }
        Goal::Scare { need, below } => {
            race_for_band(dict, scorer, tiles, geometry, mods, need, Some(below), threads)
        }
        Goal::MaxDamage => max_damage(dict, scorer, tiles, geometry, mods, threads),
    }
}

/// Every lethal word, ranked; plus the fallbacks for when none is.
///
/// Structured as [`max_damage`] is — the same work-stealing scan over the whole dictionary — because
/// the question is the same shape: nothing can be concluded until every word has been seen. What
/// differs is only what is kept. `best` and `longest` are still collected on the way past, because
/// the caller needs them when nothing kills and the scan that produced them is already paid for.
///
/// The ranking runs **only on lethal candidates**. It is a tiebreak among words that end the fight,
/// never a reason to prefer one that does not, and evaluating it on every word would spend the
/// board-walk in [`crate::pick::hazard_fall`] on the overwhelming majority that are irrelevant.
#[allow(clippy::too_many_arguments)]
pub fn ranked_kill(
    dict: &Dictionary,
    scorer: &Scorer,
    tiles: &[Tile],
    geometry: &Geometry,
    mods: &Modifiers,
    need: i64,
    picking: &crate::pick::Context,
    threads: usize,
) -> Outcome {
    let typist = Typist::new(tiles, geometry);
    let corner_count = geometry.corner_count();
    let words = dict.words();
    let threads = threads.max(1).min(words.len().max(1));

    // The winner and the rank it won with, together: comparing a candidate needs the incumbent's
    // rank, and recomputing it per comparison would walk the board again for nothing.
    let lethal: Mutex<Option<(Found, crate::pick::Rank)>> = Mutex::new(None);
    let best: Mutex<Option<Found>> = Mutex::new(None);
    let longest: Mutex<Option<Found>> = Mutex::new(None);
    let considered = std::sync::atomic::AtomicUsize::new(0);
    let cursor = std::sync::atomic::AtomicUsize::new(0);

    std::thread::scope(|scope| {
        for slice in 0..threads {
            let (lethal, best, longest, considered, cursor, typist) =
                (&lethal, &best, &longest, &considered, &cursor, &typist);
            scope.spawn(move || {
                let mut seen = 0usize;
                let mut local_lethal: Option<(Found, crate::pick::Rank)> = None;
                let mut local_best: Option<Found> = None;
                let mut local_longest: Option<Found> = None;
                loop {
                    let start = cursor.fetch_add(CLAIM, Ordering::Relaxed);
                    if start >= words.len() {
                        break;
                    }
                    for word in &words[start..(start + CLAIM).min(words.len())] {
                        seen += 1;
                        if mods.excluded.contains(word) {
                            continue;
                        }
                        let Some(typed) = typist.type_word(word) else { continue };
                        let consumed: Vec<Tile> =
                            typed.tiles.iter().map(|&i| tiles[i].clone()).collect();
                        let score = scorer.score_typed(
                            &consumed,
                            word.chars().count(),
                            mods.modifier_for(word, typed.corners_used, corner_count),
                        );
                        if score >= need {
                            let rank = crate::pick::rank(
                                tiles,
                                geometry,
                                &typed.tiles,
                                scorer,
                                &picking.target,
                                picking.prefs,
                            );
                            let take = match &local_lethal {
                                Some((_, cur)) => rank.better_than(cur),
                                None => true,
                            };
                            if take {
                                local_lethal =
                                    Some((Found { word: word.clone(), score, slice }, rank));
                            }
                        }
                        if local_best.as_ref().map(|b| score > b.score).unwrap_or(true) {
                            local_best = Some(Found { word: word.clone(), score, slice });
                        }
                        if local_longest
                            .as_ref()
                            .map(|b| word.chars().count() > b.word.chars().count())
                            .unwrap_or(true)
                        {
                            local_longest = Some(Found { word: word.clone(), score, slice });
                        }
                    }
                }
                considered.fetch_add(seen, Ordering::Relaxed);
                if let Some((found, rank)) = local_lethal {
                    let mut l = lethal.lock().unwrap();
                    // Same comparison across threads as within one. A thread's local winner is only
                    // the best of the slice it happened to claim.
                    if l.as_ref().map(|(_, cur)| rank.better_than(cur)).unwrap_or(true) {
                        *l = Some((found, rank));
                    }
                }
                if let Some(lb) = local_best {
                    let mut b = best.lock().unwrap();
                    if b.as_ref().map(|cur| lb.score > cur.score).unwrap_or(true) {
                        *b = Some(lb);
                    }
                }
                if let Some(ll) = local_longest {
                    let mut g = longest.lock().unwrap();
                    if g.as_ref()
                        .map(|cur| ll.word.chars().count() > cur.word.chars().count())
                        .unwrap_or(true)
                    {
                        *g = Some(ll);
                    }
                }
            });
        }
    });

    Outcome {
        lethal: lethal.into_inner().unwrap().map(|(f, _)| f),
        best: best.into_inner().unwrap(),
        longest: longest.into_inner().unwrap(),
        words_considered: considered.into_inner(),
    }
}

/// Exhaustive hardest-hit search, balanced by work-stealing.
pub fn max_damage(
    dict: &Dictionary,
    scorer: &Scorer,
    tiles: &[Tile],
    geometry: &Geometry,
    mods: &Modifiers,
    threads: usize,
) -> Outcome {
    let typist = Typist::new(tiles, geometry);
    let corner_count = geometry.corner_count();
    let words = dict.words();
    let threads = threads.max(1).min(words.len().max(1));

    let best: Mutex<Option<Found>> = Mutex::new(None);
    let longest: Mutex<Option<Found>> = Mutex::new(None);
    let considered = std::sync::atomic::AtomicUsize::new(0);
    let cursor = std::sync::atomic::AtomicUsize::new(0);

    std::thread::scope(|scope| {
        for slice in 0..threads {
            let (best, longest, considered, cursor, typist) =
                (&best, &longest, &considered, &cursor, &typist);
            scope.spawn(move || {
                let mut seen = 0usize;
                let mut local_best: Option<Found> = None;
                let mut local_longest: Option<Found> = None;
                loop {
                    // Claim the next block. No thread waits on another's slice being slow.
                    let start = cursor.fetch_add(CLAIM, Ordering::Relaxed);
                    if start >= words.len() {
                        break;
                    }
                    for word in &words[start..(start + CLAIM).min(words.len())] {
                        seen += 1;
                        if mods.excluded.contains(word) {
                            continue;
                        }
                        let Some(typed) = typist.type_word(word) else { continue };
                        let consumed: Vec<Tile> =
                            typed.tiles.iter().map(|&i| tiles[i].clone()).collect();
                        let score = scorer.score_typed(
                            &consumed,
                            word.chars().count(),
                            mods.modifier_for(word, typed.corners_used, corner_count),
                        );
                        if local_best.as_ref().map(|b| score > b.score).unwrap_or(true) {
                            local_best = Some(Found { word: word.clone(), score, slice });
                        }
                        if local_longest
                            .as_ref()
                            .map(|b| word.chars().count() > b.word.chars().count())
                            .unwrap_or(true)
                        {
                            local_longest = Some(Found { word: word.clone(), score, slice });
                        }
                    }
                }
                considered.fetch_add(seen, Ordering::Relaxed);
                if let Some(lb) = local_best {
                    let mut b = best.lock().unwrap();
                    if b.as_ref().map(|cur| lb.score > cur.score).unwrap_or(true) {
                        *b = Some(lb);
                    }
                }
                if let Some(ll) = local_longest {
                    let mut l = longest.lock().unwrap();
                    if l.as_ref()
                        .map(|cur| ll.word.chars().count() > cur.word.chars().count())
                        .unwrap_or(true)
                    {
                        *l = Some(ll);
                    }
                }
            });
        }
    });

    Outcome {
        // No `lethal`: the point of this mode is that "enough to kill" is not the target. The caller
        // plays `best`, which is lethal too whenever anything is.
        lethal: None,
        best: best.into_inner().unwrap(),
        longest: longest.into_inner().unwrap(),
        words_considered: considered.into_inner(),
    }
}

pub fn race_for_kill(
    dict: &Dictionary,
    scorer: &Scorer,
    tiles: &[Tile],
    geometry: &Geometry,
    mods: &Modifiers,
    need: i64,
    threads: usize,
) -> Outcome {
    race_for_band(dict, scorer, tiles, geometry, mods, need, None, threads)
}

/// First word scoring at least `need`, and under `below` when one is given.
///
/// The upper bound is what turns a kill race into a scare: the enemy has to survive to run away, so
/// a lethal word is not an acceptable answer even though it scores higher.
#[allow(clippy::too_many_arguments)]
pub fn race_for_band(
    dict: &Dictionary,
    scorer: &Scorer,
    tiles: &[Tile],
    geometry: &Geometry,
    mods: &Modifiers,
    need: i64,
    below: Option<i64>,
    threads: usize,
) -> Outcome {
    let typist = Typist::new(tiles, geometry);
    let corner_count = geometry.corner_count();
    let words = dict.words();
    let threads = threads.max(1).min(words.len().max(1));
    let chunk = words.len().div_ceil(threads);

    let stop = AtomicBool::new(false);
    let lethal: Mutex<Option<Found>> = Mutex::new(None);
    let best: Mutex<Option<Found>> = Mutex::new(None);
    let longest: Mutex<Option<Found>> = Mutex::new(None);
    let considered = std::sync::atomic::AtomicUsize::new(0);

    std::thread::scope(|scope| {
        for (slice, part) in words.chunks(chunk).enumerate() {
            let (stop, lethal, best, longest, considered, typist, mods) =
                (&stop, &lethal, &best, &longest, &considered, &typist, mods);
            scope.spawn(move || {
                let mut seen = 0usize;
                let mut local_best: Option<Found> = None;
                let mut local_longest: Option<Found> = None;
                for word in part {
                    // Checked periodically rather than every word: an atomic load per word would
                    // dominate the loop for no benefit at this granularity.
                    if seen % 512 == 0 && stop.load(Ordering::Relaxed) {
                        break;
                    }
                    seen += 1;
                    if mods.excluded.contains(word) {
                        continue;
                    }
                    // Not "are the letters there" but "does typing this work", which also tells us
                    // exactly which tiles it eats -- and so what it is worth.
                    let Some(typed) = typist.type_word(word) else { continue };
                    let consumed: Vec<Tile> =
                        typed.tiles.iter().map(|&i| tiles[i].clone()).collect();
                    let score = scorer.score_typed(
                        &consumed,
                        word.chars().count(),
                        mods.modifier_for(word, typed.corners_used, corner_count),
                    );
                    if score >= need && below.map(|b| score < b).unwrap_or(true) {
                        let found = Found { word: word.clone(), score, slice };
                        let mut l = lethal.lock().unwrap();
                        // First writer wins, so the result does not depend on thread scheduling
                        // any more than it has to.
                        if l.is_none() {
                            *l = Some(found);
                        }
                        stop.store(true, Ordering::Relaxed);
                        break;
                    }
                    if local_best.as_ref().map(|b| score > b.score).unwrap_or(true) {
                        local_best = Some(Found { word: word.clone(), score, slice });
                    }
                    if local_longest
                        .as_ref()
                        .map(|b| word.chars().count() > b.word.chars().count())
                        .unwrap_or(true)
                    {
                        local_longest = Some(Found { word: word.clone(), score, slice });
                    }
                }
                considered.fetch_add(seen, Ordering::Relaxed);
                if let Some(lb) = local_best {
                    let mut b = best.lock().unwrap();
                    if b.as_ref().map(|cur| lb.score > cur.score).unwrap_or(true) {
                        *b = Some(lb);
                    }
                }
                if let Some(ll) = local_longest {
                    let mut l = longest.lock().unwrap();
                    if l.as_ref()
                        .map(|cur| ll.word.chars().count() > cur.word.chars().count())
                        .unwrap_or(true)
                    {
                        *l = Some(ll);
                    }
                }
            });
        }
    });

    Outcome {
        lethal: lethal.into_inner().unwrap(),
        best: best.into_inner().unwrap(),
        longest: longest.into_inner().unwrap(),
        words_considered: considered.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn game_dir() -> PathBuf {
        PathBuf::from("../sternly-worded-adventures")
    }

    fn present() -> bool {
        game_dir().join("utils/dictionary.lua").is_file()
    }

    fn plain(letters: &str) -> Vec<Tile> {
        letters.chars().map(|c| Tile::plain(&c.to_string())).collect()
    }

    /// The actual level-0 crypt board, from `tests/fixtures/combatSaveData-crypt-l0.lua`.
    fn crypt_board() -> Vec<Tile> {
        plain("OYCAACTPORLIGAHJ")
    }

    fn race(scorer: &Scorer, dict: &Dictionary, tiles: &[Tile], mods: &Modifiers, need: i64) -> Outcome {
        race_for_kill(dict, scorer, tiles, &Geometry::default(), mods, need, 8)
    }

    #[test]
    fn refresh_only_when_nothing_good_and_nothing_lethal() {
        let lethal = Outcome {
            lethal: Some(Found { word: "CAT".into(), score: 9, slice: 0 }),
            best: Some(Found { word: "CAT".into(), score: 9, slice: 0 }),
            longest: Some(Found { word: "CAT".into(), score: 9, slice: 0 }),
            words_considered: 1,
        };
        // A kill ends the exchange, which beats any board-quality judgement.
        assert!(!lethal.should_refresh(8), "never refresh when a lethal word exists");

        let weak = Outcome {
            lethal: None,
            best: Some(Found { word: "CAT".into(), score: 4, slice: 0 }),
            longest: Some(Found { word: "CAT".into(), score: 4, slice: 0 }),
            words_considered: 1,
        };
        assert!(weak.should_refresh(8), "3 letters is under the 8 threshold");
        assert!(!weak.should_refresh(3), "3 letters meets a threshold of 3");

        let nothing = Outcome::default();
        assert!(nothing.should_refresh(8), "no playable word at all must refresh");
    }

    #[test]
    fn refresh_keys_off_length_not_score() {
        // A short word of gold tiles can outscore a long one of wood, so `best` is the wrong field
        // for a rule that is explicitly about length. JAZZ-like cases are exactly why these are
        // tracked separately.
        let out = Outcome {
            lethal: None,
            best: Some(Found { word: "JAY".into(), score: 20, slice: 0 }),
            longest: Some(Found { word: "OATMEALS".into(), score: 12, slice: 1 }),
            words_considered: 10,
        };
        assert!(!out.should_refresh(8), "an 8-letter word meets the threshold even if not top-scoring");
        assert_eq!(out.choice().map(|f| f.word.as_str()), Some("JAY"), "still PLAY the best scorer");
    }

    #[test]
    fn a_corner_resistant_enemy_takes_no_damage_from_a_corner_free_word() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // The failure the corner model exists to prevent. The skeleton shield boss carries
        // `resistCornerless` (`rpg/enemies/skeletons.lua:251`), and against it a word that touches no
        // corner does LITERALLY nothing -- nerf = 0/4. Reporting such a word as lethal would waste a
        // turn against a boss and could lose the run.
        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        let board = crypt_board();
        let geom = Geometry::default();

        let mut cornerless = Modifiers::none();
        cornerless.resist_cornerless = true;

        let out = race(&scorer, &dict, &board, &cornerless, 3);
        let played = out.choice().expect("something must be playable").word.clone();
        let typed = Typist::new(&board, &geom).type_word(&played).unwrap();
        assert!(
            typed.corners_used > 0,
            "{played} uses no corner, so it would do zero damage to this enemy"
        );
    }

    #[test]
    fn the_corner_nerf_changes_what_is_worth_playing() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // Same board, same enemy health, different answer -- which is the whole point. If the nerf
        // made no difference to the search, it would not be modelled, merely stored.
        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        let board = crypt_board();

        let mut cornerless = Modifiers::none();
        cornerless.resist_cornerless = true;

        // High enough that the search exhausts and reports its true best under each rule.
        let plain_best = race(&scorer, &dict, &board, &Modifiers::none(), 100_000).best.unwrap();
        let nerfed_best = race(&scorer, &dict, &board, &cornerless, 100_000).best.unwrap();
        assert!(
            nerfed_best.score < plain_best.score,
            "the nerf can only reduce: {} vs {}",
            nerfed_best.score,
            plain_best.score
        );
    }

    #[test]
    fn a_full_corner_sweep_is_not_nerfed_at_all() {
        // nerf = 4/4 = 1. The nerf must be a fraction of the corners USED, not a flat penalty --
        // otherwise a word that sweeps every corner would still be docked.
        let mut m = Modifiers::none();
        m.resist_cornerless = true;
        assert_eq!(m.modifier(4, 4), 1.0);
        assert_eq!(m.modifier(2, 4), 0.5);
        assert_eq!(m.modifier(0, 4), 0.0, "no corners means no damage");
        // And an enemy without the status is never scaled.
        assert_eq!(Modifiers::none().modifier(0, 4), 1.0);
    }

    #[test]
    fn the_halfling_can_never_reach_more_than_half_the_corners() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // `shortCharacter` -- "Can only reach the bottom 3 rows" -- sets tileboardUnselectableRow4
        // through Row10 (`items/classpassives.lua:25-33`). On the default board that locks row 4,
        // which holds the corners (1,4) and (4,4).
        //
        // The denominator does NOT shrink to match: `tileboard.getCornerCount` is `#corners`
        // (`tileboard.lua:117-120`) and never consults the locks. So against a corner-resistant enemy
        // a halfling is capped at 2/4 -- HALF DAMAGE, permanently, with no word able to do better.
        // That is a fact about the game, not about this code, and the search must reproduce it
        // rather than quietly assume the corners it can see are all the corners there are.
        let save = crate::game::save::parse(
            r#"return { passives = {}, rpg = { player = { gearFlags = {
                tileboardUnselectableRow4 = 1, tileboardUnselectableRow5 = 1,
            } } } }"#,
        )
        .unwrap();
        let (_, geometry) = Modifiers::from_save(&game_dir(), &save, 16).unwrap();
        assert_eq!(geometry.corner_count(), 4, "the denominator ignores the locks");

        let reachable = geometry
            .corner_indices()
            .into_iter()
            .filter(|&i| geometry.slot_selectable(i))
            .count();
        assert_eq!(reachable, 2, "only the row-1 corners are left");

        // And the search reflects it: the best word cannot exceed the half-damage ceiling.
        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        let board = crypt_board();
        let mut cornerless = Modifiers::none();
        cornerless.resist_cornerless = true;

        let best = race_for_kill(&dict, &scorer, &board, &geometry, &cornerless, 100_000, 8)
            .best
            .expect("something is playable");
        let typed = Typist::new(&board, &geometry).type_word(&best.word).unwrap();
        assert!(typed.corners_used <= 2, "{} used {} corners", best.word, typed.corners_used);
        assert!(cornerless.modifier(typed.corners_used, geometry.corner_count()) <= 0.5);
    }

    #[test]
    fn a_locked_row_is_not_the_same_as_a_smaller_board() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // The tiles in a locked row are still ON the board -- they are dumped, they count toward
        // totalTileCount, they just cannot be selected. Treating the halfling's board as 4x3 would
        // shift every subsequent tile's position by one and put the corners on the wrong letters.
        let save = crate::game::save::parse(
            r#"return { passives = {}, rpg = { player = { gearFlags = {
                tileboardUnselectableRow4 = 1,
            } } } }"#,
        )
        .unwrap();
        let (mods, geometry) = Modifiers::from_save(&game_dir(), &save, 16).unwrap();
        assert!(mods.problems.is_empty(), "a 16-tile dump still fits: {:?}", mods.problems);
        assert_eq!(geometry.total_tiles(), 16);
        assert_eq!(geometry.position(3), Some((1, 4)), "the locked tile keeps its place");
    }

    #[test]
    fn the_real_crypt_enemy_needs_no_modifiers() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // The captured fight, read the way the live loop will read it. If this ever starts reporting
        // problems, the measured board that every other test is built on is no longer understood.
        let save = crate::game::save::load(Path::new("tests/fixtures/combatSaveData-crypt-l0.lua"))
            .expect("fixture loads");
        let (mods, geometry) = Modifiers::from_save(&game_dir(), &save, 16).unwrap();
        assert!(mods.problems.is_empty(), "problems: {:?}", mods.problems);
        assert!(!mods.resist_cornerless, "Amorphous does not resist corners");
        assert!(mods.excluded.is_empty(), "no lexicon immunity");
        assert_eq!(geometry, Geometry::default());
    }

    #[test]
    fn an_immune_lexicon_removes_its_words_from_the_race() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // `nerf * 0` is a zero-damage turn, so an immune enemy's lexicon must not be searched at all.
        // This wires `crate::lexica` into the race, which previously computed exclusions nobody read.
        let save = crate::game::save::parse(
            r#"return { passives = {}, rpg = { enemy = { statusEffects = { lexiconBonusBone = 0 } } } }"#,
        )
        .unwrap();
        let (mods, _) = Modifiers::from_save(&game_dir(), &save, 16).unwrap();
        assert!(mods.excluded.contains("AITCHBONE"), "a bone word must be excluded");

        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        // A board that spells a bone word and little else.
        let board = plain("AITCHBONE");
        let out = race(&scorer, &dict, &board, &mods, 100_000);
        if let Some(best) = &out.best {
            assert_ne!(best.word, "AITCHBONE", "an immune word must never be chosen");
        }
    }

    #[test]
    fn an_overkill_enemy_switches_the_goal_to_max_damage() {
        // The mimic carries `statusEffects.overkillGold` (`rpg/enemies/mimic.lua:32`), and gold is
        // paid out equal to the excess damage (`rpgview.lua:1211-1214`). Taking the first killing
        // word against it is throwing the reward away.
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        let save = crate::game::save::parse(
            r#"return { passives = {}, rpg = { enemy = { statusEffects = { overkillGold = -1 } } } }"#,
        )
        .unwrap();
        let (mods, _) = Modifiers::from_save(&game_dir(), &save, 16).unwrap();
        assert!(mods.overkill_gold);
        assert_eq!(Goal::for_enemy(&mods, 3, 0, None, None), Goal::MaxDamage);
        // An ordinary enemy ranks its kills instead of racing for the first. Gold does not scale
        // with the excess here, so there is nothing to spend the extra damage on and the board the
        // word leaves behind is what the choice is for.
        assert_eq!(
            Goal::for_enemy(&Modifiers::none(), 3, 2, None, None),
            Goal::RankedKill { need: 6 }
        );
    }

    #[test]
    fn a_player_curse_grants_overkill_gold_against_every_enemy() {
        // `rpgview.lua:1211` accepts EITHER source, so the player's gear flag makes every fight an
        // overkill fight. Reading only the enemy would silently under-earn for the whole run.
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        let save = crate::game::save::parse(
            r#"return { passives = {}, rpg = { player = { gearFlags = { overkillGold = 1 } } } }"#,
        )
        .unwrap();
        let (mods, _) = Modifiers::from_save(&game_dir(), &save, 16).unwrap();
        assert!(mods.overkill_gold, "the player's curse must count");
    }

    #[test]
    fn max_damage_beats_the_first_kill_and_examines_everything() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        let board = crypt_board();
        let geom = Geometry::default();
        let mods = Modifiers::none();

        let kill = search(&dict, &scorer, &board, &geom, &mods, Goal::FirstKill { need: 3 }, &crate::pick::Context::default(), 8);
        let hardest = search(&dict, &scorer, &board, &geom, &mods, Goal::MaxDamage, &crate::pick::Context::default(), 8);

        let first = kill.lethal.expect("a 3-health enemy is killable here");
        let best = hardest.best.expect("max damage must report something");
        assert!(
            best.score > first.score,
            "hardest hit {} should beat the first kill {}",
            best.score,
            first.score
        );
        // Exhaustive by definition: there is no early stop to cut it short.
        assert_eq!(hardest.words_considered, dict.len());
        assert!(kill.words_considered < dict.len(), "the race must still stop early");
    }

    #[test]
    fn work_stealing_spreads_the_load_across_threads() {
        // The reason for the shared cursor: under MaxDamage nothing stops early, so the slowest
        // thread sets the wall time. With contiguous slices one thread grinds the long-word tail
        // while the rest idle. If only one slice ever reports a best, the split is not working.
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        let out = max_damage(&dict, &scorer, &crypt_board(), &Geometry::default(), &Modifiers::none(), 8);
        assert_eq!(out.words_considered, dict.len(), "every word must be examined exactly once");
        assert!(out.best.is_some());
    }

    #[test]
    fn one_and_two_letter_words_are_kept() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // The game has no minimum length -- "a" and "I" are ordinary words. An earlier 3-letter floor
        // silently dropped them.
        let dict = Dictionary::load(&game_dir()).unwrap();
        assert!(dict.words().iter().any(|w| w.chars().count() == 1), "no 1-letter words survived");
        assert!(dict.words().iter().any(|w| w.chars().count() == 2), "no 2-letter words survived");
    }

    /// The ranked goal kills, and picks a word the ranking actually endorses.
    ///
    /// Worth its own test because the wiring can be inert without anything noticing: a `RankedKill`
    /// that quietly behaved like `FirstKill` would still kill, still pass every other test, and never
    /// once consult [`crate::pick`]. What pins it is comparing the winner's rank against every other
    /// lethal word on the board — if the ranking is being applied, nothing beats the winner.
    #[test]
    fn the_ranked_goal_picks_a_kill_that_nothing_else_lethal_beats() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        let board = crypt_board();
        let geom = Geometry::default();
        let mods = Modifiers::none();
        let picking = crate::pick::Context {
            target: crate::letters::Weights::load(&game_dir()).unwrap().target(board.len()),
            prefs: crate::pick::Preferences::default(),
        };
        let need = 3;

        let out =
            search(&dict, &scorer, &board, &geom, &mods, Goal::RankedKill { need }, &picking, 8);
        let won = out.lethal.expect("a 3-health enemy must be killable from this board");

        let typist = Typist::new(&board, &geom);
        let rank_of = |word: &str| {
            let typed = typist.type_word(word)?;
            let consumed: Vec<Tile> = typed.tiles.iter().map(|&i| board[i].clone()).collect();
            let score = scorer.score_typed(
                &consumed,
                word.chars().count(),
                mods.modifier_for(word, typed.corners_used, geom.corner_count()),
            );
            (score >= need).then(|| {
                crate::pick::rank(
                    &board,
                    &geom,
                    &typed.tiles,
                    &scorer,
                    &picking.target,
                    picking.prefs,
                )
            })
        };
        let winner = rank_of(&won.word).expect("the winning word must itself be lethal");
        let mut rivals = 0usize;
        for word in dict.words() {
            if mods.excluded.contains(word) {
                continue;
            }
            let Some(other) = rank_of(word) else { continue };
            rivals += 1;
            assert!(
                !other.better_than(&winner),
                "{word} ranks above the chosen {}: {other:?} vs {winner:?}",
                won.word
            );
        }
        // Without this the assertion above is vacuous -- it passes trivially if nothing else kills.
        assert!(rivals > 1, "only {rivals} lethal word(s); the comparison proves nothing");
    }

    #[test]
    fn finds_a_lethal_word_on_the_real_crypt_board() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        // The real fight: "Amorphous", 3 health, armour absent.
        let out = race(&scorer, &dict, &crypt_board(), &Modifiers::none(), 3);
        assert!(!out.should_refresh(8), "a lethal word means no refresh");
        let found = out.lethal.clone().expect("a 3-health enemy must be killable from this board");
        assert!(found.score >= 3);
        let board = crypt_board();
        let geom = Geometry::default();
        assert!(
            Typist::new(&board, &geom).type_word(&found.word).is_some(),
            "{} cannot actually be typed",
            found.word
        );
    }

    #[test]
    fn a_race_stops_early_rather_than_scanning_everything() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        let out = race(&scorer, &dict, &crypt_board(), &Modifiers::none(), 3);
        // The whole point of racing: a trivially killable enemy must not cost a full dictionary scan.
        assert!(
            out.words_considered < dict.len(),
            "considered {} of {} words -- the race did not stop early",
            out.words_considered,
            dict.len()
        );
    }

    #[test]
    fn an_unkillable_enemy_yields_a_best_word_and_no_lethal() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        // Absurd health, so the search must exhaust and fall back rather than reporting nothing.
        let out = race(&scorer, &dict, &crypt_board(), &Modifiers::none(), 100_000);
        assert!(out.lethal.is_none());
        let best = out.best.expect("must still report the best word it found");
        assert!(best.score > 0);
        assert_eq!(out.words_considered, dict.len(), "no lethal word means a full scan");
    }

    #[test]
    fn the_dictionary_looks_like_a_dictionary() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        let dict = Dictionary::load(&game_dir()).unwrap();
        assert!(dict.len() > 100_000, "expected a large word list, got {}", dict.len());
        assert!(dict.words().iter().all(|w| w.chars().all(|c| c.is_ascii_uppercase())));
        assert!(dict.words().iter().all(|w| w.len() >= MIN_WORD_LEN));
        assert_eq!(MIN_WORD_LEN, 1, "the game enforces no minimum word length");
        // Sorted, which is what makes a contiguous slice an alphabetical range.
        assert!(dict.words().windows(2).all(|w| w[0] <= w[1]));
    }
}

#[cfg(test)]
mod scare_goal_tests {
    /// The whole reason the buffer is proportional rather than fixed.
    ///
    /// A `fear` enemy on a small maximum has almost no room between "hurt enough to run" and "dead".
    /// A flat +/-1 would invert the band and send us back to killing something we could have
    /// frightened off — which for a human is a Murder flag we did not have to take.
    #[test]
    fn a_frightenable_enemy_with_no_room_keeps_its_band_rather_than_losing_it() {
        // health 4 of a maximum 6: fleeing needs it at or below (6-1)/2 = 2, so 2 damage; 4 kills.
        // The raw band is 2..=3, one point of slack, and half of that is nothing to spend.
        let g = Goal::for_enemy(&feared(), 4, 0, Some(6), None);
        assert_eq!(g, Goal::Scare { need: 2, below: 4 }, "unbuffered, but still a scare");
    }

    #[test]
    fn a_band_with_two_to_spare_buys_a_buffer_on_each_side() {
        // health 6 of a maximum 6: flee at or below 2, so 4 damage; 6 kills. Raw band 4..=5 is one
        // wide -- still nothing. Widen the enemy and the buffer appears.
        assert_eq!(
            Goal::for_enemy(&feared(), 6, 0, Some(6), None),
            Goal::Scare { need: 4, below: 6 }
        );
        // health 12 of 12: flee at or below 5, so 7 damage; 12 kills. Raw band 7..=11 has four to
        // spare, so a full point goes to each edge.
        assert_eq!(
            Goal::for_enemy(&feared(), 12, 0, Some(12), None),
            Goal::Scare { need: 8, below: 11 }
        );
    }

    /// The buffer must never turn a survivable scare into a kill.
    #[test]
    fn the_buffered_ceiling_still_leaves_the_enemy_alive() {
        let g = Goal::for_enemy(&feared(), 12, 0, Some(12), None);
        let Goal::Scare { need, below } = g else { panic!("expected a scare, got {g:?}") };
        assert!(below <= 12, "damage below this must not reach the 12 that kills");
        assert!(need < below, "the band must not be empty");
        // The worst case inside the band still frightens it: 8 damage leaves 4, and 4*2 < 12.
        assert!(crate::flee::Nerve::Fear.would_leave(12 - need, 12));
    }

    use super::*;
    use crate::flee::Nerve;

    fn feared() -> Modifiers {
        Modifiers { nerve: Some(Nerve::Fear), ..Modifiers::none() }
    }

    #[test]
    fn a_fearful_enemy_is_scared_not_killed() {
        // A cultist at full health: 12 of 12, no armour. It leaves at 5 or below, so we need 7 and
        // must stay under 12.
        assert_eq!(
            Goal::for_enemy(&feared(), 12, 0, Some(12), None),
            Goal::Scare { need: 8, below: 11 }
        );
    }

    #[test]
    fn armour_is_added_to_both_ends() {
        // Armour absorbs first, so reaching the threshold costs the difference plus the armour, and
        // the lethal line moves out by the same amount.
        assert_eq!(
            Goal::for_enemy(&feared(), 12, 3, Some(12), None),
            Goal::Scare { need: 11, below: 14 }
        );
    }

    #[test]
    fn an_enemy_already_below_the_line_just_needs_a_non_lethal_word() {
        // It will run at the start of its own turn; our only job is not to kill it first.
        assert_eq!(
            Goal::for_enemy(&feared(), 4, 0, Some(12), None),
            Goal::Scare { need: 1, below: 3 }
        );
    }

    #[test]
    fn immobile_cannot_run_so_we_kill_it() {
        // rpgview.lua:1646 -- the flee branch requires `not currentEnemy.immobile`. Trying to scare
        // one would leave it alive and still attacking.
        let m = Modifiers { immobile: true, ..feared() };
        assert_eq!(Goal::for_enemy(&m, 12, 0, Some(12), None), Goal::RankedKill { need: 13 });
    }

    #[test]
    fn without_a_known_maximum_we_do_not_guess() {
        assert_eq!(Goal::for_enemy(&feared(), 12, 0, None, None), Goal::RankedKill { need: 13 });
    }

    #[test]
    fn overkill_gold_still_wants_the_corpse() {
        // Gold scales with the excess, so this fight is the exception.
        let m = Modifiers { overkill_gold: true, ..feared() };
        assert_eq!(Goal::for_enemy(&m, 12, 0, Some(12), None), Goal::MaxDamage);
    }

    #[test]
    fn a_one_health_maximum_offers_no_non_lethal_scare() {
        // leaves_at_or_below is None below 2 max health, so there is no room to hurt without killing.
        assert_eq!(Goal::for_enemy(&feared(), 1, 0, Some(1), None), Goal::RankedKill { need: 2 });
    }
}

#[cfg(test)]
mod rested_goal_tests {
    use super::*;
    use crate::rested::Vitals;

    fn player(current: i64, consumes_charge: bool) -> PlayerState {
        PlayerState {
            vitals: Vitals { current, max: 12 },
            heals: true,
            consumes_charge,
            bleeding: false,
        }
    }

    /// Enemy on 10 with no armour, so the plain kill needs 10.
    fn goal(p: Option<&PlayerState>) -> Goal {
        Goal::for_enemy(&Modifiers::none(), 10, 0, None, p)
    }

    #[test]
    fn a_scratch_kills_without_triggering_a_heal() {
        // Missing 2 of 12. floor(overkill/2) must stay at 0, so overkill of 0 or 1: damage 10 or 11.
        assert_eq!(goal(Some(&player(10, true))), Goal::Scare { need: 10, below: 12 });
    }

    #[test]
    fn a_bad_wound_buys_a_full_top_up() {
        // Missing 8, and the heal is half the overkill, so 16 of overkill are needed: damage 26.
        assert_eq!(goal(Some(&player(4, true))), Goal::FirstKill { need: 27 });
    }

    #[test]
    fn full_health_kills_normally() {
        assert_eq!(goal(Some(&player(12, true))), Goal::RankedKill { need: 11 });
    }

    #[test]
    fn a_free_heal_is_never_conserved() {
        // A gear flag grants the heal without spending anything (the decrement at :1204-1210 only
        // touches statusEffects), so there is no reason to avoid a scratch top-up.
        assert_eq!(goal(Some(&player(10, false))), Goal::RankedKill { need: 11 });
    }

    #[test]
    fn bleeding_kills_normally_because_no_heal_can_happen() {
        let p = PlayerState { bleeding: true, ..player(4, true) };
        assert_eq!(goal(Some(&p)), Goal::RankedKill { need: 11 });
    }

    #[test]
    fn a_cancelled_heal_kills_normally() {
        let p = PlayerState { heals: false, ..player(4, true) };
        assert_eq!(goal(Some(&p)), Goal::RankedKill { need: 11 });
    }

    #[test]
    fn scaring_takes_precedence_over_healing() {
        // We cannot heal off an enemy we deliberately leave alive -- and not murdering it is worth
        // more than a top-up.
        let m = Modifiers { nerve: Some(crate::flee::Nerve::Fear), ..Modifiers::none() };
        let p = player(4, true);
        assert_eq!(
            Goal::for_enemy(&m, 12, 0, Some(12), Some(&p)),
            Goal::Scare { need: 8, below: 11 }
        );
    }
}

#[cfg(test)]
mod lexicon_bonus_tests {
    use super::*;

    fn with_bonus(pairs: &[(&str, f64)]) -> Modifiers {
        Modifiers {
            bonuses: pairs
                .iter()
                .map(|(w, v)| (HashSet::from([w.to_string()]), *v))
                .collect(),
            ..Modifiers::none()
        }
    }

    #[test]
    fn a_bonus_lexicon_multiplies_the_word() {
        // A slime carries lexiconBonusSlime 1.5 (rpg/enemies/slimes.lua:90).
        let m = with_bonus(&[("OOZE", 1.5)]);
        assert_eq!(m.modifier_for("OOZE", 0, 0), 1.5);
        // A word outside the lexicon is untouched.
        assert_eq!(m.modifier_for("STONE", 0, 0), 1.0);
    }

    #[test]
    fn two_bonuses_stack_additively_not_multiplicatively() {
        // utils/words.lua:219-242 does `mult = mult + val - 1` per lexicon, so 1.5 and 1.5 give 2.0,
        // not 2.25. Some slimes carry fire AND ice (slimes.lua:294-295, 333-334).
        let m = with_bonus(&[("SLEET", 1.5), ("SLEET", 1.5)]);
        assert_eq!(m.modifier_for("SLEET", 0, 0), 2.0);
        // The live pairing: fire 1.2 with ice 1.5 on a word in both lexicons.
        let m = with_bonus(&[("EMBER", 1.2), ("EMBER", 1.5)]);
        assert!((m.modifier_for("EMBER", 0, 0) - 1.7).abs() < 1e-9);
    }

    #[test]
    fn the_corner_nerf_still_applies_on_top() {
        // resistCornerless scales by cornersUsed/cornerCount, and the bonus multiplies that -- both
        // are in play at once for a cornerless-resisting slime.
        let m = Modifiers { resist_cornerless: true, ..with_bonus(&[("OOZE", 2.0)]) };
        assert_eq!(m.modifier_for("OOZE", 2, 4), 1.0, "half the corners, doubled");
        assert_eq!(m.modifier_for("OOZE", 0, 4), 0.0, "no corners is still no damage");
    }

    #[test]
    fn under_scoring_is_what_makes_this_a_correctness_bug() {
        // The reason bonuses could no longer be ignored: Goal::Scare has an UPPER bound. A word
        // scored at 16 against a 1.5x enemy really lands 24, which clears `below` and kills the
        // enemy we meant to frighten.
        let m = with_bonus(&[("OOZE", 1.5)]);
        let raw = 16.0;
        assert_eq!(raw * m.modifier_for("OOZE", 0, 0), 24.0);
    }
}
