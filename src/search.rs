//! Finding a word to play against the enemy.
//!
//! **Only [`Goal::FirstKill`] is still a race.** Threads take contiguous alphabetical slices of the
//! dictionary and the first to find any lethal word wins; the rest stop the moment it does. That is
//! fine there because every lethal word ends the fight the same way, so which one is irrelevant.
//!
//! Every other goal scans the whole dictionary and ranks what it finds, because "irrelevant" turned
//! out to be false in general. Live 2026-08-10, a highwayman search with a ceiling stopped every
//! thread on the first word inside the band — decided by thread scheduling, not by merit — and the
//! word that happened to win spent a solid gold tile while cheaper words in the same band went
//! unexamined. [`race_for_band`] (used for [`Goal::Scare`]) and
//! [`ranked_kill`] (for [`Goal::RankedKill`]) both pay for a full scan so [`crate::pick::Rank`] can
//! choose between candidates that are all equally acceptable by score alone. [`Goal::MaxDamage`]
//! ([`max_damage`]) was never a race either — it needs the single hardest word on the board, which
//! cannot be known until every word has been tried.
//!
//! Contiguous slices are deliberately not a balanced partition — a slice whose initial letters are
//! absent from the board yields nothing at all. That is fine for [`Goal::FirstKill`] (any thread can
//! win) but not for a full scan, where an unbalanced split lets one thread grind the long-word tail
//! while the rest idle; those paths claim work in [`CLAIM`]-sized blocks from a shared cursor instead.
//!
//! ## What makes a word playable
//!
//! Not "does the board hold these letters" but "does typing this word work" — see [`crate::typist`].
//! The game's `textinput` is a specific greedy consumer that decides which tile each character
//! takes, and that choice sets both the tile state that scores the word and the corner count that
//! `resistCornerless` scales it by. Adjacency is not required unless the player carries
//! `wordRequirementAdjacent` gear (the letter-chaining curse), which the typist models: see
//! [`crate::typist`].
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
            return Err(crate::Error::Config(format!("no words parsed from {}", path.display())));
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
    /// True facts about the board that are not faults — see [`crate::geometry::Resolved::notes`].
    /// A ragged board against `resistCornerless` is the one that matters: the scores are right and
    /// the ceiling is simply lower than full.
    pub notes: Vec<String>,
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
    /// What each of [`Modifiers::bonuses`] is called, in the same order: the lexicon's name as its
    /// status key spells it, `lexiconBonusBone` → `Bone`. For showing a bonus, not for scoring.
    pub bonus_names: Vec<String>,
    /// The spells the player holds — see [`crate::spells`]. Filled from the save, and both Diggle
    /// and the advisor are scored as casting the best spell. `None` only in tests that want the
    /// plain attack alone.
    pub spells: Option<crate::spells::SpellBook>,
    /// This enemy cancels overkill healing outright (`rpgview.lua:1084`).
    pub overkill_no_heal: bool,
    /// This enemy turns overkill healing into gold instead (`:1085`), which is the curse described
    /// in `items/curses.lua:111`. Healing does not happen.
    pub overkill_heal_to_gold: bool,
    /// The player's gear modifiers, compiled once for this fight (see [`crate::gear`]).
    ///
    /// Kept here rather than passed alongside because every scoring site already has a `Modifiers`,
    /// and a term that must be applied at all of them should not be one a new call site can forget.
    pub plan: crate::gear::Plan,
    /// Tallies over the words already played this fight, for the Raven and Whale idols. Both halves
    /// are `None` unless the corresponding idol is worn.
    pub fight: crate::gear::FightFacts,
    /// Only the auxiliary dictionaries the plan will ask about — usually none. Carried as word sets
    /// rather than a `Lexica` so `Modifiers` stays cheap to clone.
    pub dicts: Vec<(crate::gear::Dict, HashSet<String>)>,
    /// The gear that queues gems on submission, for the ranking — see [`crate::pick::GemQueue`].
    /// Built here because this is where the dictionaries are loaded.
    pub gems: crate::pick::GemQueue,
    /// The gear that changes damage *after* the word is scored — see [`crate::gear::weapon`].
    pub weapon: crate::gear::Weapon,
    /// Health the enemy loses to the status already on it, at the start of its next turn.
    ///
    /// Subtracted from its health when choosing a goal, because the question every goal is really
    /// asking is whether the enemy gets to act: `enemyCanHit` is gated on the same post-status
    /// health the game uses for `attackEstimatedToCauseEnemyDeath` (`rpgview.lua:1044-1052`).
    pub enemy_status_tick: i64,
    /// **The enemy's answering attack cannot land this turn.** Only the halfling's `immuneTurnMod2`
    /// does this, and only on the turns [`crate::parity`] names — read from the save's own
    /// `turnNumber`, which is not the number the console prints.
    ///
    /// The dev, 2026-08-15: *"make sure the halfling also only fears enemies on even turns"*, in the
    /// console's numbering — which is the same statement as this one, see [`crate::parity`].
    ///
    /// **It does not change a word by itself.** Dropping the scare floor to zero on these turns
    /// would make play worse, not better — see `Goal::for_enemy`'s scare floor. What it feeds is
    /// [`Modifiers::stall`], which [`crate::turn::plan`] sets from it.
    pub player_dodges: bool,
    /// **This turn ends nothing: the enemy is neither killed nor scared off if it can be helped.**
    ///
    /// Set by [`crate::turn::plan`], and only when every one of these holds:
    ///
    /// - the caller asked for it ([`crate::turn::Options::stall_on_free_turns`]);
    /// - the enemy cannot land a hit this turn ([`Modifiers::player_dodges`]), so keeping it alive
    ///   costs nothing;
    /// - the status already on it does not finish it anyway (post-status health above zero).
    ///
    /// What it changes:
    ///
    /// - **The enemy is held** ([`Goal::Hold`]): ending it now spends the turn it could not hurt us
    ///   on its leaving, and the next enemy then acts on a turn that can. So the hardest hit that
    ///   keeps it here comes first, then a word that deals nothing, and a scare or a kill only when
    ///   the board has nothing else.
    /// - **An enemy already past its flee threshold** runs at the start of its own turn whatever we play
    ///   (`rpgview.lua:1691-1695`), so all that is left to decide is not to murder it. Where any hit
    ///   kills, a word that deals nothing ([`Tier::Stall`]) comes before one that kills
    ///   ([`Tier::Overshoot`]); without the stall those come the other way round, which is the
    ///   dev's recorded call for a turn that is *not* free — see `fight::nothing_left_to_lower`.
    ///
    /// The turn is free because `enemyCanHit` reads `getPlayerStatus'immune'` (`:1065`).
    pub stall: bool,
    /// What one stack of toxin a word applies costs this enemy: 1, 2 while `toxinWeak`, 0 while
    /// `toxinImmune` (`baseStatusDamage`, `rpgview.lua:1262-1284`). See [`Modifiers::toxin`].
    pub toxin_scale: i64,
    /// What a word's own ice costs this enemy before it acts: 1, 2 while `iceWeak`, and 0 while
    /// `iceImmune` **or already iced** — ice ticks a flat amount however many stacks there are
    /// (`baseStatusDamage`, `rpgview.lua:1303-1307`), and ice already on it is in
    /// [`Modifiers::enemy_status_tick`]. See [`Modifiers::afflictions`].
    pub ice_tick: i64,
    /// The same for burn: `ceil(maxHealth / 16)`, or `/ 8` while `burnWeak` (`rpgview.lua:1300-1302`),
    /// and 0 while `burnImmune` or already burning. 1 when the enemy's maximum health is not in the
    /// save, the cautious guess [`crate::gear::status_tick`] makes too.
    pub burn_tick: i64,
    /// The schools' power, for a word its own tiles make a spell of — see
    /// [`crate::spells::tile_school`]. `None` while `noCastSpell` is held, which keeps every gem
    /// tile inactive (`wordboard.lua:103-118`): no boost, and no ice or burn either.
    pub tile_spells: Option<crate::spells::Power>,
    /// What stands between us and the kill, for the one gear that changes which of it a word must
    /// get through: the rapier. `None` until the turn's plan fills it in ([`crate::turn::plan`]), and
    /// then no word is treated as piercing — see [`Modifiers::reach`].
    pub defence: Option<Defence>,
}

/// The enemy's health and armour, as the goals measure them: health after the status already on it
/// ticks (see [`crate::turn::plan`]'s `deciding_health`), and the armour from the save.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Defence {
    pub health: i64,
    pub armour: i64,
}

/// The statuses a word applies, as the damage each costs the enemy before it acts — see
/// [`Modifiers::afflictions`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Afflictions {
    pub toxin: i64,
    pub ice: i64,
    pub burn: i64,
}

impl Afflictions {
    pub fn total(&self) -> i64 {
        self.toxin + self.ice + self.burn
    }
}

impl Modifiers {
    /// Reads the enemy's statuses and the board's shape out of a save table.
    ///
    /// `tile_count` is the board dump's length, used to check the derived shape fits.
    pub fn from_save(
        game_dir: &Path, save: &crate::game::save::Table, tile_count: usize,
    ) -> Result<(Self, Geometry), crate::Error> {
        let statuses = crate::lexica::Lexica::statuses_from_save(save);
        let lexica = crate::lexica::Lexica::load(game_dir)?;
        let resolved = Geometry::from_save(save, tile_count);

        let notes = resolved.notes.clone();
        let mut problems = resolved.problems.clone();
        problems.extend(lexica.problems().iter().cloned());
        problems.extend(lexica.unmodelled(&statuses));

        // Either source grants it, so both are checked (`rpgview.lua:1211`).
        let overkill_gold = statuses.contains_key("overkillGold")
            || save.path("rpg.player.gearFlags.overkillGold").is_some();

        let plan = crate::gear::Gear::from_save(save).compile();
        // A word bonus we cannot evaluate makes every score a lower bound, which is safe for a kill
        // and unsafe for a scare. Reported as a problem so it shows in the turn log rather than
        // being discovered from a murder warning, which is how it was discovered the first time.
        for flag in plan.unevaluable() {
            problems.push(format!("gear {flag} affects word score and is not modelled"));
        }
        let fight = crate::gear::FightFacts::from_save(
            save,
            plan.wants_alliteration(),
            plan.wants_repeats(),
        );
        let dicts = plan
            .dicts_needed()
            .into_iter()
            .filter_map(|d| lexica.words_for(d.key()).map(|w| (d, w.clone())))
            .collect();
        let weapon = crate::gear::Weapon::from_save(&crate::gear::Gear::from_save(save), save);
        // The spells held. Diggle is scored as casting the best of them — the user's call,
        // 2026-11-07, with the button press stubbed (see `crate::spells`) — and as the game casts a
        // lone one for it. The advisor replaces this book with one carrying the user's config.
        let undead = crate::advisor::bestiary::Bestiary::load(game_dir)
            .undead(save.str_at("rpg.enemy.name").unwrap_or(""));
        let spells = crate::spells::SpellBook::from_save(save, &lexica, undead);
        // The status a word *applies* is not in this number — it is per word, and the band the
        // search filters on is per word too. `bonusBastard` trading 6 damage for 3 bleed and 3 toxin
        // is the case that matters, and it under-rates the turn's total by about 4.
        let enemy_status_tick =
            crate::gear::status_tick(&statuses, crate::gear::WeaponBonus::default(), None);
        let toxin_scale =
            match (statuses.contains_key("toxinImmune"), statuses.contains_key("toxinWeak")) {
                (true, _) => 0,
                (false, true) => 2,
                (false, false) => 1,
            };
        // A flat tick only adds when nothing of the kind is on the enemy yet.
        let fresh = |s: &str| {
            !statuses.contains_key(&format!("{s}Immune"))
                && statuses.get(s).is_none_or(|v| *v <= 0.0)
        };
        let ice_tick = match (fresh("ice"), statuses.contains_key("iceWeak")) {
            (false, _) => 0,
            (true, true) => 2,
            (true, false) => 1,
        };
        let burn_tick = if fresh("burn") {
            let divisor = if statuses.contains_key("burnWeak") { 8.0 } else { 16.0 };
            save.int_at("rpg.enemy.maxHealth")
                .map(|m| (m as f64 / divisor).ceil() as i64)
                .unwrap_or(1)
        } else {
            0
        };
        let tile_spells = save
            .path("rpg.player.gearFlags.noCastSpell")
            .is_none()
            .then(|| crate::spells::Power::from_save(save));
        // One line for the whole deferred status model, not one per piece of it.
        if let Some(note) = crate::gear::weapon::deferred_status_model(&statuses, &weapon) {
            problems.push(note);
        }

        // **A turn the enemy cannot land on.** Absent the turn number this reads as false, which is
        // the safe direction: it only ever restores the ordinary floor.
        let player_dodges = crate::parity::save_turn(save)
            .is_some_and(|t| crate::parity::dodges(crate::parity::has_flag(save), t));

        Ok((
            Modifiers {
                gems: crate::pick::GemQueue::from_save(save, &lexica),
                excluded: lexica.excluded_words(&statuses),
                resist_cornerless: statuses.contains_key("resistCornerless"),
                overkill_gold,
                problems,
                notes,
                // **Never on the undead.** Skeletons carry no nerve of their own
                // (`rpg/enemies/skeletons.lua`); the `terror` or `fear` one has came from Turn
                // undead (`rpg/effects/spell/TurnUndead.lua:38-41`), and a skeleton is always
                // killed, not scared off. An enemy whose nature cannot be told keeps its nerve.
                //
                // Not every fight needs the skeleton dead to proceed — defending a village from the
                // corruption, or a forest path node, can be got past by scaring it off. Those cases
                // are out of scope for now; this always kills.
                nerve: ["fear", "terror", "caution"]
                    .iter()
                    .find(|k| statuses.contains_key(**k))
                    .and_then(|k| crate::flee::Nerve::from_status(k))
                    .filter(|_| {
                        let name = save.str_at("rpg.enemy.name").unwrap_or("");
                        crate::advisor::bestiary::Bestiary::load(game_dir).undead(name)
                            != Some(true)
                    }),
                immobile: save.path("rpg.enemy.immobile").is_some(),
                bonuses: lexica.bonus_sets(&statuses),
                bonus_names: lexica.bonus_names(&statuses),
                spells: Some(spells),
                overkill_no_heal: statuses.contains_key("overkillNoHeal"),
                overkill_heal_to_gold: statuses.contains_key("overkillHealToGold"),
                plan,
                fight,
                dicts,
                weapon,
                enemy_status_tick,
                player_dodges,
                // Decided per turn, from more than the save — see `crate::turn::plan`.
                stall: false,
                toxin_scale,
                ice_tick,
                burn_tick,
                tile_spells,
                defence: None,
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
            notes: Vec::new(),
            nerve: None,
            immobile: false,
            bonuses: Vec::new(),
            bonus_names: Vec::new(),
            spells: None,
            overkill_no_heal: false,
            overkill_heal_to_gold: false,
            plan: crate::gear::Plan::default(),
            fight: crate::gear::FightFacts::default(),
            dicts: Vec::new(),
            gems: Default::default(),
            weapon: crate::gear::Weapon::default(),
            enemy_status_tick: 0,
            player_dodges: false,
            stall: false,
            toxin_scale: 1,
            ice_tick: 1,
            burn_tick: 1,
            tile_spells: Some(crate::spells::Power::default()),
            defence: None,
        }
    }

    /// What this word's statuses cost the enemy at the start of its next turn, before it acts:
    /// [`Modifiers::toxin`], and the ice and burn a gem tile starting the word inflicts.
    ///
    /// - **A Chrysocolla first** (`gemice`) inflicts 4 ice per gem in the word, **a Corundum first**
    ///   (`gemfire`) 3 burn per gem (`rpg/effects/material/gemice.lua:11-13`, `gemfire.lua:7-9`).
    ///   Each status ticks a flat amount whatever the stacks — [`Modifiers::ice_tick`],
    ///   [`Modifiers::burn_tick`] — so the count of gems decides only that there is some.
    /// - Paid like toxin, through `getStatusEffectHealthDeltasFor` before the enemy acts
    ///   (`rpgview.lua:1328-1341,1610-1612`), so it is damage to every goal and earns nothing a
    ///   blow would: no overkill gold, no Well-Rested heal, no wood-kill gear.
    ///
    /// The user's report, 2026-11-06: against a chest, `FIRWOOD` on a Chrysocolla F did more than
    /// the listed `DRIFTWOOD`, because the ice and the arcane boost (see [`scored_cast`]) were not
    /// counted.
    pub fn afflictions(&self, scorer: &Scorer, word: &str, consumed: &[Tile]) -> Afflictions {
        let first =
            match (&self.tile_spells, consumed.first().and_then(|t| scorer.material_name(t))) {
                (Some(_), Some(m)) => m,
                _ => "",
            };
        Afflictions {
            toxin: self.toxin(scorer, word, consumed),
            ice: if first == "gemice" { self.ice_tick } else { 0 },
            burn: if first == "gemfire" { self.burn_tick } else { 0 },
        }
    }

    /// [`Modifiers::afflictions`], added up: the part of a word's damage that lands after the blow.
    pub fn status_damage(&self, scorer: &Scorer, word: &str, consumed: &[Tile]) -> i64 {
        self.afflictions(scorer, word, consumed).total()
    }

    /// The toxin damage this word adds, as health the enemy loses at the start of its next turn.
    ///
    /// Two sources, the user's request, 2026-11-04: *serrated tiles (and other gear items) can add
    /// toxin to the enemy, and the number of toxin stacks should add to the damage for ranking.*
    ///
    /// - **A toxin tile that starts the word**: `bronzetoxin`, `silvertoxin`, `goldtoxin` and
    ///   `electrumtoxin` are `active` only as `wordTiles[1]`, and then add 3 toxin per **serrated**
    ///   tile in the word, bleed and toxin kinds alike (`countEachMaterialWithCat(wordTiles,
    ///   'serrated')*3`, `rpg/effects/material/bronzetoxin.lua:2-8`).
    /// - **The weapon flags**, [`crate::gear::WeaponBonus::toxin`]: the Bastard sword, the CEI
    ///   dagger and `toxinAttack` (`rpgview.lua:868-912`).
    ///
    /// Toxin damage is the stack count (`baseStatusDamage`, `rpgview.lua:1260-1273`), paid at the
    /// start of the enemy's turn **before it acts** (`:1610-1612`) and routed to health past armour
    /// (`:1297-1298`). So it is damage in every sense a goal asks about: it kills before the enemy's
    /// attack, it counts toward a flee threshold, and on a free turn it can end the enemy just as a
    /// kill would — which is why [`scored`] folds it in rather than ranking it on the side. Counting
    /// it against health plus armour is cautious, never optimistic: it skips the armour, so it can
    /// only do more than this says. What it does not earn is anything paid on the word's own blow:
    /// overkill gold, the Well-Rested heal and the wood-kill gear.
    pub fn toxin(&self, scorer: &Scorer, word: &str, consumed: &[Tile]) -> i64 {
        let serrated = |m: &str| m.ends_with("toxin") || m.ends_with("bleed");
        let tile = match consumed.first().and_then(|t| scorer.material_name(t)) {
            Some(m) if m.ends_with("toxin") => {
                let serrated_tiles =
                    consumed.iter().filter(|t| scorer.material_name(t).is_some_and(serrated));
                3 * serrated_tiles.count() as i64
            }
            _ => 0,
        };
        (tile + self.weapon.bonus_for(word, consumed).toxin) * self.toxin_scale
    }

    /// This word's damage ignores the enemy's armour: the rapier is held, the word starts and ends
    /// with the same letter ([`crate::gear::Weapon::pierces`]), and there is armour to ignore.
    pub fn pierces(&self, word: &str) -> bool {
        self.defence.is_some_and(|d| d.armour > 0) && self.weapon.pierces(word)
    }

    /// `damage` as the goals measure it: against health **plus** armour.
    ///
    /// A piercing word's damage goes straight to health (`rpgview.lua:1022`, `:1200-1201`), so it
    /// counts as its damage plus the armour it skips. This is exact, not an approximation, because
    /// every threshold a goal sets is measured on that same sum, and adding the armour to both sides
    /// changes nothing:
    ///
    /// - **the kill**: `damage >= health` is `damage + armour >= health + armour`;
    /// - **a scare's floor**, `(health - top) + armour` ([`Goal::for_enemy`]), and its ceiling,
    ///   `health + armour`, the same way;
    /// - **the Well-Rested heal**: overkill is `damage - (health + effectiveArmour)`
    ///   (`rpgview.lua:1207`), which is `reach - (health + armour)` either way;
    /// - **overkill gold**, which pays that same overkill.
    ///
    /// The damage shown to a person stays the real one; this is only for comparing with a goal.
    /// It is **not** an ordering of the words that miss — see [`Modifiers::hits_harder`].
    pub fn reach(&self, word: &str, damage: i64) -> i64 {
        match self.defence {
            Some(d) if self.pierces(word) => damage + d.armour,
            _ => damage,
        }
    }

    /// Whether `a` (word, damage) is a harder hit than `b`: the ordering every "hardest word"
    /// fallback uses.
    ///
    /// **A kill beats any non-kill**, and kills compare by [`Modifiers::reach`], which is how far
    /// past lethal they go — what overkill gold pays on.
    ///
    /// **Among words that do not kill, the larger share of what is left wins** — the user's rule,
    /// 2026-11-03. A rapier word removes `damage / health`, any other `damage / (health + armour)`.
    /// Ranking by reach instead would put a three-letter rapier word for 5 (worth 59 in reach)
    /// above a long word that strips 53 of 54 armour, and that is the wrong way round: the long
    /// word leaves almost any word able to finish the job next turn.
    ///
    /// The rule assumes the next word will be the same kind as this one, so close calls lean a
    /// little towards non-rapier words: 17 to health out of 42 (0.40) loses to 43 of 96 (0.45),
    /// though it leaves a rapier word far less to do. Accepted for being explainable in a sentence.
    ///
    /// Without the rapier, or without armour, every word has the same denominator and this is
    /// plain damage, as it was before.
    pub fn hits_harder(&self, a: (&str, i64), b: (&str, i64)) -> std::cmp::Ordering {
        let (ra, rb) = (self.reach(a.0, a.1), self.reach(b.0, b.1));
        let Some(d) = self.defence else { return ra.cmp(&rb) };
        let lethal = d.health + d.armour;
        match (ra >= lethal, rb >= lethal) {
            (true, true) => ra.cmp(&rb),
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (false, false) => {
                let share = |word: &str, damage: i64| -> (i128, i128) {
                    let of = if self.pierces(word) { d.health } else { lethal };
                    (damage as i128, of.max(1) as i128)
                };
                let ((na, da), (nb, db)) = (share(a.0, a.1), share(b.0, b.1));
                (na * db).cmp(&(nb * da))
            }
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
        self.modifier_for_base(word, corners_used, corner_count, 1.0)
    }

    /// The same, starting from the multiplier the player's gear has already accumulated.
    ///
    /// **Not the gear multiplier times the lexicon one.** `words.modifiers` hands its accumulated
    /// `mult` *into* `words.getWordBonusModifier` (`utils/words.lua:287`), which then does
    /// `mult = mult+val-1` per matching lexicon. So gear and lexicons add on one accumulator and
    /// only the corner nerf multiplies. With gear at 1.25 and one 1.5x lexicon the game gets 1.75,
    /// while multiplying the two factors separately would get 1.875 — an over-estimate, which is
    /// the direction that types a word expecting a kill and does not get one.
    pub fn modifier_for_base(
        &self, word: &str, corners_used: usize, corner_count: usize, base: f64,
    ) -> f64 {
        let mut mult = base;
        for (words, val) in &self.bonuses {
            if words.contains(word) {
                mult += val - 1.0;
            }
        }
        self.modifier(corners_used, corner_count) * mult
    }
}

/// One word's damage, with everything that modifies it.
///
/// The single place the three sources are combined, because they have to be combined the same way
/// every time and there are four scoring sites. Getting the *order* right is the point:
///
/// 1. the player's gear contributes `preAdd`, a share of `mult`, and `postAdd` ([`crate::gear`]);
/// 2. the gear multiplier is the **starting value** for the enemy's lexicon accumulator, not a
///    separate factor — [`Modifiers::modifier_for_base`];
/// 3. `preAdd` joins the tile sum before the multiplier and the length scale, `postAdd` lands after
///    the rounding — [`Scorer::score_typed_with`].
pub fn scored(
    scorer: &Scorer, mods: &Modifiers, word: &str, consumed: &[Tile],
    typed: Option<&crate::typist::Typed>, corners_used: usize, corner_count: usize,
) -> i64 {
    scored_cast(scorer, mods, word, consumed, typed, corners_used, corner_count).0
}

/// [`scored`], and the spell to cast for that damage — `None` for the plain attack.
///
/// A held spell is an alternative to the plain attack, chosen on the cast buttons
/// (`wordboard.lua:483-487`), so the damage is the better of the two
/// ([`crate::spells::SpellBook::best_cast`]). It applies to the word's score before the weapon
/// layer: the spell is handed `cache.score` (`wordboard.lua:603`) and deals it through
/// `rpgview.dealDamage`, where the weapon bonus is added (`rpgview.lua:868-912`).
pub fn scored_cast(
    scorer: &Scorer, mods: &Modifiers, word: &str, consumed: &[Tile],
    typed: Option<&crate::typist::Typed>, corners_used: usize, corner_count: usize,
) -> (i64, Option<&'static str>) {
    let facts = crate::gear::Facts::new(word, consumed, typed, &mods.dicts);
    let adjust = mods.plan.apply(&facts, &mods.fight);
    let modifier = mods.modifier_for_base(word, corners_used, corner_count, adjust.mult);
    let score = scorer.score_typed_with(
        consumed,
        // Letters, not tiles, and `len()` rather than a scan: the dictionary is ASCII A-Z
        // (`search.rs:67-68`), so the byte length is the letter count.
        word.len(),
        modifier,
        adjust.pre_add,
        adjust.post_add,
    );
    // The weapon layer, which is the only gear that can make us over-estimate: `bonusBastard`
    // removes 6 from a seven-letter word (`rpgview.lua:871-876`). Clamped at zero because the game
    // takes `math.max(0, damageByPlayer-effectiveArmour)` downstream and a negative here would
    // otherwise read as healing.
    // **A plain attack its own tiles make a spell of** is boosted by that school's power
    // (`wordboard.lua:611-612`, and `rpgview.lua:1021-1022` for the estimate): a Chrysocolla
    // starting the word, with Adept, is 1.5x. A held spell cast instead is boosted only as itself
    // (`spell and not TrueTrueSpell`), so it is offered the unboosted score, and the plain attack
    // keeps a tie as it always has.
    let plain = match (&mods.tile_spells, crate::spells::tile_school(scorer, consumed)) {
        (Some(power), Some(school)) => power.boost(score, school),
        _ => score,
    };
    // What the word does: the plain attack, or a spell — chosen, or cast for us when it is the
    // only one ([`crate::spells::SpellBook::attack`]).
    let (score, cast) = match &mods.spells {
        Some(book) => book.attack(word, &tile_letters(word, consumed), score, plain),
        None => (plain, None),
    };
    let direct = (score + mods.weapon.bonus_for(word, consumed).damage_delta).max(0);
    // The statuses it applies, which the enemy pays before it acts: see
    // [`Modifiers::afflictions`].
    (direct + mods.status_damage(scorer, word, consumed), cast)
}

/// Each tile's letters as typed, in word order: a ligature's run of letters, or the one typed into a
/// wildcard — what Four heal reads off `wordTiles` (`rpg/effects/spell/Heal4.lua:17-34`).
fn tile_letters(word: &str, consumed: &[Tile]) -> Vec<String> {
    let mut at = 0;
    consumed
        .iter()
        .map(|t| {
            let n = if t.letter == "." { 1 } else { t.letter.len().max(1) };
            let s = word.get(at..at + n).unwrap_or_default().to_string();
            at += n;
            s
        })
        .collect()
}

/// A word the search found worth reporting.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub word: String,
    pub score: i64,
    /// Which dictionary slice found it — useful for seeing whether the split is pulling its weight.
    pub slice: usize,
    /// The tiles to click for it, which is no longer simply what typing the word would take: see
    /// [`crate::typist::Typist::placements`]. Carried so the player plays the placement that was
    /// scored rather than re-deriving one.
    pub typed: crate::typist::Typed,
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
    /// Hardest hit seen, for when nothing is lethal — by [`Modifiers::hits_harder`], which is plain
    /// damage unless the rapier is held.
    pub best: Option<Found>,
    /// Lowest-scoring word seen. The **last** resort for a band goal, behind [`Outcome::under`].
    ///
    /// A goal with a ceiling cannot fall back on [`Outcome::best`], which may be far over it. Where
    /// anything at all sits under the ceiling, `under` is the answer — see its note. This field is
    /// for the case where *nothing* does, and every word available overshoots.
    ///
    /// Live, 2026-08-09: a murder warning backed the band down to `need 0, below 1`, which no word
    /// can satisfy because no word scores zero. `choice` fell through to `best` and played the
    /// hardest-hitting word on the board, which raised the warning again — thirty times, until the
    /// run was stopped by hand. When every option breaks the ceiling, the smallest hit is the least
    /// bad of them.
    pub least: Option<Found>,
    /// Hardest hit that still stays **under a band's ceiling**, for when nothing lands inside it.
    ///
    /// Only collected by [`race_for_band`], and only when a ceiling was given — with no ceiling
    /// there is nothing to stay under and [`Outcome::best`] is already the answer.
    ///
    /// ## Why this is not just `best`, and not `least` either
    ///
    /// A band goal that misses its band is choosing among the words it *may* play, and both existing
    /// fields answer a different question. `best` is the hardest word on the board and may be well
    /// over the ceiling — for a scare that means killing the thing we meant to frighten off, which
    /// for a human is murder. `least` is the gentlest word on the board, which is safe and barely
    /// does anything.
    ///
    /// The words below the ceiling are all safe, so the only reason to pick a small one is if
    /// smallness were itself worth something, and it is not: `need` tracks the enemy's health, so
    /// **every point of damage lowers the band toward what the board can actually reach**. Chipping
    /// with the hardest safe word is how an unreachable scare becomes reachable, and it costs
    /// nothing the gentle word saves.
    ///
    /// Note that when the band is empty, "below the ceiling" and "below the floor" describe the same
    /// set of words — anything in between would have been in the band. So this single field replaces
    /// what used to be a two-way branch on `best.score < need`, and does strictly better in the case
    /// that branch never covered: hard words that overshoot, with softer safe ones available.
    pub under: Option<Found>,
    /// A word that deals **no damage at all** and stays under a band's ceiling — the word to spend a
    /// free turn on. See [`Modifiers::stall`].
    ///
    /// Only collected by [`race_for_band`], and only when `mods.stall` is set, so on every other
    /// turn it is `None` and [`Outcome::choice_for`] behaves exactly as it did before it existed.
    /// Among several, the one [`crate::pick::Rank`] likes best, since what the word costs the board
    /// is the only thing left to tell them apart.
    pub nothing: Option<Found>,
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
    ///
    /// Only correct for goals with **no upper bound**. See [`Outcome::choice_for`] — a band goal
    /// that misses its band must not fall back on the hardest hit available.
    pub fn choice(&self) -> Option<&Found> {
        self.lethal.as_ref().or(self.best.as_ref())
    }

    /// What to play given the goal that was searched for.
    ///
    /// The fallback direction is a property of the goal, not of the search, and reading it off the
    /// goal is what keeps the two from disagreeing.
    ///
    /// [`Goal::Scare`] is the one goal with a ceiling, and that ceiling is a **constraint**:
    /// overshooting kills something we meant to frighten off, so the fallback must stay under it:
    /// [`Outcome::under`]. Every other goal falls back to the hardest hit.
    ///
    /// ## The rule, and the two runs that paid for it
    ///
    /// **Play the hardest word that stays under the ceiling.** One rule, and it is the dev's, given
    /// 2026-08-14 after watching the second of these:
    ///
    /// * **Nothing reaches the floor.** 2026-08-10, a cultist at 127+33: the band was `98..=158` and
    ///   the best word on the board did 29. Seven turns of `SE`, `YA`, `PI`, `NA`, `EE`, `IF`, `GI`
    ///   — one point each — while the board eroded from sixteen tiles to nine and the run died.
    /// * **Everything in reach overshoots.** 2026-08-14, a `4..=5` band on a 6hp cultist: nothing
    ///   landed inside it, the hardest word would have killed her, and the fallback played `AA` for
    ///   1. That one worked, because the enemy was small enough that a single point of chip brought
    ///   the band into reach next turn — but it is the same mechanism as the run above, and against
    ///   anything with real health it is the same chain of one-point strikes.
    ///
    /// Both are the gentle fallback, and the fix for both is the same: among words that cannot
    /// commit the murder the ceiling exists to prevent, smallness buys nothing. `need` tracks the
    /// enemy's health, so **every point of damage lowers the band toward the board**; hitting as
    /// hard as safety allows is how an unreachable scare becomes reachable.
    ///
    /// [`Outcome::least`] survives as the last resort, for the case that produced it: a murder
    /// warning backed the band down to `need 0, below 1`, and no word scores zero, so nothing at all
    /// is under that ceiling. The gentlest word is then the least bad of the words that overshoot.
    ///
    /// ## On a free turn, nothing before the kill
    ///
    /// [`Outcome::nothing`] sits between the safe chip and the overshoot: when the enemy cannot hit
    /// back this turn, a word that does nothing is better than one that murders. It is only ever
    /// filled in on such a turn ([`Modifiers::stall`]), so this is the old chain everywhere else.
    pub fn choice_for(&self, goal: Goal) -> Option<&Found> {
        match goal {
            Goal::Scare { .. } | Goal::Hold { .. } => self
                .lethal
                .as_ref()
                .or(self.under.as_ref())
                .or(self.nothing.as_ref())
                .or(self.least.as_ref()),
            _ => self.choice(),
        }
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
    /// **Nothing selects this any more.** [`Goal::RankedKill`] has the general case, and as of
    /// 2026-08-10 the last exception went with it — so this is reachable only from a test.
    ///
    /// Kept, not deleted, because it is the documented fallback: if the ranking turns out not to earn
    /// the full dictionary scan it costs, this is what we come back to, and it is a one-line change
    /// in `killing_blow` to do so.
    ///
    /// The exception that went was `killing_blow`'s [`crate::rested::Aim::HealFully`], on the
    /// argument that a player who heals on overkill wants the *deepest* overkill that still lands.
    /// Wrong twice: this does not find the deepest overkill — it reports whichever thread cleared
    /// `need` first and stops the rest — and depth past a full heal is worth nothing to want, because
    /// a player can only be missing so much health. See `killing_blow`.
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
    /// **Keep it here, alive and not yet frightened, on a turn it cannot hit back.** Damage
    /// strictly less than `below` leaves it standing and short of its flee threshold; damage of
    /// `lethal` or more kills it. For an enemy that cannot run the two are the same edge. Only on
    /// a free turn ([`Modifiers::stall`]), and never for an enemy already past its threshold, which
    /// runs whatever we play.
    ///
    /// Why not end it: whatever ends it on this turn — a kill or a scare — spends the one enemy
    /// turn that could not have hurt us on its leaving, and the next enemy then gets to act on a
    /// turn that can. Ended on the turn after, its own attack never comes and the next enemy
    /// opens on a free turn. The user's rule, 2026-11-04: *a word that scares it off should not be
    /// allowed on the halfling's even turns, because after the current enemy runs off, the next
    /// enemy will do damage on its first turn* — and the same of a kill, which is what the user
    /// first asked for: *stop just short of killing them*. It is also the dev's *"the halfling only
    /// fears enemies on even turns"*, counted from the console.
    ///
    /// There is no band. In order: the hardest hit under `below` ([`Tier::Under`]) — the chip makes
    /// the next turn's kill or scare easier — then a word that deals nothing ([`Tier::Stall`]),
    /// then a scare after all, gentlest first ([`Tier::Flees`]), and a kill only when nothing else
    /// is on the board. The scare is kept above the kill as the last resort, because for an enemy
    /// that can run the kill is the avoidable murder.
    Hold { below: i64, lethal: i64 },
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

/// The least damage worth submitting a word for.
///
/// **One, not zero.** A word that deals nothing spends the turn and buys the enemy a free swing, so
/// there is currently no state in which it is the right play. A live run submitted `A` and then `O`,
/// both scoring 0, under a band the murder backoff had collapsed to `0..=0`; that read as the
/// handler working and it was a wasted turn either way.
///
/// **Post-MVP this stops being true**, and the two exceptions the dev named are specific:
///
/// - **The halfling on a turn the enemy cannot land on.** `immuneTurnMod2` — "Enemies can't deal
///   damage to you on even turns" (`items/classpassives.lua:35-48`). **The parity input now
///   exists**: [`crate::parity`] reads it, [`Modifiers::player_dodges`] carries it, and the turn log
///   prints it. What is still missing is a way to *choose* a turn that does nothing.
///
///   Counting the way the console does — `Player turn n start;`, which is 0 for the first word —
///   the halfling is safe on odd turns and can be hit on even ones. That is the dev's sentence
///   exactly: *"make sure the halfling also only fears enemies on even turns"*. An earlier version
///   of this comment said "the halfling on an odd turn" and meant the same thing; a later one said
///   the opposite by quoting `turnNumber`, which is the *other* numbering. See [`crate::parity`],
///   which now holds the conversion and two tests that pin it to files in this repo.
///
///   The floor still stays. Dropping it to zero on a dodge turn is worse than leaving it —
///   `Goal::for_enemy` carries the argument. What was worth having is the **stall**, and it now
///   exists: on a turn we cannot be hit, when every word that lands would commit an avoidable
///   murder, a word that deals nothing is played instead of the kill. It is a separate tier below
///   the safe chip ([`Tier::Stall`]), not a lower floor, so it never outranks a word that does
///   real work short of scaring. See [`Modifiers::stall`] and [`Goal::Hold`]. On a turn that is not free,
///   `fight::nothing_left_to_lower` still takes the kill, which is the dev's recorded call.
///
/// - **The slime brooch, letting toxin land the kill.** Not a scare consideration at all, which is
///   what this comment said before the dev corrected it. `onStatusKillToxinRegen` is
///   `type = 'onStatusKill'` and pays `affectPlayerStatus('regen', enemy.statusEffects.toxin)` —
///   "an enemy succumbing to status damage gives you 1 turn of health regeneration for each toxin
///   stack they had" (`items/statusgear.lua:75`). A kill by our own damage arrives *before* the
///   toxin does and earns nothing. The game says so itself: the brooch's `highlightIf` (`:83`) is
///   `not getEstimationAttackAboutToKill() and getEstimationEnemyAboutToDie()` — hold back, and let
///   the status finish it.
///
/// The brooch still needs machinery that does not exist — the enemy's status arithmetic — so a band
/// that aims at zero is still a bug rather than a tactic. The stall above never aims at zero: it
/// only reorders what to play once nothing in the band is on the board.
pub const MIN_MEANINGFUL_DAMAGE: i64 = 1;

impl Goal {
    /// The ranking context this goal's band is judged by.
    ///
    /// A band that does not kill — [`Goal::Scare`], [`Goal::Hold`] — ranks without the payouts only
    /// a kill collects (see [`crate::pick::Preferences::without_kill_payouts`]). The kills that do
    /// turn up under those goals are their overshoot and are never ranked.
    pub fn picking(&self, picking: &crate::pick::Context) -> crate::pick::Context {
        match self {
            Goal::Scare { .. } | Goal::Hold { .. } => crate::pick::Context {
                prefs: picking.prefs.without_kill_payouts(),
                ..picking.clone()
            },
            // The blow must make the kill for what pays on it — see `Preferences::kill_at`.
            Goal::FirstKill { need } | Goal::RankedKill { need } => crate::pick::Context {
                prefs: crate::pick::Preferences { kill_at: Some(*need), ..picking.prefs },
                ..picking.clone()
            },
            Goal::MaxDamage => picking.clone(),
        }
    }

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
        player: Option<&PlayerState>, prefs: crate::pick::Preferences,
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
        // **A free turn ends nothing** — see [`Goal::Hold`]. Ahead of every other goal, overkill
        // gold included: the gold is still there for the taking on the next turn.
        if mods.stall {
            if let Some(hold) = Self::hold(mods, health, armour, max_health) {
                return hold;
            }
        }
        if mods.overkill_gold {
            // Gold scales with the excess, so this one really does want the corpse.
            return Goal::MaxDamage;
        }
        // `immobile` suppresses fleeing outright (`rpgview.lua:1646`) -- it cannot run, so trying to
        // frighten it just leaves it alive and swinging.
        if mods.immobile {
            return Self::killing_blow(kill, lethal, player, prefs);
        }
        let (Some(nerve), Some(max)) = (mods.nerve, max_health) else {
            return Self::killing_blow(kill, lethal, player, prefs);
        };
        let Some(top) = nerve.leaves_at_or_below(max) else {
            return Self::killing_blow(kill, lethal, player, prefs);
        };
        let below = lethal;
        // Armour absorbs first, so reaching `top` health costs the difference plus the armour.
        let scare_need = ((health - top) + armour).max(0);
        if scare_need >= below {
            // No non-lethal hit reaches the threshold, so this one dies. **But it is still a
            // fearable enemy**, so `player` is dropped on this path and the plain kill is used:
            // aiming past lethal for a heal spends damage we do not need on an enemy the game
            // classes as a person (`looksLikeAvoidableFirstMurder`, `wordboard.lua:486-497`).
            //
            // On a free turn the empty band is kept instead, as below — see [`Modifiers::stall`].
            if mods.stall {
                return Goal::Scare { need: below, below };
            }
            return Self::killing_blow(kill, lethal, None, prefs);
        }
        // Buffer both edges by as much as the band can spare. `slack` is the width of the raw
        // non-lethal band `scare_need ..= below - 1`; see `affordable_buffer` for why this shrinks
        // instead of inverting.
        let b = affordable_buffer(below - 1 - scare_need);
        let ceiling = below - b;
        // Floored **after** the buffer, not before it.
        //
        // An enemy already at or under its flee threshold needs nothing to make it run, so
        // `scare_need` is legitimately 0 — but we still have to submit a word to end the turn, and
        // one that deals nothing hands it a free swing (see [`MIN_MEANINGFUL_DAMAGE`]). Flooring
        // the input instead would double-count against the buffer and narrow a band that was
        // already correct: at 4 health against a threshold of 5, `0+1 .. 3` becomes `1+1 .. 3`, a
        // band one point wide for no reason.
        //
        // **This floor stays at 1 even when the enemy cannot swing.** Dropping it to 0 on a dodge
        // turn admits zero-damage words to the band, and a band is ranked by [`crate::pick::Rank`], which carries **no damage term at all** because every
        // candidate in a band is acceptable by construction. A zero-damage word would therefore win
        // on board tidiness over a word that actually moves the enemy toward its flee threshold, and
        // being unable to be hit this turn does not make that trade good — the work still has to be
        // done, one turn later, on a turn we *can* be hit. A free turn is answered above, by
        // [`Goal::Hold`], which never reaches this band. See [`MIN_MEANINGFUL_DAMAGE`].
        let need = (scare_need + b).max(MIN_MEANINGFUL_DAMAGE);
        // The floor can push the floor into the ceiling, and that is not a band.
        //
        // An enemy on 1 health with no armour is the case: `scare_need` is 0 because it is already
        // past its threshold, `below` is 1 because 1 damage kills it, and the floor lifts `need` to
        // exactly 1. There is no hit that both lands and leaves it alive, which is the same
        // conclusion the `scare_need >= below` check above reaches by a different route — so it
        // ends the same way, killing without chasing a heal.
        //
        // **Unless the turn is free** ([`Modifiers::stall`]). Then the band stays, empty, and
        // nothing lands in it; the ranking falls through to a word that deals nothing, and an
        // enemy already past its threshold runs at the start of its own turn. Only if the board has
        // no such word does this end in the kill after all — as the gentlest overshoot.
        if need >= ceiling {
            if mods.stall {
                return Goal::Scare { need: below, below };
            }
            return Self::killing_blow(kill, lethal, None, prefs);
        }
        Goal::Scare { need, below: ceiling }
    }

    /// The [`Goal::Hold`] for a free turn, or `None` when nothing short of a kill can keep the enemy
    /// here — one already past its flee threshold runs at the start of its own turn whatever we play,
    /// and [`Goal::for_enemy`]'s scare path decides that turn instead.
    ///
    /// Both edges are buffered downward, by as much as the gap can spare: our damage can be off by a
    /// point either way, and here a point too many ends the enemy on the turn we meant to keep it
    /// through. Floored at 1, the ceiling that admits only a word dealing nothing.
    fn hold(mods: &Modifiers, health: i64, armour: i64, max_health: Option<i64>) -> Option<Goal> {
        let lethal = health + armour;
        let below_lethal = (lethal - DAMAGE_BUFFER).max(MIN_MEANINGFUL_DAMAGE);
        // An enemy that can run must be kept short of running too. `immobile` cannot run
        // (`rpgview.lua:1646`), so it only has the one edge.
        let nerve = mods.nerve.filter(|_| !mods.immobile);
        let Some(top) = nerve.zip(max_health).and_then(|(n, max)| n.leaves_at_or_below(max)) else {
            return Some(Goal::Hold { below: below_lethal, lethal: below_lethal });
        };
        // Judged on health, not on the damage to reach the threshold: armour keeps that above zero
        // for an enemy already on its way out.
        if health <= top {
            return None;
        }
        let scare_need = (health - top) + armour;
        // The scare's own ceiling, buffered as [`Goal::Scare`] buffers it, is where killing starts.
        let ceiling = lethal - affordable_buffer(lethal - 1 - scare_need);
        let below = (scare_need - DAMAGE_BUFFER).max(MIN_MEANINGFUL_DAMAGE);
        Some(Goal::Hold { below, lethal: ceiling })
    }

    /// The kill, adjusted to heal this wound off the Well-Rested overkill heal.
    ///
    /// Overkill on a killing blow heals `floor(overkill/2)` (`rpgview.lua:1086`). See
    /// [`crate::rested`] for the mechanic in full. `lethal` is the EXACT damage that kills,
    /// unbuffered, because the heal threshold is arithmetic about overkill.
    fn killing_blow(
        kill: Goal, lethal: i64, player: Option<&PlayerState>, prefs: crate::pick::Preferences,
    ) -> Goal {
        let Some(p) = player else { return kill };
        if !p.heals {
            return kill;
        }
        match crate::rested::aim(p.vitals, p.bleeding) {
            crate::rested::Aim::Best => kill,
            // Spend it well: enough overkill that the floored halving covers the whole deficit. If
            // nothing reaches that, the search falls back to its best-scoring word, which is exactly
            // "if we cannot heal fully, use the best scoring answer".
            //
            // Buffered, because this is a kill with no upper bound: overshooting heals a little more
            // than needed, undershooting fails to kill at all.
            //
            // ## Ranked, not first-found, and the dev's reason is the good one
            //
            // **A player can only be missing so much health.** `need` already carries the whole
            // deficit — `2 * missing`, because the heal is `floor(overkill/2)` — so every point past
            // it heals nothing at all. There is no prize for hitting harder here, which makes this a
            // threshold like any other kill's, and the question of *which* word to clear it with is
            // the ordinary one: the cheapest.
            //
            // It read `Goal::FirstKill` and was justified as wanting "the deepest overkill that
            // still lands". That was wrong twice over. `FirstKill` does not find the deepest
            // overkill — it reports whichever thread reached `need` first and stops the rest — and
            // depth past a full heal is worth nothing to want. So a board could spend its `Q` on a
            // heal a wood word would also have completed. [`crate::pick::Rank`] answers it now,
            // including what the word costs the board (`pick::hoarded`).
            //
            // ## Unless the wood gear is also paying, in which case the floor comes off
            //
            // The dev's rule, 2026-08-15: *if a Well-Rested heal only heals for 3 while a wood-only
            // kill provides 4 armour, first check if there's a wood-only kill that triggers both;
            // otherwise go for the best wood-only kill, and only if that's not possible, go for the
            // Well-Rested heal.*
            //
            // A raised floor cannot say that. It discards every word under `2 * missing` before
            // anything is compared, so the wood word that kills without healing is never seen — and
            // that word is the dev's *second* choice, ahead of the heal. So when the wood payout is
            // live the floor drops to plain lethal and [`crate::pick::Rank`] carries the heal as its
            // second key, below `wood_only`. Same four tiers, in the stated order, and the search
            // still sees every candidate.
            //
            // When no wood gear is paying there is nothing to trade the heal against, and the floor
            // stays where it was: a heal we can reach is simply worth reaching.
            crate::rested::Aim::HealFully => {
                let heal_at = lethal + 2 * p.vitals.missing();
                match prefs.wood_only_pays {
                    true => Goal::RankedKill { need: lethal + DAMAGE_BUFFER },
                    false => Goal::RankedKill { need: heal_at + DAMAGE_BUFFER },
                }
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
    dict: &Dictionary, scorer: &Scorer, tiles: &[Tile], geometry: &Geometry, mods: &Modifiers,
    goal: Goal, picking: &crate::pick::Context, threads: usize,
) -> Outcome {
    let picking = &goal.picking(picking);
    match goal {
        Goal::FirstKill { need } => {
            race_for_band(dict, scorer, tiles, geometry, mods, need, None, picking, threads)
        }
        Goal::RankedKill { need } => {
            ranked_kill(dict, scorer, tiles, geometry, mods, need, picking, threads)
        }
        // The same search either way — a band is a band. Only the fallback differs, and that is
        // `Outcome::choice_for`'s business rather than the racer's.
        Goal::Scare { need, below } => {
            race_for_band(dict, scorer, tiles, geometry, mods, need, Some(below), picking, threads)
        }
        // A band with nothing in it, under the hold's ceiling: `under` is the chip, `nothing` the
        // stall, and `least` the gentlest of the rest, which is a scare whenever the board has one.
        Goal::Hold { below, .. } => {
            race_for_band(dict, scorer, tiles, geometry, mods, below, Some(below), picking, threads)
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
    dict: &Dictionary, scorer: &Scorer, tiles: &[Tile], geometry: &Geometry, mods: &Modifiers,
    need: i64, picking: &crate::pick::Context, threads: usize,
) -> Outcome {
    let typist = Typist::new(tiles, geometry);
    let corner_count = geometry.corner_count();
    let words = dict.words();
    let threads = threads.max(1).min(words.len().max(1));

    // The winner and the rank it won with, together: comparing a candidate needs the incumbent's
    // rank, and recomputing it per comparison would walk the board again for nothing.
    let lethal: Mutex<Option<(Found, crate::pick::Rank)>> = Mutex::new(None);
    let best: Mutex<Option<Hardest>> = Mutex::new(None);
    let ranker = Ranker { mods, tiles, geometry, scorer, picking };
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
                let mut local_best: Option<Hardest> = None;
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
                        for typed in typist.placements(word) {
                            let consumed: Vec<Tile> =
                                typed.tiles.iter().map(|&i| tiles[i].clone()).collect();
                            let score = scored(
                                scorer,
                                mods,
                                word,
                                &consumed,
                                Some(&typed),
                                typed.corners_used,
                                corner_count,
                            );
                            // Measured as the goal is: see [`Modifiers::reach`].
                            let reach = mods.reach(word, score);
                            if reach >= need {
                                let rank = crate::pick::rank(
                                    word,
                                    tiles,
                                    geometry,
                                    &typed.tiles,
                                    scorer,
                                    picking,
                                    reach - mods.status_damage(scorer, word, &consumed),
                                );
                                let take = match &local_lethal {
                                    Some((_, cur)) => rank.better_than(cur),
                                    None => true,
                                };
                                if take {
                                    local_lethal = Some((
                                        Found {
                                            word: word.clone(),
                                            score,
                                            slice,
                                            typed: typed.clone(),
                                        },
                                        rank,
                                    ));
                                }
                            }
                            let at = &typed;
                            Hardest::offer(
                                &mut local_best,
                                mods,
                                ranker,
                                word,
                                score,
                                slice,
                                at,
                                reach,
                            );
                            if local_longest
                                .as_ref()
                                .map(|b| word.chars().count() > b.word.chars().count())
                                .unwrap_or(true)
                            {
                                local_longest = Some(Found {
                                    word: word.clone(),
                                    score,
                                    slice,
                                    typed: typed.clone(),
                                });
                            }
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
                    Hardest::merge(&mut best.lock().unwrap(), mods, ranker, lb);
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
        best: best.into_inner().unwrap().map(|h| h.found),
        // No ceiling on this goal, so nothing ever asks for the gentlest word — see
        // `Outcome::choice_for`. Left unset rather than collected, so that a goal which *did* want
        // it and came through here would fail loudly instead of quietly playing the hardest hit.
        least: None,
        // Likewise: with no ceiling there is nothing to stay under.
        under: None,
        nothing: None,
        longest: longest.into_inner().unwrap(),
        words_considered: considered.into_inner(),
    }
}

/// Exhaustive hardest-hit search, balanced by work-stealing.
pub fn max_damage(
    dict: &Dictionary, scorer: &Scorer, tiles: &[Tile], geometry: &Geometry, mods: &Modifiers,
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
                        for typed in typist.placements(word) {
                            let consumed: Vec<Tile> =
                                typed.tiles.iter().map(|&i| tiles[i].clone()).collect();
                            let score = scored(
                                scorer,
                                mods,
                                word,
                                &consumed,
                                Some(&typed),
                                typed.corners_used,
                                corner_count,
                            );
                            if local_best
                                .as_ref()
                                .map(|b| {
                                    mods.hits_harder((word, score), (&b.word, b.score)).is_gt()
                                })
                                .unwrap_or(true)
                            {
                                local_best = Some(Found {
                                    word: word.clone(),
                                    score,
                                    slice,
                                    typed: typed.clone(),
                                });
                            }
                            if local_longest
                                .as_ref()
                                .map(|b| word.chars().count() > b.word.chars().count())
                                .unwrap_or(true)
                            {
                                local_longest = Some(Found {
                                    word: word.clone(),
                                    score,
                                    slice,
                                    typed: typed.clone(),
                                });
                            }
                        }
                    }
                }
                considered.fetch_add(seen, Ordering::Relaxed);
                if let Some(lb) = local_best {
                    let mut b = best.lock().unwrap();
                    if b.as_ref()
                        .map(|cur| {
                            mods.hits_harder((&lb.word, lb.score), (&cur.word, cur.score)).is_gt()
                        })
                        .unwrap_or(true)
                    {
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
        // `MaxDamage` is the one goal that explicitly wants the hardest hit, so the gentlest word is
        // not merely unused here — asking for it would be a contradiction. The same goes for a
        // ceiling this goal does not have.
        least: None,
        under: None,
        nothing: None,
        longest: longest.into_inner().unwrap(),
        words_considered: considered.into_inner(),
    }
}

/// Where a candidate sits in [`ranked`]'s list, in the order the tiers are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Meets the goal: lethal for a kill, inside the band for a scare.
    InBand,
    /// A scare's safe chip: lands, and stays under the ceiling. See [`Outcome::under`].
    Under,
    /// A deliberate wasted turn: deals nothing, on a turn the enemy cannot hit back. Only when
    /// [`Modifiers::stall`] is set; otherwise these words are [`Tier::Fallback`]. Ordered among
    /// themselves by [`crate::pick::Rank`], as the band is. See [`Outcome::nothing`].
    Stall,
    /// A [`Goal::Hold`]'s last resort before a kill: frightens it off on the free turn the hold
    /// exists to keep it through. Gentlest first.
    Flees,
    /// A scare's overshoot: would break the ceiling. See [`Outcome::least`].
    Overshoot,
    /// Everything else, hardest hit first — or, for a scare, the words that do no damage at all.
    Fallback,
}

/// One playable word, as [`ranked`] lists it.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub word: String,
    /// The damage the word deals — what a person is shown.
    pub score: i64,
    /// [`Modifiers::reach`]: the damage as the goal measures it, which is what decides the tier.
    pub reach: i64,
    /// The damage ignores armour ([`Modifiers::pierces`]).
    pub pierces: bool,
    pub tier: Tier,
    /// Only for [`Tier::InBand`], where it is what orders the band — as in the search.
    pub rank: Option<crate::pick::Rank>,
    /// Which tiles the word consumes, so a caller can show them.
    pub typed: crate::typist::Typed,
    /// The spell to cast for [`Candidate::score`], when one beats the plain attack.
    pub cast: Option<&'static str>,
    /// The part of [`Candidate::score`] that is status — toxin, ice, burn — paid at the start of
    /// the enemy's turn, so a kill it makes pays nothing the blow would: [`Modifiers::afflictions`].
    pub afflictions: Afflictions,
}

/// **Every** playable word, in the order Diggle prefers them for `goal`.
///
/// [`search`] answers "which one"; this answers "and then which", for the advisor
/// (`src/bin/diggle_advisor.rs`), which shows the whole list to a human who may see something the
/// model does not. The order is [`Outcome::choice_for`]'s fallback chain carried down the whole
/// list rather than stopped at its first answer:
///
/// - **kill goals** (`RankedKill`, `FirstKill`): the band by [`crate::pick::Rank`],
///   then everything else hardest first, since a kill that cannot be made tidily is still a kill;
/// - **`Scare`**: the band by `Rank`, then the safe chip hardest first, then the overshoots
///   *gentlest* first, then the words that do nothing — except on a free turn
///   ([`Modifiers::stall`]), when the words that do nothing move up ahead of the overshoots, as
///   [`Tier::Stall`], ordered by `Rank`;
/// - **`Hold`**: the safe chip hardest first, then the words that do nothing by `Rank`, then the
///   scares and then the kills, each gentlest first;
/// - **`MaxDamage`**: hardest first.
///
/// "Hardest" is [`Modifiers::hits_harder`] and "gentlest" is [`Modifiers::reach`], the same as the
/// search's own fallbacks; both are plain damage unless the rapier is held. The tiers themselves are
/// decided on reach, so a rapier word kills on health alone.
///
/// Ties are broken by the harder hit, then alphabetically, so the list is stable between reads of
/// the same save. `FirstKill` is listed as `RankedKill` would list it: a list has no "first to be found".
///
/// **Diggle does not play from this yet.** It still calls [`search`], and
/// `ranked_head_matches_choice_for_on_every_fixture` holds the two together: the head of this list
/// is Diggle's word or one tied with it. Moving Diggle onto this list is a separate change, for
/// after the advisor has been watched in real fights — the same caution [`Outcome`]'s note records
/// about restructuring a ranking that has not yet been seen working.
#[allow(clippy::too_many_arguments)]
pub fn ranked(
    dict: &Dictionary, scorer: &Scorer, tiles: &[Tile], geometry: &Geometry, mods: &Modifiers,
    goal: Goal, picking: &crate::pick::Context, threads: usize,
) -> Vec<Candidate> {
    if tiles.is_empty() {
        return Vec::new();
    }
    let picking = &goal.picking(picking);
    let typist = Typist::new(tiles, geometry);
    let corner_count = geometry.corner_count();
    let words = dict.words();
    let threads = threads.max(1).min(words.len().max(1));
    // Judged on reach, which is the damage measured the way the thresholds are (see
    // [`Modifiers::reach`]) — except the floor, which is about whether the word lands at all.
    let tier_of = |score: i64, reach: i64| -> Tier {
        match goal {
            Goal::RankedKill { need } | Goal::FirstKill { need } if reach >= need => Tier::InBand,
            Goal::Scare { need, below } if reach >= need && reach < below => Tier::InBand,
            Goal::Scare { below, .. } if reach >= below => Tier::Overshoot,
            Goal::Scare { .. } if score >= MIN_MEANINGFUL_DAMAGE => Tier::Under,
            Goal::Scare { .. } if mods.stall => Tier::Stall,
            Goal::Hold { lethal, .. } if reach >= lethal => Tier::Overshoot,
            Goal::Hold { below, .. } if reach >= below => Tier::Flees,
            Goal::Hold { .. } if score >= MIN_MEANINGFUL_DAMAGE => Tier::Under,
            Goal::Hold { .. } => Tier::Stall,
            _ => Tier::Fallback,
        }
    };
    let all: Mutex<Vec<Candidate>> = Mutex::new(Vec::new());
    let cursor = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let (all, cursor, typist, tier_of) = (&all, &cursor, &typist, &tier_of);
            scope.spawn(move || {
                let mut local = Vec::new();
                loop {
                    let start = cursor.fetch_add(CLAIM, Ordering::Relaxed);
                    if start >= words.len() {
                        break;
                    }
                    for word in &words[start..(start + CLAIM).min(words.len())] {
                        if mods.excluded.contains(word) {
                            continue;
                        }
                        let mut options: Vec<Candidate> = Vec::new();
                        for typed in typist.placements(word) {
                            let consumed: Vec<Tile> =
                                typed.tiles.iter().map(|&i| tiles[i].clone()).collect();
                            let (score, cast) = scored_cast(
                                scorer,
                                mods,
                                word,
                                &consumed,
                                Some(&typed),
                                typed.corners_used,
                                corner_count,
                            );
                            let reach = mods.reach(word, score);
                            let afflictions = mods.afflictions(scorer, word, &consumed);
                            let tier = tier_of(score, reach);
                            // Every tier that is ordered by the ranking, in full or on a tie: all but
                            // the gentlest-first ones.
                            let ranked = !matches!(tier, Tier::Overshoot | Tier::Flees);
                            let rank = ranked.then(|| {
                                crate::pick::rank(
                                    word,
                                    tiles,
                                    geometry,
                                    &typed.tiles,
                                    scorer,
                                    picking,
                                    reach - mods.status_damage(scorer, word, &consumed),
                                )
                            });
                            options.push(Candidate {
                                word: word.clone(),
                                score,
                                reach,
                                pierces: mods.pierces(word),
                                tier,
                                rank,
                                typed,
                                cast,
                                afflictions,
                            });
                        }
                        // One row per word: its best placement. On a tie, the one that spends less
                        // of what the board keeps — gold, wildcards, gems ([`crate::pick::hoarded`])
                        // — so a letter is shown on the plain twin when the precious one would do
                        // no more: the user, 2026-11-07. The ranking already asks this inside a
                        // band; the tiers without one (overshoot, flees) need it asked here. Then
                        // the typed one.
                        let heal_wanted = picking.prefs.room.health > 0;
                        let kept = |c: &Candidate| {
                            crate::pick::hoarded(tiles, &c.typed.tiles, scorer, heal_wanted)
                        };
                        if let Some(best) = options.into_iter().reduce(|a, b| {
                            let b_first = order(mods, &b, &a)
                                .then_with(|| kept(&b).total_cmp(&kept(&a)))
                                .is_lt();
                            if b_first {
                                b
                            } else {
                                a
                            }
                        }) {
                            local.push(best);
                        }
                    }
                }
                all.lock().unwrap().extend(local);
            });
        }
    });
    let mut list = all.into_inner().unwrap();
    list.sort_by(|a, b| order(mods, a, b));
    list
}

/// [`ranked`]'s comparison: tier, then within it the tier's own ordering, then the tiebreaks.
///
/// "Hardest first" is [`Modifiers::hits_harder`] and "gentlest first" is reach, so both agree with
/// the search's own fallbacks.
fn order(mods: &Modifiers, a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    let tier = |t: Tier| t as usize;
    let rank = || match (a.rank, b.rank) {
        (Some(ra), Some(rb)) if ra.better_than(&rb) => Less,
        (Some(ra), Some(rb)) if rb.better_than(&ra) => Greater,
        _ => Equal,
    };
    let harder = || mods.hits_harder((&b.word, b.score), (&a.word, a.score));
    tier(a.tier).cmp(&tier(b.tier)).then_with(|| {
        let within = match a.tier {
            // A band: every word in it reaches the goal, so the ranking decides and damage only
            // breaks its ties.
            Tier::InBand => rank().then_with(harder),
            // Gentlest first: the least it can do past the line.
            Tier::Overshoot | Tier::Flees => a.reach.cmp(&b.reach),
            // Short of the goal, a stall's chips and its words that deal nothing included: the
            // hardest hit, and the ranking between equals. See [`Hardest`].
            Tier::Under | Tier::Stall | Tier::Fallback => harder().then_with(rank),
        };
        within.then_with(|| a.word.cmp(&b.word))
    })
}

/// Ranks a word with the turn's shared [`crate::pick::Rank`]. Bundled so the collectors below can
/// rank a word only when they need to, which is when two words tie on damage.
#[derive(Clone, Copy)]
struct Ranker<'a> {
    mods: &'a Modifiers,
    tiles: &'a [Tile],
    geometry: &'a Geometry,
    scorer: &'a Scorer,
    picking: &'a crate::pick::Context,
}

impl Ranker<'_> {
    /// Ranked on the blow's own damage, [`Modifiers::toxin`] taken off: what the blow alone pays
    /// on — the wood-kill gear, the Well-Rested heal — does not come from an enemy the poison
    /// finishes (`rpgview.enemyDeathChecks` runs with no word, `rpgview.lua:1152-1170`).
    fn rank(&self, word: &str, consumed: &[usize], reach: i64) -> crate::pick::Rank {
        let tiles: Vec<Tile> =
            consumed.iter().filter_map(|&i| self.tiles.get(i).cloned()).collect();
        crate::pick::rank(
            word,
            self.tiles,
            self.geometry,
            consumed,
            self.scorer,
            self.picking,
            reach - self.mods.status_damage(self.scorer, word, &tiles),
        )
    }
}

/// The hardest hit seen so far — [`Outcome::best`] and [`Outcome::under`] — with what it needs to
/// be ranked if another word ties it.
///
/// **Hardest first, then the shared ranking.** The user, 2026-11-04: a turn where nothing reaches
/// the goal plays the hardest hit, and when two words hit equally hard the one the ranking prefers
/// — more fire eaten, a `!` brought down, more health or armour, less gold spent — is the better
/// of them, exactly as it is between two kills. A stall is that same turn with a ceiling
/// (*it should behave the same as a turn where none of the candidates kill the target*), so the
/// chip under the ceiling is chosen this way too, and a word that deals nothing is simply the
/// weakest hit. Before this, a tie went to whichever thread saw its word first: on one live save
/// twenty words hit the hold's 12 and the one played gave nothing while others gave 4 armour.
///
/// The rank is computed lazily, on the first tie, because most words lose on damage alone and the
/// board walk would be wasted on them.
struct Hardest {
    found: Found,
    consumed: Vec<usize>,
    reach: i64,
    rank: Option<crate::pick::Rank>,
}

impl Hardest {
    /// Keeps `word` in `slot` if it hits harder, or as hard and ranks better.
    #[allow(clippy::too_many_arguments)]
    fn offer(
        slot: &mut Option<Hardest>, mods: &Modifiers, ranker: Ranker, word: &str, score: i64,
        slice: usize, typed: &crate::typist::Typed, reach: i64,
    ) {
        let consumed = &typed.tiles;
        let fresh = || Hardest {
            found: Found { word: word.to_string(), score, slice, typed: typed.clone() },
            consumed: consumed.to_vec(),
            reach,
            rank: None,
        };
        let Some(cur) = slot.as_mut() else {
            *slot = Some(fresh());
            return;
        };
        match mods.hits_harder((word, score), (&cur.found.word, cur.found.score)) {
            std::cmp::Ordering::Less => {}
            std::cmp::Ordering::Greater => *slot = Some(fresh()),
            std::cmp::Ordering::Equal => {
                let mine = ranker.rank(word, consumed, reach);
                if mine.better_than(&cur.ranked(ranker)) {
                    *slot = Some(Hardest { rank: Some(mine), ..fresh() });
                }
            }
        }
    }

    /// The same comparison between two threads' winners.
    fn merge(slot: &mut Option<Hardest>, mods: &Modifiers, ranker: Ranker, mut other: Hardest) {
        let Some(cur) = slot.as_mut() else {
            *slot = Some(other);
            return;
        };
        let wins = match mods
            .hits_harder((&other.found.word, other.found.score), (&cur.found.word, cur.found.score))
        {
            std::cmp::Ordering::Less => false,
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Equal => other.ranked(ranker).better_than(&cur.ranked(ranker)),
        };
        if wins {
            *slot = Some(other);
        }
    }

    fn ranked(&mut self, ranker: Ranker) -> crate::pick::Rank {
        let (word, consumed, reach) = (&self.found.word, &self.consumed, self.reach);
        *self.rank.get_or_insert_with(|| ranker.rank(word, consumed, reach))
    }
}

pub fn race_for_kill(
    dict: &Dictionary, scorer: &Scorer, tiles: &[Tile], geometry: &Geometry, mods: &Modifiers,
    need: i64, picking: &crate::pick::Context, threads: usize,
) -> Outcome {
    race_for_band(dict, scorer, tiles, geometry, mods, need, None, picking, threads)
}

/// First word scoring at least `need`, and under `below` when one is given.
///
/// The upper bound is what turns a kill race into a scare: the enemy has to survive to run away, so
/// a lethal word is not an acceptable answer even though it scores higher.
#[allow(clippy::too_many_arguments)]
pub fn race_for_band(
    dict: &Dictionary, scorer: &Scorer, tiles: &[Tile], geometry: &Geometry, mods: &Modifiers,
    need: i64, below: Option<i64>, picking: &crate::pick::Context, threads: usize,
) -> Outcome {
    let typist = Typist::new(tiles, geometry);
    let corner_count = geometry.corner_count();
    let words = dict.words();
    let threads = threads.max(1).min(words.len().max(1));
    let chunk = words.len().div_ceil(threads);

    // **Not a race any more, whatever the name says.** It stopped every thread on the first word
    // inside the band, so which qualifying word got played was decided by thread scheduling — and a
    // band is usually wide. Live 2026-08-10, a highwayman with a `17..=25` band was routed with a
    // word that spent a **solid gold tile**, while cheaper words in the same band went unexamined.
    //
    // A band is a set of equally acceptable answers, which is exactly the situation
    // [`crate::pick::Rank`] exists for — it is already what picks between two words that both kill.
    // Ranking costs a full dictionary scan instead of an early exit; [`ranked_kill`] pays the same
    // price for the same reason, and hoarding a gold tile is worth more than the milliseconds.
    let stop = AtomicBool::new(false);
    let lethal: Mutex<Option<(Found, crate::pick::Rank)>> = Mutex::new(None);
    let best: Mutex<Option<Hardest>> = Mutex::new(None);
    let ranker = Ranker { mods, tiles, geometry, scorer, picking };
    let least: Mutex<Option<Found>> = Mutex::new(None);
    let under: Mutex<Option<Hardest>> = Mutex::new(None);
    let nothing: Mutex<Option<(Found, crate::pick::Rank)>> = Mutex::new(None);
    let longest: Mutex<Option<Found>> = Mutex::new(None);
    let considered = std::sync::atomic::AtomicUsize::new(0);

    std::thread::scope(|scope| {
        for (slice, part) in words.chunks(chunk).enumerate() {
            let (stop, lethal, best, least, under, longest, considered, typist, mods) =
                (&stop, &lethal, &best, &least, &under, &longest, &considered, &typist, mods);
            let nothing = &nothing;
            scope.spawn(move || {
                let mut seen = 0usize;
                let mut local_lethal: Option<(Found, crate::pick::Rank)> = None;
                let mut local_best: Option<Hardest> = None;
                let mut local_least: Option<Found> = None;
                let mut local_under: Option<Hardest> = None;
                let mut local_nothing: Option<(Found, crate::pick::Rank)> = None;
                let mut local_longest: Option<Found> = None;
                'words: for word in part {
                    // Checked periodically rather than every word: an atomic load per word would
                    // dominate the loop for no benefit at this granularity. Only ever set by the
                    // unbounded-kill path below; a banded search must see every word to rank them.
                    if seen % 512 == 0 && stop.load(Ordering::Relaxed) {
                        break;
                    }
                    seen += 1;
                    if mods.excluded.contains(word) {
                        continue;
                    }
                    // Not "are the letters there" but "does typing this work", which also tells us
                    // exactly which tiles it eats -- and so what it is worth.
                    for typed in typist.placements(word) {
                        let consumed: Vec<Tile> =
                            typed.tiles.iter().map(|&i| tiles[i].clone()).collect();
                        let score = scored(
                            scorer,
                            mods,
                            word,
                            &consumed,
                            Some(&typed),
                            typed.corners_used,
                            corner_count,
                        );
                        // Measured as the goal is: see [`Modifiers::reach`]. The floor below stays on
                        // the damage itself, because it asks whether the word lands at all.
                        let reach = mods.reach(word, score);
                        if reach >= need && below.map(|b| reach < b).unwrap_or(true) {
                            // Ranked, not raced. Every word in the band ends the exchange the same way,
                            // so the tiles it spends are the only thing left to choose on.
                            let rank = crate::pick::rank(
                                word,
                                tiles,
                                geometry,
                                &typed.tiles,
                                scorer,
                                picking,
                                reach - mods.status_damage(scorer, word, &consumed),
                            );
                            // **Only a band ranks.** With no ceiling this is `race_for_kill`, whose
                            // contract is `Goal::FirstKill`'s: every lethal word ends the exchange, so
                            // the first one wins and the rest stop. Scanning the whole dictionary to
                            // choose between words that are all simply "a kill" is the cost that goal
                            // exists to avoid.
                            if below.is_none() {
                                let mut l = lethal.lock().unwrap();
                                if l.is_none() {
                                    *l = Some((
                                        Found {
                                            word: word.clone(),
                                            score,
                                            slice,
                                            typed: typed.clone(),
                                        },
                                        rank,
                                    ));
                                }
                                stop.store(true, Ordering::Relaxed);
                                break 'words;
                            }
                            let better = match local_lethal.as_ref() {
                                Some((_, cur)) => rank.better_than(cur),
                                None => true,
                            };
                            if better {
                                local_lethal = Some((
                                    Found {
                                        word: word.clone(),
                                        score,
                                        slice,
                                        typed: typed.clone(),
                                    },
                                    rank,
                                ));
                            }
                            continue;
                        }
                        let at = &typed;
                        Hardest::offer(
                            &mut local_best,
                            mods,
                            ranker,
                            word,
                            score,
                            slice,
                            at,
                            reach,
                        );
                        // The gentlest word that still does something. A zero-scoring word is the
                        // gentlest of all and is never the answer — see `MIN_MEANINGFUL_DAMAGE` — so
                        // this is the minimum over words that actually land, not over all of them.
                        if score >= MIN_MEANINGFUL_DAMAGE
                            && local_least
                                .as_ref()
                                .map(|b| reach < mods.reach(&b.word, b.score))
                                .unwrap_or(true)
                        {
                            local_least = Some(Found {
                                word: word.clone(),
                                score,
                                slice,
                                typed: typed.clone(),
                            });
                        }
                        // The hardest word that still cannot break a ceiling. Every word reaching this
                        // line missed the band, so `reach < below` here also means `reach < need` — the
                        // in-between words left above. See [`Outcome::under`].
                        if score >= MIN_MEANINGFUL_DAMAGE && below.is_some_and(|b| reach < b) {
                            let at = &typed;
                            Hardest::offer(
                                &mut local_under,
                                mods,
                                ranker,
                                word,
                                score,
                                slice,
                                at,
                                reach,
                            );
                        }
                        // The word to waste a free turn on — see [`Outcome::nothing`]. Ranked, because
                        // they all do the same nothing and differ only in what they leave behind.
                        if mods.stall
                            && score < MIN_MEANINGFUL_DAMAGE
                            && below.is_some_and(|b| reach < b)
                        {
                            let rank = crate::pick::rank(
                                word,
                                tiles,
                                geometry,
                                &typed.tiles,
                                scorer,
                                picking,
                                reach - mods.status_damage(scorer, word, &consumed),
                            );
                            if local_nothing.as_ref().is_none_or(|(_, cur)| rank.better_than(cur)) {
                                local_nothing = Some((
                                    Found {
                                        word: word.clone(),
                                        score,
                                        slice,
                                        typed: typed.clone(),
                                    },
                                    rank,
                                ));
                            }
                        }
                        if local_longest
                            .as_ref()
                            .map(|b| word.chars().count() > b.word.chars().count())
                            .unwrap_or(true)
                        {
                            local_longest = Some(Found {
                                word: word.clone(),
                                score,
                                slice,
                                typed: typed.clone(),
                            });
                        }
                    }
                }
                considered.fetch_add(seen, Ordering::Relaxed);
                if let Some((lf, rank)) = local_lethal {
                    let mut l = lethal.lock().unwrap();
                    if l.as_ref().map(|(_, cur)| rank.better_than(cur)).unwrap_or(true) {
                        *l = Some((lf, rank));
                    }
                }
                if let Some(lb) = local_best {
                    Hardest::merge(&mut best.lock().unwrap(), mods, ranker, lb);
                }
                if let Some(lst) = local_least {
                    let mut l = least.lock().unwrap();
                    if l.as_ref()
                        .map(|cur| {
                            mods.reach(&lst.word, lst.score) < mods.reach(&cur.word, cur.score)
                        })
                        .unwrap_or(true)
                    {
                        *l = Some(lst);
                    }
                }
                if let Some(lu) = local_under {
                    Hardest::merge(&mut under.lock().unwrap(), mods, ranker, lu);
                }
                if let Some((ln, rank)) = local_nothing {
                    let mut n = nothing.lock().unwrap();
                    if n.as_ref().is_none_or(|(_, cur)| rank.better_than(cur)) {
                        *n = Some((ln, rank));
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
        lethal: lethal.into_inner().unwrap().map(|(f, _)| f),
        best: best.into_inner().unwrap().map(|h| h.found),
        least: least.into_inner().unwrap(),
        // Stays `None` when no ceiling was given, because the collector above requires one. That is
        // the `race_for_kill` case, where nothing is out of bounds and `best` is already the answer.
        under: under.into_inner().unwrap().map(|h| h.found),
        nothing: nothing.into_inner().unwrap().map(|(f, _)| f),
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

    fn race(
        scorer: &Scorer, dict: &Dictionary, tiles: &[Tile], mods: &Modifiers, need: i64,
    ) -> Outcome {
        race_for_kill(
            dict,
            scorer,
            tiles,
            &Geometry::default(),
            mods,
            need,
            &crate::pick::Context::default(),
            8,
        )
    }

    /// The shortsword, end to end, against the readings that exposed it.
    ///
    /// From the run of 2026-08-09, player wearing `wordScoreBonusPreLength456 = 3`: we logged
    /// `AAPA` at 4 and the game estimated 7, while three-letter `AAM` was accepted unchanged.
    ///
    /// **The gap is what is asserted, not the absolute score.** On plain tiles `AAPA` comes to 5
    /// here, not the 4 the run logged, so one of the tiles it actually consumed carried a quality —
    /// a burn or a carbon count — that this fixture does not reproduce. Asserting 4 would be
    /// asserting a board we do not have. The invariant that survives is the one the bug was made
    /// of: for a four-letter word at multiplier 1, the three stacks add exactly 3, because
    /// `lengthScore(4)` is `0.2*5 = 1.0` and `preAdd` is scaled by it.
    #[test]
    fn the_shortsword_adds_exactly_three_to_a_four_letter_words_damage() {
        if !present() {
            return;
        }
        let scorer = Scorer::new(&game_dir()).unwrap();
        let bare = Modifiers::none();
        let armed = Modifiers {
            plan: crate::gear::Gear::from_pairs(&[("wordScoreBonusPreLength456", 3.0)]).compile(),
            ..Modifiers::none()
        };
        let tiles =
            |w: &str| -> Vec<Tile> { w.chars().map(|c| Tile::plain(&c.to_string())).collect() };

        let aapa = tiles("AAPA");
        let unarmed = scored(&scorer, &bare, "AAPA", &aapa, None, 0, 0);
        let armed_score = scored(&scorer, &armed, "AAPA", &aapa, None, 0, 0);
        assert_eq!(
            armed_score - unarmed,
            3,
            "the same +3 that turned the run's 4 into the game's 7 (got {unarmed} -> {armed_score})"
        );

        // Three letters is outside the 4..=6 band, so the gear changes nothing — which is what
        // pins the cause to the length band rather than to something else about the board.
        let aam = tiles("AAM");
        assert_eq!(
            scored(&scorer, &bare, "AAM", &aam, None, 0, 0),
            scored(&scorer, &armed, "AAM", &aam, None, 0, 0),
            "a three-letter word takes no length bonus either way"
        );

        // Twelve letters is outside it too, so the word that looped the run was never affected by
        // this gear — it was simply the hardest hit on the board, which is the other bug.
        let long = tiles("VENEPUNCTURE");
        assert_eq!(
            scored(&scorer, &bare, "VENEPUNCTURE", &long, None, 0, 0),
            scored(&scorer, &armed, "VENEPUNCTURE", &long, None, 0, 0),
        );
    }

    /// Gear and lexicon bonuses add on one accumulator; they do not multiply.
    ///
    /// `words.modifiers` passes its `mult` into `getWordBonusModifier`, which does `mult+val-1`.
    /// Treating them as independent factors would over-estimate whenever both apply — and an
    /// over-estimate is the miss that types a word expecting a kill and does not get one.
    #[test]
    fn a_gear_multiplier_joins_the_lexicon_accumulator_rather_than_multiplying_it() {
        let mut m = Modifiers::none();
        m.bonuses = vec![(["OOZE".to_string()].into_iter().collect(), 1.5)];
        // Gear at +0.25 and a 1.5x lexicon: 1.25 + 0.5 = 1.75, not 1.25 * 1.5 = 1.875.
        assert_eq!(m.modifier_for_base("OOZE", 0, 0, 1.25), 1.75);
        // Without the gear term the old behaviour is unchanged.
        assert_eq!(m.modifier_for("OOZE", 0, 0), 1.5);
    }

    /// A scare band can be missed from **either side**, and the two want opposite answers.
    ///
    /// This fixture is the *floor* case: `need: 1000` on a board whose best word scores far less, so
    /// nothing reaches the band from below. That is the 2026-08-10 death — a cultist at 127+33 with
    /// the band at `98..=158`, and seven consecutive one-point words while the board eroded from
    /// sixteen tiles to nine. Playing softly there scares nothing, kills nothing and buys nothing.
    ///
    /// **This test asserted the opposite until 2026-08-10**, and it passed, because its fixture
    /// never matched the failure in its own comment. It described the murder backoff — `need 0,
    /// below 1`, where every word *overshoots* — but was written with `need: 1000`, where every word
    /// undershoots. One rule was being asked to cover two directions and only one of them was ever
    /// exercised. The overshoot direction now has its own test below, on the band that actually
    /// caused it.
    #[test]
    fn a_scare_whose_floor_is_out_of_reach_hits_as_hard_as_it_can() {
        if !present() {
            return;
        }
        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        let tiles = crypt_board();
        let mods = Modifiers::none();
        let goal = Goal::Scare { need: 1000, below: 1001 };
        let out = race_for_band(
            &dict,
            &scorer,
            &tiles,
            &Geometry::default(),
            &mods,
            1000,
            Some(1001),
            &crate::pick::Context::default(),
            8,
        );

        assert!(out.lethal.is_none(), "no word on this board scores 1000, so the band is empty");
        let best = out.best.as_ref().expect("the scan still collects a best word");
        let least = out.least.as_ref().expect("and must now collect a least one");
        assert!(
            least.score < best.score,
            "this board has words of different scores, or the test proves nothing: \
             least {} ({}), best {} ({})",
            least.word,
            least.score,
            best.word,
            best.score
        );

        let played = out.choice_for(goal).expect("something is playable");
        assert_eq!(
            played.score, best.score,
            "nothing reaches the floor, so the scare is off and chipping is the only play; \
             expected {} at {}, got {} at {}",
            best.word, best.score, played.word, played.score
        );
        // Hitting hard here cannot commit the murder the ceiling guards against: the whole board is
        // below `need`, and `need <= below`. That is what makes this direction safe as well as right.
        assert!(played.score < 1000, "the hardest word is still under the floor");

        // The ordinary kill goals must be unaffected — they have no ceiling, so hitting hard is
        // still the right miss.
        assert_eq!(
            out.choice_for(Goal::MaxDamage).map(|f| f.score),
            Some(best.score),
            "only a band goal changes direction"
        );
    }

    /// The other direction: every word **overshoots** the ceiling, so the gentlest is the answer.
    ///
    /// The live failure, 2026-08-09: a murder warning backed the band down to `need 0, below 1`
    /// against an enemy on 2 health. Nothing landed inside it, the fallback handed back `best` —
    /// `VENEPUNCTURE`, 40 points — the game refused it as murder, the backoff lowered the ceiling
    /// again, and the same word came back thirty times until the run was stopped by hand. "Aim as
    /// low as you can" had come to mean "hit as hard as you can".
    ///
    /// Built from an `Outcome` directly rather than from a dictionary scan, because the thing under
    /// test is the fallback rule and this is the one band that cannot be reproduced from the crypt
    /// board — single letters on plain tiles score zero, so `need 0, below 1` is *satisfiable* there
    /// and the bug never fires. That is exactly why the original test drifted onto the wrong fixture.
    #[test]
    fn a_scare_whose_ceiling_is_below_everything_plays_the_gentlest_word() {
        // `under` is what a search with THIS ceiling would have collected: nothing scores under 1,
        // because nothing scores zero — which is the whole reason this band cannot be satisfied.
        let out = Outcome {
            lethal: None,
            best: Some(Found {
                word: "VENEPUNCTURE".into(),
                score: 40,
                slice: 0,
                typed: Default::default(),
            }),
            least: Some(Found { word: "AT".into(), score: 2, slice: 0, typed: Default::default() }),
            under: None,
            nothing: None,
            longest: Some(Found {
                word: "VENEPUNCTURE".into(),
                score: 40,
                slice: 0,
                typed: Default::default(),
            }),
            words_considered: 900,
        };
        assert_eq!(
            out.choice_for(Goal::Scare { need: 0, below: 1 }).map(|f| f.word.as_str()),
            Some("AT"),
            "nothing at all is under this ceiling, so the least bad overshoot is the smallest hit"
        );
    }

    /// The control for the test above, and the run it is named for.
    ///
    /// A separate `Outcome` rather than a second goal on the same one, because `under` is collected
    /// *for a particular ceiling* — asking one search's result about a different band is a question
    /// no live caller can ask, and the old shared fixture quietly encoded a state the search cannot
    /// produce.
    #[test]
    fn a_scare_nothing_can_reach_chips_as_hard_as_the_ceiling_allows() {
        // 2026-08-10, a cultist at 127+33: band `98..=158`, best word on the board 29. Every word is
        // under the ceiling, so the hardest one is also the hardest safe one.
        let out = Outcome {
            lethal: None,
            best: Some(Found {
                word: "VENEPUNCTURE".into(),
                score: 29,
                slice: 0,
                typed: Default::default(),
            }),
            least: Some(Found { word: "SE".into(), score: 1, slice: 0, typed: Default::default() }),
            under: Some(Found {
                word: "VENEPUNCTURE".into(),
                score: 29,
                slice: 0,
                typed: Default::default(),
            }),
            nothing: None,
            longest: Some(Found {
                word: "VENEPUNCTURE".into(),
                score: 29,
                slice: 0,
                typed: Default::default(),
            }),
            words_considered: 900,
        };
        assert_eq!(
            out.choice_for(Goal::Scare { need: 98, below: 159 }).map(|f| f.word.as_str()),
            Some("VENEPUNCTURE"),
            "seven turns of one-point words lost this run"
        );
    }

    /// The case neither of the pair above covers, and the reason the rule is now one rule.
    ///
    /// 2026-08-14, a `4..=5` band on a 6hp cultist: nothing landed inside it, and the fallback chose
    /// between a word that would murder her and a word that did 1. There was a third option all
    /// along — the hardest hit that still leaves her standing — and the old two-way branch could not
    /// express it, because `best` was over the floor and so the "chip hard" arm never ran.
    #[test]
    fn a_scare_that_can_only_overshoot_or_tickle_plays_the_hardest_safe_word() {
        let out = Outcome {
            lethal: None,
            // Over the ceiling: playing this kills her, and she carries `fear`, so that is murder.
            best: Some(Found {
                word: "SISSOO".into(),
                score: 25,
                slice: 0,
                typed: Default::default(),
            }),
            least: Some(Found { word: "AA".into(), score: 1, slice: 0, typed: Default::default() }),
            under: Some(Found {
                word: "MIL".into(),
                score: 3,
                slice: 0,
                typed: Default::default(),
            }),
            nothing: None,
            longest: Some(Found {
                word: "SISSOO".into(),
                score: 25,
                slice: 0,
                typed: Default::default(),
            }),
            words_considered: 900,
        };
        let goal = Goal::Scare { need: 4, below: 6 };
        assert_eq!(
            out.choice_for(goal).map(|f| f.word.as_str()),
            Some("MIL"),
            "3 damage cannot murder and drops the band by three times as much as 1 does"
        );

        // A word inside the band still beats every fallback — this is a miss handler, not a policy.
        let mut hit = out.clone();
        hit.lethal =
            Some(Found { word: "LOWN".into(), score: 4, slice: 0, typed: Default::default() });
        assert_eq!(hit.choice_for(goal).map(|f| f.word.as_str()), Some("LOWN"));
    }

    /// **On a free turn, nothing comes before the kill** — and after everything that does real work.
    #[test]
    fn a_free_turn_spends_itself_on_nothing_before_it_kills() {
        let out = Outcome {
            lethal: None,
            best: Some(Found {
                word: "SISSOO".into(),
                score: 25,
                slice: 0,
                typed: Default::default(),
            }),
            least: Some(Found { word: "AA".into(), score: 1, slice: 0, typed: Default::default() }),
            under: None,
            nothing: Some(Found {
                word: "O".into(),
                score: 0,
                slice: 0,
                typed: Default::default(),
            }),
            longest: Some(Found {
                word: "SISSOO".into(),
                score: 25,
                slice: 0,
                typed: Default::default(),
            }),
            words_considered: 900,
        };
        let goal = Goal::Scare { need: 1, below: 1 };
        assert_eq!(out.choice_for(goal).map(|f| f.word.as_str()), Some("O"));
        // Without the stall the field is never filled, and the gentlest overshoot is played.
        let no_stall = Outcome { nothing: None, ..out.clone() };
        assert_eq!(no_stall.choice_for(goal).map(|f| f.word.as_str()), Some("AA"));
        // A safe chip still beats doing nothing: it moves the band toward the board.
        let chip = Outcome {
            under: Some(Found {
                word: "MIL".into(),
                score: 3,
                slice: 0,
                typed: Default::default(),
            }),
            ..out.clone()
        };
        assert_eq!(
            chip.choice_for(Goal::Scare { need: 4, below: 6 }).map(|f| &*f.word),
            Some("MIL")
        );
        // A hold falls back the same way: the chip, then nothing, then the gentlest of the rest.
        let hold = Goal::Hold { below: 4, lethal: 9 };
        assert_eq!(chip.choice_for(hold).map(|f| &*f.word), Some("MIL"));
        assert_eq!(out.choice_for(hold).map(|f| &*f.word), Some("O"));
        assert_eq!(no_stall.choice_for(hold).map(|f| &*f.word), Some("AA"));
    }

    /// The producer of [`Outcome::nothing`], against the real dictionary: the crypt board's single
    /// letters score zero, so on a free turn there is a word to waste it on — and only then.
    #[test]
    fn the_search_finds_a_word_that_deals_nothing_only_when_stalling() {
        if !present() {
            return;
        }
        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        let tiles = crypt_board();
        let goal = Goal::Scare { need: 1, below: 1 };
        let run = |mods: &Modifiers| {
            race_for_band(
                &dict,
                &scorer,
                &tiles,
                &Geometry::default(),
                mods,
                1,
                Some(1),
                &crate::pick::Context::default(),
                8,
            )
        };
        let stalling = run(&Modifiers { stall: true, ..Modifiers::none() });
        let nothing = stalling.nothing.as_ref().expect("the crypt board has a zero-damage word");
        assert_eq!(nothing.score, 0, "{}", nothing.word);
        assert_eq!(stalling.choice_for(goal).map(|f| f.score), Some(0));

        let ordinary = run(&Modifiers::none());
        assert!(ordinary.nothing.is_none());
        let played = ordinary.choice_for(goal).expect("something is playable");
        assert!(played.score >= 1, "not stalling, so a word that lands: {}", played.word);

        // The advisor's list agrees: the stall tier heads it, and holds only zero-damage words.
        let mods = Modifiers { stall: true, ..Modifiers::none() };
        let ctx = crate::pick::Context::default();
        let list = ranked(&dict, &scorer, &tiles, &Geometry::default(), &mods, goal, &ctx, 8);
        assert_eq!(list[0].tier, Tier::Stall);
        let stalls: Vec<&Candidate> = list.iter().filter(|c| c.tier == Tier::Stall).collect();
        assert!(stalls.iter().all(|c| c.score == 0 && c.rank.is_some()));
        assert!(
            stalls.iter().any(|c| c.word == nothing.word),
            "{} not in the stall tier",
            nothing.word
        );
    }

    /// The producer, against the real dictionary — the tests above only exercise the consumer.
    ///
    /// Worth its own test because the two halves fail independently and identically from the
    /// outside: a `choice_for` that prefers `under` and a search that never fills it in play the
    /// gentlest word together, exactly as if nothing had been changed at all.
    ///
    /// The thresholds are derived from the board rather than guessed, so this asserts the *rule*
    /// and not the crypt board's particular arithmetic.
    #[test]
    fn the_search_collects_the_hardest_word_under_a_ceiling() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        let (tiles, geom, mods) = (crypt_board(), Geometry::default(), Modifiers::none());
        let pick = crate::pick::Context::default();
        let band = |need, below| {
            race_for_band(&dict, &scorer, &tiles, &geom, &mods, need, below, &pick, 8)
        };

        // A floor far above anything this board can reach, so nothing lands in the band and every
        // word on it is under the ceiling. The hardest safe word is then the hardest word.
        let free = band(1_000, Some(2_000));
        assert!(free.lethal.is_none(), "the band must be out of reach for this to test anything");
        let hardest = free.best.clone().expect("the crypt board makes words");
        assert_eq!(
            free.under.as_ref().map(|u| u.score),
            Some(hardest.score),
            "with nothing over the ceiling, the hardest safe word is the hardest word"
        );

        // Now drop the ceiling below the hardest word. `under` must fall with it — and must still be
        // the hardest of what remains, which is the entire point.
        let capped = band(1_000, Some(hardest.score));
        let u = capped.under.clone().expect("softer words exist below the hardest one");
        let softest = capped.least.clone().unwrap();
        assert!(u.score < hardest.score, "{} is not under a ceiling of {}", u.score, hardest.score);
        assert!(
            u.score > softest.score,
            "fell back to the gentlest word ({}) instead of the hardest safe one",
            softest.score
        );
        assert_eq!(
            capped.choice_for(Goal::Scare { need: 1_000, below: hardest.score }).map(|f| f.score),
            Some(u.score),
            "and it is what actually gets played"
        );

        // Control: with no ceiling nothing is out of bounds, so there is nothing to stay under and
        // the field is left empty rather than quietly duplicating `best`.
        assert!(race(&scorer, &dict, &tiles, &mods, 1_000).under.is_none());
    }

    #[test]
    fn refresh_only_when_nothing_good_and_nothing_lethal() {
        let lethal = Outcome {
            lethal: Some(Found {
                word: "CAT".into(),
                score: 9,
                slice: 0,
                typed: Default::default(),
            }),
            best: Some(Found { word: "CAT".into(), score: 9, slice: 0, typed: Default::default() }),
            least: None,
            under: None,
            nothing: None,
            longest: Some(Found {
                word: "CAT".into(),
                score: 9,
                slice: 0,
                typed: Default::default(),
            }),
            words_considered: 1,
        };
        // A kill ends the exchange, which beats any board-quality judgement.
        assert!(!lethal.should_refresh(8), "never refresh when a lethal word exists");

        let weak = Outcome {
            lethal: None,
            best: Some(Found { word: "CAT".into(), score: 4, slice: 0, typed: Default::default() }),
            least: None,
            under: None,
            nothing: None,
            longest: Some(Found {
                word: "CAT".into(),
                score: 4,
                slice: 0,
                typed: Default::default(),
            }),
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
            best: Some(Found {
                word: "JAY".into(),
                score: 20,
                slice: 0,
                typed: Default::default(),
            }),
            least: None,
            under: None,
            nothing: None,
            longest: Some(Found {
                word: "OATMEALS".into(),
                score: 12,
                slice: 1,
                typed: Default::default(),
            }),
            words_considered: 10,
        };
        assert!(
            !out.should_refresh(8),
            "an 8-letter word meets the threshold even if not top-scoring"
        );
        assert_eq!(
            out.choice().map(|f| f.word.as_str()),
            Some("JAY"),
            "still PLAY the best scorer"
        );
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

        let reachable =
            geometry.corner_indices().into_iter().filter(|&i| geometry.slot_selectable(i)).count();
        assert_eq!(reachable, 2, "only the row-1 corners are left");

        // And the search reflects it: the best word cannot exceed the half-damage ceiling.
        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        let board = crypt_board();
        let mut cornerless = Modifiers::none();
        cornerless.resist_cornerless = true;

        let best = race_for_kill(
            &dict,
            &scorer,
            &board,
            &geometry,
            &cornerless,
            100_000,
            &crate::pick::Context::default(),
            8,
        )
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
        assert_eq!(
            Goal::for_enemy(&mods, 3, 0, None, None, crate::pick::Preferences::default()),
            Goal::MaxDamage
        );
        // An ordinary enemy ranks its kills instead of racing for the first. Gold does not scale
        // with the excess here, so there is nothing to spend the extra damage on and the board the
        // word leaves behind is what the choice is for.
        assert_eq!(
            Goal::for_enemy(
                &Modifiers::none(),
                3,
                2,
                None,
                None,
                crate::pick::Preferences::default()
            ),
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

        let kill = search(
            &dict,
            &scorer,
            &board,
            &geom,
            &mods,
            Goal::FirstKill { need: 3 },
            &crate::pick::Context::default(),
            8,
        );
        let hardest = search(
            &dict,
            &scorer,
            &board,
            &geom,
            &mods,
            Goal::MaxDamage,
            &crate::pick::Context::default(),
            8,
        );

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
        let out =
            max_damage(&dict, &scorer, &crypt_board(), &Geometry::default(), &Modifiers::none(), 8);
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
            gems: Default::default(),
        };
        let need = 3;

        let out =
            search(&dict, &scorer, &board, &geom, &mods, Goal::RankedKill { need }, &picking, 8);
        let won = out.lethal.expect("a 3-health enemy must be killable from this board");

        let typist = Typist::new(&board, &geom);
        let rank_of = |word: &str| {
            let typed = typist.type_word(word)?;
            let consumed: Vec<Tile> = typed.tiles.iter().map(|&i| board[i].clone()).collect();
            let score = scored(
                &scorer,
                &mods,
                word,
                &consumed,
                Some(&typed),
                typed.corners_used,
                geom.corner_count(),
            );
            (score >= need).then(|| {
                crate::pick::rank(word, &board, &geom, &typed.tiles, &scorer, &picking, score)
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

        // **A band does the opposite, and deliberately.** With a ceiling, every qualifying word is
        // an equally good outcome and the only thing left to choose on is what it spends — so the
        // scan has to finish before anything can be ranked. Live 2026-08-10 it did not: a highwayman
        // with a `17..=25` band was routed with a word that burned a solid gold tile, because the
        // first in-band word any thread happened to reach won and stopped the rest.
        //
        // This pair is the control for each other. Removing the `below.is_none()` guard makes the
        // first assertion fail; removing the ranking makes this one fail.
        let banded = race_for_band(
            &dict,
            &scorer,
            &crypt_board(),
            &Geometry::default(),
            &Modifiers::none(),
            3,
            Some(1_000),
            &crate::pick::Context::default(),
            8,
        );
        assert!(banded.lethal.is_some(), "3..1000 is satisfiable on this board");
        assert_eq!(
            banded.words_considered,
            dict.len(),
            "a band must see every candidate before it can rank them"
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
        let g =
            Goal::for_enemy(&feared(), 4, 0, Some(6), None, crate::pick::Preferences::default());
        assert_eq!(g, Goal::Scare { need: 2, below: 4 }, "unbuffered, but still a scare");
    }

    #[test]
    fn a_band_with_two_to_spare_buys_a_buffer_on_each_side() {
        // health 6 of a maximum 6: flee at or below 2, so 4 damage; 6 kills. Raw band 4..=5 is one
        // wide -- still nothing. Widen the enemy and the buffer appears.
        assert_eq!(
            Goal::for_enemy(&feared(), 6, 0, Some(6), None, crate::pick::Preferences::default()),
            Goal::Scare { need: 4, below: 6 }
        );
        // health 12 of 12: flee at or below 5, so 7 damage; 12 kills. Raw band 7..=11 has four to
        // spare, so a full point goes to each edge.
        assert_eq!(
            Goal::for_enemy(&feared(), 12, 0, Some(12), None, crate::pick::Preferences::default()),
            Goal::Scare { need: 8, below: 11 }
        );
    }

    /// The buffer must never turn a survivable scare into a kill.
    #[test]
    fn the_buffered_ceiling_still_leaves_the_enemy_alive() {
        let g =
            Goal::for_enemy(&feared(), 12, 0, Some(12), None, crate::pick::Preferences::default());
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
            Goal::for_enemy(&feared(), 12, 0, Some(12), None, crate::pick::Preferences::default()),
            Goal::Scare { need: 8, below: 11 }
        );
    }

    #[test]
    fn armour_is_added_to_both_ends() {
        // Armour absorbs first, so reaching the threshold costs the difference plus the armour, and
        // the lethal line moves out by the same amount.
        assert_eq!(
            Goal::for_enemy(&feared(), 12, 3, Some(12), None, crate::pick::Preferences::default()),
            Goal::Scare { need: 11, below: 14 }
        );
    }

    /// No goal may ask for a word that deals nothing.
    ///
    /// A live run submitted `A` and then `O`, both scoring 0, under a band the murder backoff had
    /// collapsed to `0..=0`. Each spent a turn and bought the enemy a free swing. The legitimate
    /// zero-damage plays — halfling turn parity, and holding back so toxin lands the kill for the
    /// slime brooch — are post-MVP and need machinery that does not exist, so until then a floor of
    /// zero is a bug. See [`MIN_MEANINGFUL_DAMAGE`].
    #[test]
    fn no_scare_band_ever_asks_for_a_zero_damage_word() {
        // An enemy at or under its flee threshold needs nothing to make it run, which is exactly
        // where `scare_need` goes to zero.
        for health in 1..=6 {
            for armour in 0..=2 {
                let g = Goal::for_enemy(
                    &feared(),
                    health,
                    armour,
                    Some(12),
                    None,
                    crate::pick::Preferences::default(),
                );
                if let Goal::Scare { need, below } = g {
                    assert!(
                        need >= MIN_MEANINGFUL_DAMAGE,
                        "{health}+{armour}hp produced {g:?}, which permits a wasted turn"
                    );
                    assert!(need < below, "{health}+{armour}hp produced the empty band {g:?}");
                }
            }
        }
    }

    #[test]
    fn an_enemy_already_below_the_line_just_needs_a_non_lethal_word() {
        // It will run at the start of its own turn; our only job is not to kill it first.
        assert_eq!(
            Goal::for_enemy(&feared(), 4, 0, Some(12), None, crate::pick::Preferences::default()),
            Goal::Scare { need: 1, below: 3 }
        );
    }

    #[test]
    fn immobile_cannot_run_so_we_kill_it() {
        // rpgview.lua:1646 -- the flee branch requires `not currentEnemy.immobile`. Trying to scare
        // one would leave it alive and still attacking.
        let m = Modifiers { immobile: true, ..feared() };
        assert_eq!(
            Goal::for_enemy(&m, 12, 0, Some(12), None, crate::pick::Preferences::default()),
            Goal::RankedKill { need: 13 }
        );
    }

    #[test]
    fn without_a_known_maximum_we_do_not_guess() {
        assert_eq!(
            Goal::for_enemy(&feared(), 12, 0, None, None, crate::pick::Preferences::default()),
            Goal::RankedKill { need: 13 }
        );
    }

    #[test]
    fn overkill_gold_still_wants_the_corpse() {
        // Gold scales with the excess, so this fight is the exception.
        let m = Modifiers { overkill_gold: true, ..feared() };
        assert_eq!(
            Goal::for_enemy(&m, 12, 0, Some(12), None, crate::pick::Preferences::default()),
            Goal::MaxDamage
        );
    }

    #[test]
    fn a_one_health_maximum_offers_no_non_lethal_scare() {
        // leaves_at_or_below is None below 2 max health, so there is no room to hurt without killing.
        assert_eq!(
            Goal::for_enemy(&feared(), 1, 0, Some(1), None, crate::pick::Preferences::default()),
            Goal::RankedKill { need: 2 }
        );
    }

    /// **The stall keeps the band that has no room in it.** At 1 health past its threshold, any hit
    /// kills; on a free turn the goal stays a scare, empty, so the ranking reaches a word that deals
    /// nothing before it reaches one that kills.
    #[test]
    fn a_free_turn_keeps_the_empty_band_instead_of_killing() {
        let stalling = Modifiers { stall: true, ..feared() };
        let prefs = crate::pick::Preferences::default();
        assert_eq!(
            Goal::for_enemy(&stalling, 1, 0, Some(12), None, prefs),
            Goal::Scare { need: 1, below: 1 }
        );
        // The control: the same enemy on a turn that is not free is killed.
        assert_eq!(
            Goal::for_enemy(&feared(), 1, 0, Some(12), None, prefs),
            Goal::RankedKill { need: 2 }
        );
    }

    /// **A free turn holds an enemy that has not yet broken, rather than scaring it.** The user's
    /// rule: a scare on the halfling's even turn spends the turn the enemy could not hurt us on its
    /// leaving, and the next enemy acts on one that can.
    #[test]
    fn a_free_turn_holds_an_enemy_short_of_its_flee_threshold() {
        let stalling = Modifiers { stall: true, ..feared() };
        let prefs = crate::pick::Preferences::default();
        // 12 of 12, fleeing at 5 or below: 7 damage scares it, 12 kills it. Held under 6, one point
        // of buffer short of the scare; the scare's own ceiling stays where it was.
        assert_eq!(
            Goal::for_enemy(&feared(), 12, 0, Some(12), None, prefs),
            Goal::Scare { need: 8, below: 11 },
            "the control: not a free turn, so the ordinary scare"
        );
        assert_eq!(
            Goal::for_enemy(&stalling, 12, 0, Some(12), None, prefs),
            Goal::Hold { below: 6, lethal: 11 }
        );
        // Armour counts toward both edges, as it does for the scare.
        assert_eq!(
            Goal::for_enemy(&stalling, 12, 3, Some(12), None, prefs),
            Goal::Hold { below: 9, lethal: 14 }
        );
        // One point from breaking: no hit that lands is safe, so only a word that deals nothing.
        assert_eq!(
            Goal::for_enemy(&stalling, 6, 0, Some(12), None, prefs),
            Goal::Hold { below: 1, lethal: 5 }
        );
    }

    /// **Every enemy is held, not only the ones that can run**: a kill on a free turn hands the next
    /// enemy a turn it can hit on, as a scare does. Overkill gold and `immobile` included.
    #[test]
    fn a_free_turn_holds_an_enemy_with_no_nerve_short_of_the_kill() {
        let prefs = crate::pick::Preferences::default();
        let plain = Modifiers { stall: true, ..Modifiers::none() };
        assert_eq!(
            Goal::for_enemy(&Modifiers::none(), 15, 0, Some(15), None, prefs),
            Goal::RankedKill { need: 16 },
            "the control: not a free turn"
        );
        assert_eq!(
            Goal::for_enemy(&plain, 15, 0, Some(15), None, prefs),
            Goal::Hold { below: 14, lethal: 14 }
        );
        assert_eq!(
            Goal::for_enemy(&plain, 5, 3, None, None, prefs),
            Goal::Hold { below: 7, lethal: 7 },
            "armour counts, and no maximum is needed"
        );
        assert_eq!(
            Goal::for_enemy(&plain, 1, 0, None, None, prefs),
            Goal::Hold { below: 1, lethal: 1 },
            "on 1 health, only a word that deals nothing keeps it"
        );
        let gold = Modifiers { overkill_gold: true, stall: true, ..Modifiers::none() };
        assert_eq!(
            Goal::for_enemy(&gold, 15, 0, None, None, prefs),
            Goal::Hold { below: 14, lethal: 14 }
        );
        // Immobile with fear: it cannot run, so only the kill is an edge.
        let stuck = Modifiers { immobile: true, ..Modifiers { stall: true, ..feared() } };
        assert_eq!(
            Goal::for_enemy(&stuck, 12, 0, Some(12), None, prefs),
            Goal::Hold { below: 11, lethal: 11 }
        );
    }

    /// Already past its threshold, it runs at the start of its own turn whatever we play short of
    /// killing it — so a free turn changes nothing, and the ordinary scare stands.
    #[test]
    fn a_free_turn_cannot_hold_an_enemy_already_past_its_threshold() {
        let stalling = Modifiers { stall: true, ..feared() };
        let prefs = crate::pick::Preferences::default();
        for (health, max) in [(4, 12), (2, 6)] {
            assert_eq!(
                Goal::for_enemy(&stalling, health, 0, Some(max), None, prefs),
                Goal::for_enemy(&feared(), health, 0, Some(max), None, prefs),
                "{health} of {max}"
            );
            assert!(matches!(
                Goal::for_enemy(&stalling, health, 0, Some(max), None, prefs),
                Goal::Scare { .. }
            ));
        }
    }

    /// Every hold leaves the enemy short of breaking and alive, for every damage it admits.
    #[test]
    fn no_hold_admits_a_hit_that_scares_or_kills() {
        let prefs = crate::pick::Preferences::default();
        let mut holds = 0;
        for nerve in [Nerve::Fear, Nerve::Terror, Nerve::Caution] {
            let stalling = Modifiers { stall: true, nerve: Some(nerve), ..Modifiers::none() };
            for max in 2..=20 {
                for health in 1..=max {
                    for armour in 0..=3 {
                        let g = Goal::for_enemy(&stalling, health, armour, Some(max), None, prefs);
                        let Goal::Hold { below, lethal } = g else { continue };
                        holds += 1;
                        assert!(below >= MIN_MEANINGFUL_DAMAGE && below <= lethal, "{g:?}");
                        let at = format!("{nerve:?} {health}+{armour}/{max} under {g:?}");
                        for damage in 0..below {
                            let left = health - (damage - armour).max(0);
                            assert!(left > 0, "{damage} kills {at}");
                            assert!(!nerve.would_leave(left, max), "{damage} scares {at}");
                        }
                    }
                }
            }
        }
        assert!(holds > 100, "only {holds} holds: the sweep is not reaching the hold");
    }
}

#[cfg(test)]
mod rested_goal_tests {
    use super::*;
    use crate::rested::Vitals;

    fn player(current: i64) -> PlayerState {
        PlayerState { vitals: Vitals { current, max: 12 }, heals: true, bleeding: false }
    }

    /// Enemy on 10 with no armour, so the plain kill needs 10.
    ///
    /// No wood gear paying, which is the case where the Well-Rested heal stays a floor. The
    /// alternative is its own test — see `a_wood_kill_outranks_a_heal_it_cannot_also_collect`.
    fn goal(p: Option<&PlayerState>) -> Goal {
        Goal::for_enemy(&Modifiers::none(), 10, 0, None, p, crate::pick::Preferences::default())
    }

    #[test]
    fn a_scratch_is_healed_too() {
        // Missing 2 of 12: 4 of overkill buys it back, so 10 + 4, plus `DAMAGE_BUFFER`. No stack is
        // kept in reserve — the frugal kill that used to aim at `10..=11` here is gone.
        assert_eq!(goal(Some(&player(10))), Goal::RankedKill { need: 15 });
    }

    /// A full top-up is a threshold, so it is bought with the cheapest word that clears it.
    ///
    /// The dev's correction, 2026-08-10: **a player can only be missing so much health.** `need`
    /// already carries the whole deficit, so every point past it heals nothing — which makes this an
    /// ordinary kill threshold and the word that clears it an ordinary ranking question. It was
    /// `FirstKill`, justified as wanting the deepest overkill; depth past a full heal is worth
    /// nothing to want, and `FirstKill` does not find it anyway.
    #[test]
    fn a_bad_wound_buys_a_full_top_up_with_the_cheapest_word_that_does_it() {
        // Missing 8, and the heal is half the overkill, so 16 of overkill are needed: damage 26.
        assert_eq!(goal(Some(&player(4))), Goal::RankedKill { need: 27 });
    }

    #[test]
    fn full_health_kills_normally() {
        assert_eq!(goal(Some(&player(12))), Goal::RankedKill { need: 11 });
    }

    #[test]
    fn bleeding_kills_normally_because_no_heal_can_happen() {
        let p = PlayerState { bleeding: true, ..player(4) };
        assert_eq!(goal(Some(&p)), Goal::RankedKill { need: 11 });
    }

    #[test]
    fn a_cancelled_heal_kills_normally() {
        let p = PlayerState { heals: false, ..player(4) };
        assert_eq!(goal(Some(&p)), Goal::RankedKill { need: 11 });
    }

    #[test]
    fn scaring_takes_precedence_over_healing() {
        // We cannot heal off an enemy we deliberately leave alive -- and not murdering it is worth
        // more than a top-up.
        let m = Modifiers { nerve: Some(crate::flee::Nerve::Fear), ..Modifiers::none() };
        let p = player(4);
        assert_eq!(
            Goal::for_enemy(&m, 12, 0, Some(12), Some(&p), crate::pick::Preferences::default()),
            Goal::Scare { need: 8, below: 11 }
        );
    }
}

#[cfg(test)]
mod lexicon_bonus_tests {
    use super::*;

    fn with_bonus(pairs: &[(&str, f64)]) -> Modifiers {
        Modifiers {
            bonuses: pairs.iter().map(|(w, v)| (HashSet::from([w.to_string()]), *v)).collect(),
            bonus_names: Vec::new(),
            spells: None,
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

    /// Each bonus is named after its lexicon, in the same order, so a reader can say which.
    #[test]
    fn bonuses_are_named_after_their_lexicon() {
        let src = std::fs::read_to_string("tests/fixtures/combatSaveData-l1-depleted.lua").unwrap();
        let save = crate::game::save::parse(&src).unwrap();
        let tiles = save.table_at("tileboard").map(crate::observe::board::tiles_from).unwrap();
        let game = std::path::PathBuf::from("../sternly-worded-adventures");
        let (mods, _) = Modifiers::from_save(&game, &save, tiles.len()).unwrap();
        assert_eq!(mods.bonus_names, vec!["Bone".to_string()], "lexiconBonusBone = 2");
        assert_eq!(mods.bonus_names.len(), mods.bonuses.len());
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

#[cfg(test)]
mod typeable_count {
    use super::*;

    /// How many dictionary words can actually be spelled on a full board?
    ///
    /// **The number the gear design rests on.** `search::scored` runs only on words the typist
    /// accepted, so this — not the dictionary size — is the loop that per-word scoring work lives
    /// in. Measured on the level-0 crypt board, 2026-08-09: **1750 of 345128, 0.51%**.
    ///
    /// An earlier draft of `docs/superpowers/specs/2026-08-09-gear-word-bonuses-design.md` proposed
    /// precomputing word facts into a table parallel to the dictionary, costing ~1.4 MB and a
    /// load-time pass. That would have built 345k entries to answer 1750 questions. The measurement
    /// is what killed it, and this test is here so the assumption cannot rot silently: if a future
    /// board makes most of the dictionary typeable, the trade-off is worth revisiting.
    #[test]
    fn only_a_fraction_of_the_dictionary_can_be_spelled_on_one_board() {
        if !std::path::Path::new("../sternly-worded-adventures/utils/dictionary.lua").is_file() {
            return;
        }
        let dict =
            Dictionary::load(&std::path::PathBuf::from("../sternly-worded-adventures")).unwrap();
        let tiles: Vec<Tile> =
            "OYCAACTPORLIGAHJ".chars().map(|c| Tile::plain(&c.to_string())).collect();
        let geom = Geometry::default();
        let typist = crate::typist::Typist::new(&tiles, &geom);
        let total = dict.words().len();
        let typeable = dict.words().iter().filter(|w| typist.type_word(w).is_some()).count();
        let share = typeable as f64 / total as f64;
        assert!(
            share < 0.05,
            "{typeable} of {total} ({:.2}%) typeable — if this is no longer a small fraction, \
             precomputing word facts becomes worth reconsidering",
            share * 100.0
        );
    }
}

#[cfg(test)]
mod ranked_tests {
    use super::*;
    use std::path::PathBuf;

    fn game_dir() -> PathBuf {
        PathBuf::from("../sternly-worded-adventures")
    }

    struct World {
        dict: Dictionary,
        scorer: Scorer,
        board: Vec<Tile>,
        geom: Geometry,
        picking: crate::pick::Context,
    }

    /// The level-0 crypt board, with the real dictionary, scores and letter target.
    fn world() -> World {
        let board: Vec<Tile> =
            "OYCAACTPORLIGAHJ".chars().map(|c| Tile::plain(&c.to_string())).collect();
        let picking = crate::pick::Context {
            target: crate::letters::Weights::load(&game_dir()).unwrap().target(board.len()),
            prefs: crate::pick::Preferences::default(),
            gems: Default::default(),
        };
        World {
            dict: Dictionary::load(&game_dir()).unwrap(),
            scorer: Scorer::new(&game_dir()).unwrap(),
            board,
            geom: Geometry::default(),
            picking,
        }
    }

    fn rank_all(w: &World, mods: &Modifiers, goal: Goal) -> Vec<Candidate> {
        ranked(&w.dict, &w.scorer, &w.board, &w.geom, mods, goal, &w.picking, 8)
    }

    fn tier_index(t: Tier) -> usize {
        match t {
            Tier::InBand => 0,
            Tier::Under => 1,
            Tier::Stall => 2,
            Tier::Flees => 3,
            Tier::Overshoot => 4,
            Tier::Fallback => 5,
        }
    }

    /// Tiers never go backwards down the list.
    fn assert_tiers_in_order(list: &[Candidate]) {
        for pair in list.windows(2) {
            assert!(
                tier_index(pair[0].tier) <= tier_index(pair[1].tier),
                "{} ({:?}) listed before {} ({:?})",
                pair[0].word,
                pair[0].tier,
                pair[1].word,
                pair[1].tier
            );
        }
    }

    /// Within the band the order is the Rank's; outside it, the hardest hit first.
    #[test]
    fn ranked_kill_puts_lethal_words_first_by_rank() {
        let w = world();
        let need = 3;
        let list = rank_all(&w, &Modifiers::none(), Goal::RankedKill { need });
        assert_tiers_in_order(&list);
        let band: Vec<&Candidate> = list.iter().filter(|c| c.tier == Tier::InBand).collect();
        assert!(band.len() > 1, "the comparison proves nothing with {} kills", band.len());
        assert!(band.iter().all(|c| c.score >= need && c.rank.is_some()));
        for pair in band.windows(2) {
            let (a, b) = (pair[0].rank.unwrap(), pair[1].rank.unwrap());
            assert!(
                !b.better_than(&a),
                "{} outranks {} but is listed after it",
                pair[1].word,
                pair[0].word
            );
        }
        let rest: Vec<&Candidate> = list.iter().filter(|c| c.tier != Tier::InBand).collect();
        assert!(rest.iter().all(|c| c.score < need && c.tier == Tier::Fallback));
        assert!(rest.windows(2).all(|p| p[0].score >= p[1].score));
    }

    /// A scare: the band, then the safe chip hardest-first, then overshoots gentlest-first.
    #[test]
    fn scare_orders_band_then_under_then_overshoot_low_first() {
        let w = world();
        let (need, below) = (4, 7);
        let list = rank_all(&w, &Modifiers::none(), Goal::Scare { need, below });
        assert_tiers_in_order(&list);
        let of = |t: Tier| -> Vec<&Candidate> { list.iter().filter(|c| c.tier == t).collect() };
        assert!(of(Tier::InBand).iter().all(|c| c.score >= need && c.score < below));
        let under = of(Tier::Under);
        assert!(!under.is_empty());
        assert!(under.iter().all(|c| c.score >= MIN_MEANINGFUL_DAMAGE && c.score < need));
        assert!(under.windows(2).all(|p| p[0].score >= p[1].score));
        let over = of(Tier::Overshoot);
        assert!(!over.is_empty());
        assert!(over.iter().all(|c| c.score >= below));
        assert!(over.windows(2).all(|p| p[0].score <= p[1].score), "overshoots go gentlest first");
        assert!(of(Tier::Fallback).iter().all(|c| c.score < MIN_MEANINGFUL_DAMAGE));
    }

    /// A hold: the safe chip hardest first, then nothing, then the scares and the kills, each
    /// gentlest first — and the head of the list is the word Diggle would play.
    #[test]
    fn hold_orders_chip_then_nothing_then_scares_then_kills() {
        let w = world();
        let (below, lethal) = (4, 7);
        let goal = Goal::Hold { below, lethal };
        let mods = Modifiers { stall: true, ..Modifiers::none() };
        let list = rank_all(&w, &mods, goal);
        assert_tiers_in_order(&list);
        let of = |t: Tier| -> Vec<&Candidate> { list.iter().filter(|c| c.tier == t).collect() };
        assert!(of(Tier::InBand).is_empty(), "a hold has no band");
        let (under, stall, flees, over) =
            (of(Tier::Under), of(Tier::Stall), of(Tier::Flees), of(Tier::Overshoot));
        // Every tier present, or the order proves nothing.
        for (name, t) in [("under", &under), ("stall", &stall), ("flees", &flees), ("over", &over)]
        {
            assert!(!t.is_empty(), "this board has no {name} word for {goal:?}");
        }
        assert!(under.iter().all(|c| c.score >= 1 && c.score < below));
        assert!(under.windows(2).all(|p| p[0].score >= p[1].score), "chip hardest first");
        assert!(stall.iter().all(|c| c.score == 0));
        assert!(flees.iter().all(|c| c.score >= below && c.score < lethal));
        assert!(flees.windows(2).all(|p| p[0].score <= p[1].score), "scares gentlest first");
        assert!(over.iter().all(|c| c.score >= lethal));

        let out = search(&w.dict, &w.scorer, &w.board, &w.geom, &mods, goal, &w.picking, 8);
        let played = out.choice_for(goal).expect("something is playable");
        assert_eq!(
            played.score, list[0].score,
            "Diggle plays {}, the list leads {}",
            played.word, list[0].word
        );
        assert!(played.score >= 1 && played.score < below);
    }

    #[test]
    fn an_empty_board_ranks_nothing() {
        let mut w = world();
        w.board.clear();
        assert!(rank_all(&w, &Modifiers::none(), Goal::RankedKill { need: 1 }).is_empty());
    }

    /// With a spell book, a word that casts a stronger spell scores the spell's damage and names it;
    /// without one — Diggle, which never casts — it scores the plain attack.
    #[test]
    fn a_held_spell_raises_the_score_only_with_a_book() {
        let w = world();
        let lex = crate::lexica::Lexica::load(&game_dir()).unwrap();
        let word = lex.words_for("bone").unwrap().iter().min().unwrap().clone();
        let tiles: Vec<Tile> = word.chars().map(|c| Tile::plain(&c.to_string())).collect();
        let plain = scored(&w.scorer, &Modifiers::none(), &word, &tiles, None, 0, 0);
        assert!(plain > 0, "{word} must do some damage for the doubling to show");
        let cs = crate::game::save::parse(
            "return { rpg = { player = { gearFlags = { spellTurnUndead = 1 } } } }",
        )
        .unwrap();
        let mut mods = Modifiers::none();
        mods.spells = Some(crate::spells::SpellBook::from_save(&cs, &lex, Some(true)));
        assert_eq!(
            scored_cast(&w.scorer, &mods, &word, &tiles, None, 0, 0),
            (plain * 2, Some("Turn")),
            "{word}"
        );
        assert_eq!(scored(&w.scorer, &mods, &word, &tiles, None, 0, 0), plain * 2);
    }

    /// Modifiers for a player holding the rapier against an enemy on `health + armour`.
    fn rapier(health: i64, armour: i64) -> Modifiers {
        let save = crate::game::save::parse(
            "return { rpg = { player = { gearFlags = { damageArmourPiercingEndsMatch = 1 } } } }",
        )
        .unwrap();
        let mut mods = Modifiers::none();
        mods.weapon = crate::gear::Weapon::from_save(&crate::gear::Gear::from_save(&save), &save);
        mods.defence = Some(Defence { health, armour });
        mods
    }

    /// A rapier word goes straight to health, so against every threshold — all of them measured
    /// on health plus armour — it counts as its damage plus the armour it skips.
    #[test]
    fn a_rapier_word_reaches_past_the_armour() {
        let mods = rapier(42, 54);
        assert_eq!(mods.reach("SINGES", 45), 99);
        assert_eq!(mods.reach("SCATHING", 53), 53);
        let mut no_defence = rapier(42, 54);
        no_defence.defence = None;
        assert_eq!(no_defence.reach("SINGES", 45), 45);
        assert_eq!(Modifiers::none().reach("SINGES", 45), 45);
        assert!(!rapier(42, 0).pierces("SINGES"), "nothing to pierce");
    }

    /// The user's rule for a hit that does not kill: the share of what stands between us and the
    /// kill — health alone for a rapier word, health plus armour for any other.
    #[test]
    fn the_harder_hit_is_the_larger_share_of_what_is_left() {
        use std::cmp::Ordering::*;
        let mods = rapier(42, 54);
        // 53/96 strips the armour; 5/42 barely scratches the health.
        assert_eq!(mods.hits_harder(("SCATHING", 53), ("SIS", 5)), Greater);
        // 39/42 leaves it on 3; 53/96 leaves it on 42 + 1.
        assert_eq!(mods.hits_harder(("SANGAREES", 39), ("SCATHING", 53)), Greater);
        // A kill beats any non-kill, and kills compare by how far past lethal they reach, which is
        // what overkill gold pays on.
        assert_eq!(mods.hits_harder(("SINGES", 45), ("SCATHING", 95)), Greater);
        assert_eq!(mods.hits_harder(("SCATHING", 100), ("SINGES", 45)), Greater);
        assert_eq!(mods.hits_harder(("SIS", 5), ("SIS", 5)), Equal);
        // Without the rapier it is plain damage, as it always was.
        assert_eq!(Modifiers::none().hits_harder(("SIS", 5), ("SCATHING", 53)), Less);
    }

    /// With the rapier, the band is decided on reach: rapier words kill on health alone, and the
    /// words that do not kill are listed by the share of what is left that they remove.
    #[test]
    fn ranked_judges_rapier_words_against_health_alone() {
        let w = world();
        let mods = rapier(2, 200);
        let need = 2 + 200 + DAMAGE_BUFFER;
        let list = rank_all(&w, &mods, Goal::RankedKill { need });
        assert_tiers_in_order(&list);
        let band: Vec<&Candidate> = list.iter().filter(|c| c.tier == Tier::InBand).collect();
        assert!(!band.is_empty(), "some rapier word should do 3");
        for c in &list {
            assert_eq!(c.pierces, mods.pierces(&c.word), "{}", c.word);
            assert_eq!(c.reach, mods.reach(&c.word, c.score), "{}", c.word);
            assert_eq!(c.tier == Tier::InBand, c.reach >= need, "{} reaches {}", c.word, c.reach);
        }
        assert!(band.iter().all(|c| c.pierces), "nothing else on this board does {need}");

        // Nothing kills here, so the whole list is the fallback, ordered by the share removed.
        let mods = rapier(400, 600);
        let list = rank_all(&w, &mods, Goal::RankedKill { need: 1001 });
        assert!(list.iter().all(|c| c.tier == Tier::Fallback));
        assert!(list.iter().any(|c| c.pierces) && list.iter().any(|c| !c.pierces));
        for p in list.windows(2) {
            assert_ne!(
                mods.hits_harder((&p[0].word, p[0].score), (&p[1].word, p[1].score)),
                std::cmp::Ordering::Less,
                "{} listed above {}",
                p[0].word,
                p[1].word
            );
        }
        assert!(
            list.iter().position(|c| c.pierces) < list.iter().position(|c| !c.pierces),
            "at 400 health behind 600 armour, a rapier word's share is the larger"
        );
    }

    /// Boggling anxiety's chain items are modelled now — the curse by the typist, the bonus from
    /// the typed word — so neither is reported as a gap.
    #[test]
    fn the_chain_items_are_not_reported_as_unmodelled() {
        let src = std::fs::read_to_string("tests/fixtures/combatSaveData-crypt-l0.lua")
            .unwrap()
            .replacen(
                "gearFlags = {",
                "gearFlags = { wordRequirementAdjacent = 1, wordScoreBonusAdjacent = 1,",
                1,
            );
        let cs = crate::game::save::parse(&src).unwrap();
        let tiles = crate::turn::tiles_of(&cs).len();
        let (mods, geom) = Modifiers::from_save(&game_dir(), &cs, tiles).unwrap();
        assert!(geom.adjacency_required);
        let adjacency: Vec<&String> =
            mods.problems.iter().filter(|p| p.contains("Adjacent")).collect();
        assert!(adjacency.is_empty(), "{adjacency:?}");
    }

    /// What a word gives the player outranks board hygiene: with a braced tile on the board and
    /// room for armour, every kill that ends on it is listed before every kill that does not.
    #[test]
    fn kills_that_give_armour_are_listed_first() {
        let mut w = world();
        // `OYCAACTPORLIGAHJ`: brace the T at index 6.
        w.board[6].quality.material = Some("woodbraced".into());
        w.picking.prefs =
            w.picking.prefs.with_room(crate::pick::Room { armour: 100, ..Default::default() });
        let list = rank_all(&w, &Modifiers::none(), Goal::RankedKill { need: 3 });
        let band: Vec<&Candidate> = list.iter().filter(|c| c.tier == Tier::InBand).collect();
        let gains: Vec<i64> = band.iter().map(|c| c.rank.unwrap().gain).collect();
        assert!(gains.iter().any(|g| *g > 0), "some kill should end on the braced T");
        assert!(gains.iter().any(|g| *g == 0));
        assert!(
            gains.windows(2).all(|p| p[0] >= p[1]),
            "a kill without armour is listed above one with it"
        );
        assert!(band.iter().filter(|c| c.rank.unwrap().gain > 0).all(|c| c.word.ends_with('T')));
    }

    /// The wood gear pays on a kill, so a scare is not ranked by it. A live save with the Wooden
    /// idol buckler listed every all-wood scare above every other scare, for a payout no scare gets.
    #[test]
    fn a_scare_is_not_ranked_by_the_wood_kill_payout() {
        let mut w = world();
        w.picking.prefs.wood_only_pays = true;
        let mods = Modifiers::none();
        let wood = |goal| {
            rank_all(&w, &mods, goal)
                .iter()
                .filter(|c| c.tier == Tier::InBand)
                .filter(|c| c.rank.unwrap().wood_only)
                .count()
        };
        assert!(wood(Goal::RankedKill { need: 3 }) > 0, "the fixture must have wood-only kills");
        let scare = Goal::Scare { need: 3, below: 1000 };
        assert_eq!(wood(scare), 0, "a scare collects no wood payout");
        assert_eq!(wood(Goal::Hold { below: 1000, lethal: 1000 }), 0);

        // Diggle's search agrees: the same word as with no wood gear at all.
        let play = |w: &World| {
            search(&w.dict, &w.scorer, &w.board, &w.geom, &mods, scare, &w.picking, 8)
                .choice_for(scare)
                .map(|f| f.word.clone())
        };
        let with = play(&w);
        w.picking.prefs.wood_only_pays = false;
        assert_eq!(with, play(&w));
    }

    /// A Chrysocolla starting the word ticks its ice, a Corundum its burn; nowhere else, never on
    /// an enemy already carrying it, and not at all under `noCastSpell`.
    #[test]
    fn a_gem_that_starts_the_word_adds_its_status_tick() {
        let mut w = world();
        // `OYCAACTPORLIGAHJ`.
        w.board[0].quality.material = Some("gemice".into());
        w.board[6].quality.material = Some("gemfire".into());
        let t = |i: &[usize]| i.iter().map(|&i| w.board[i].clone()).collect::<Vec<_>>();
        let m = Modifiers { burn_tick: 3, ..Modifiers::none() };
        let a = |m: &Modifiers, word, i: &[usize]| m.afflictions(&w.scorer, word, &t(i));
        assert_eq!(a(&m, "OC", &[0, 2]), Afflictions { ice: 1, ..Default::default() });
        assert_eq!(a(&m, "TO", &[6, 0]), Afflictions { burn: 3, ..Default::default() });
        assert_eq!(a(&m, "CO", &[2, 0]).total(), 0, "a Chrysocolla last freezes nothing");
        let iced = Modifiers { ice_tick: 0, ..Modifiers::none() };
        assert_eq!(a(&iced, "OC", &[0, 2]).total(), 0, "already iced");
        let muted = Modifiers { tile_spells: None, ..Modifiers::none() };
        assert_eq!(a(&muted, "OC", &[0, 2]).total(), 0, "noCastSpell");
    }

    /// The tick is read from the enemy: flat, doubled while weak, nothing on top of its own.
    #[test]
    fn the_ice_and_burn_ticks_read_the_enemy() {
        let mods = |enemy: &str| {
            let save = crate::game::save::parse(&format!(
                "return {{ rpg = {{ player = {{ gearFlags = {{}} }}, enemy = {{ {enemy} }} }} }}"
            ))
            .unwrap();
            Modifiers::from_save(&game_dir(), &save, 16).unwrap().0
        };
        let m = mods("health = 5");
        assert_eq!((m.ice_tick, m.burn_tick), (1, 1), "unknown maximum health burns for 1");
        let m = mods("health = 5, maxHealth = 40, statusEffects = { iceWeak = 1, burnWeak = 1 }");
        assert_eq!((m.ice_tick, m.burn_tick), (2, 5), "weak: doubled, and 40/8");
        let m = mods("health = 5, statusEffects = { ice = 3, burnImmune = 1 }");
        assert_eq!((m.ice_tick, m.burn_tick), (0, 0));
        assert!(m.tile_spells.is_some());
    }

    /// A word its own tiles make an arcane spell of is boosted, before the weapon and the status;
    /// `noCastSpell` turns it off.
    #[test]
    fn a_chrysocolla_word_is_boosted_by_adept() {
        let mut w = world();
        w.board[0].quality.material = Some("gemice".into());
        let t = |i: &[usize]| i.iter().map(|&i| w.board[i].clone()).collect::<Vec<_>>();
        let adept = crate::game::save::parse(
            "return { rpg = { player = { gearFlags = { arcaneSpellEffectBonus5 = 10 } } } }",
        )
        .unwrap();
        let m = Modifiers {
            tile_spells: Some(crate::spells::Power::from_save(&adept)),
            ..Modifiers::none()
        };
        let base = scored(&w.scorer, &Modifiers::none(), "COOP", &t(&[2, 0, 8, 7]), None, 0, 4);
        let at = |m: &Modifiers, word, i: &[usize]| scored(&w.scorer, m, word, &t(i), None, 0, 4);
        // `COOP` ends on P, so the Chrysocolla O is mid-word and does nothing.
        assert_eq!(at(&m, "COOP", &[2, 0, 8, 7]), base);
        let plain = at(&Modifiers::none(), "OPT", &[0, 7, 6]) - 1;
        assert_eq!(
            at(&m, "OPT", &[0, 7, 6]),
            crate::spells::Power::from_save(&adept).boost(plain, crate::spells::School::Arcane) + 1,
            "boosted, plus the ice"
        );
        let muted = Modifiers { tile_spells: None, ..Modifiers::none() };
        assert_eq!(at(&muted, "OPT", &[0, 7, 6]), plain, "noCastSpell: neither");
    }

    /// The user's rule, 2026-11-07: a kill that does not need the board's gems keeps them, as it
    /// keeps gold. With a Chrysocolla on one of the two Os, the best kill spells its O on the other
    /// one; a Diamond is kept the same way at full health, and spent once there is health to heal.
    #[test]
    fn a_kill_that_does_not_need_a_gem_keeps_it() {
        let mut w = world();
        // `OYCAACTPORLIGAHJ`: Os at 0 and 8.
        let goal = Goal::RankedKill { need: 3 };
        let top_spends = |w: &World, tile: usize| {
            let list = rank_all(w, &Modifiers::none(), goal);
            let top = list.iter().find(|c| c.tier == Tier::InBand).expect("a kill");
            (top.word.clone(), top.typed.tiles.contains(&tile))
        };
        for gem in ["gemice", "gemfire", "gemheal"] {
            w.board[0].quality.material = Some(gem.into());
            let (word, spent) = top_spends(&w, 0);
            assert!(!spent, "{gem}: {word} should keep it");
        }
        // Injured: the Diamond's heal is wanted, so a word that ends on it and heals comes first.
        w.picking.prefs.room = crate::pick::Room { health: 10, ..Default::default() };
        let (word, spent) = top_spends(&w, 0);
        assert!(spent, "an injured player spends the Diamond: {word}");
    }

    /// The user, 2026-11-07: when another tile carries the same letter, a word is not shown on the
    /// one worth keeping. The user's chest board has a plain F (13, `silver3`, 3 points) and a
    /// Chrysocolla F (14, 3 points): mid-word the gem does nothing, so the two placements of a word
    /// tie on everything — and in a tier the ranking does not order (here every word overshoots a
    /// `1..=1` scare), only what they spend can tell them apart.
    #[test]
    fn a_letter_is_shown_on_the_plain_twin_of_a_kept_tile() {
        let w = world();
        let save = crate::game::save::parse(
            &std::fs::read_to_string("tests/fixtures/combatSaveData-chest-chrysocolla.lua")
                .unwrap(),
        )
        .unwrap();
        let board = crate::turn::tiles_of(&save);
        let geom = Geometry::from_save(&save, board.len()).geometry;
        assert_eq!(board[14].quality.material.as_deref(), Some("gemice"));
        assert_eq!(w.scorer.tile_score(&board[13]), w.scorer.tile_score(&board[14]), "twins");
        let goal = Goal::Scare { need: 1, below: 2 };
        let list =
            ranked(&w.dict, &w.scorer, &board, &geom, &Modifiers::none(), goal, &w.picking, 8);
        let mid_f: Vec<&Candidate> = list
            .iter()
            .filter(|c| c.word.matches('F').count() == 1)
            .filter(|c| !c.word.starts_with('F') && !c.word.ends_with('F'))
            .collect();
        assert!(mid_f.len() > 5, "the fixture needs words with an F inside");
        for c in mid_f {
            assert_eq!(c.tier, Tier::Overshoot, "{}", c.word);
            assert!(!c.typed.tiles.contains(&14), "{} spells its F on the Chrysocolla", c.word);
        }
    }

    /// A toxin tile adds 3 per serrated tile, bleed kinds included, but only when it starts the
    /// word; `toxinWeak` doubles it and `toxinImmune` zeroes it.
    #[test]
    fn a_toxin_tile_that_starts_the_word_adds_three_per_serrated_tile() {
        let mut w = world();
        // `OYCAACTPORLIGAHJ`.
        w.board[0].quality.material = Some("electrumtoxin".into());
        w.board[1].quality.material = Some("bronzebleed".into());
        let t = |i: &[usize]| i.iter().map(|&i| w.board[i].clone()).collect::<Vec<_>>();
        let m = Modifiers::none();
        assert_eq!(m.toxin(&w.scorer, "OC", &t(&[0, 2])), 3, "the toxin tile is serrated itself");
        assert_eq!(m.toxin(&w.scorer, "OY", &t(&[0, 1])), 6, "a bleed tile is serrated too");
        assert_eq!(m.toxin(&w.scorer, "YO", &t(&[1, 0])), 0, "not first, not active");
        assert_eq!(
            Modifiers { toxin_scale: 2, ..Modifiers::none() }.toxin(&w.scorer, "OY", &t(&[0, 1])),
            12
        );
        assert_eq!(
            Modifiers { toxin_scale: 0, ..Modifiers::none() }.toxin(&w.scorer, "OY", &t(&[0, 1])),
            0
        );
    }

    /// Toxin is paid before the enemy acts, so it counts as damage everywhere a goal looks,
    /// including a free-turn hold's ceiling: a word that would only reach it with its toxin
    /// overshoots.
    #[test]
    fn toxin_counts_toward_a_hold_ceiling() {
        let mut w = world();
        w.board[0].quality.material = Some("electrumtoxin".into());
        // The board's other O would let the word start on a plain O and stay under — see
        // `a_ceiling_starts_the_word_on_the_plain_twin` — so this one has a single O.
        w.board[8].letter = "X".into();
        let mods = Modifiers { stall: true, ..Modifiers::none() };
        // The same enemy, immune: the direct hit alone.
        let immune = Modifiers { toxin_scale: 0, ..Modifiers { stall: true, ..Modifiers::none() } };
        let open = Goal::Hold { below: 1000, lethal: 1000 };
        let o = rank_all(&w, &immune, open)
            .into_iter()
            .find(|c| c.typed.tiles.first() == Some(&0))
            .expect("some word starts on the O");
        // A ceiling the direct hit stays under and the toxin does not.
        let below = o.score + 2;
        let goal = Goal::Hold { below, lethal: below };
        let tier = |m: &Modifiers| {
            rank_all(&w, m, goal)
                .into_iter()
                .find(|c| c.word == o.word && c.typed.tiles == o.typed.tiles)
                .unwrap()
                .tier
        };
        assert_eq!(tier(&immune), Tier::Under, "{} stays under on its direct hit", o.word);
        assert_ne!(tier(&mods), Tier::Under, "{} would finish it with toxin", o.word);
    }

    /// A click can start a word on either of two O tiles, so the hardest hit starts it on the toxin
    /// one even though typing would take the other: `OYCAACTPORLIGAHJ`, where typing an O takes
    /// tile 0 and the toxin is on tile 8. The user's report, 2026-11-06: `BREAKABLY` with its
    /// serrated tile on the second B.
    #[test]
    fn the_hardest_hit_starts_on_the_toxin_twin() {
        let mut w = world();
        w.board[8].quality.material = Some("electrumtoxin".into());
        let list = rank_all(&w, &Modifiers::none(), Goal::MaxDamage);
        let one_o: Vec<&Candidate> = list
            .iter()
            .filter(|c| c.word.starts_with('O') && c.word.matches('O').count() == 1)
            .collect();
        assert!(!one_o.is_empty(), "the board makes words starting on an O");
        for c in one_o {
            assert_eq!(c.typed.tiles.first(), Some(&8), "{} starts on the toxin O", c.word);
        }
        // Diggle plays from `search`, and is handed the placement it scored.
        let out = search(
            &w.dict,
            &w.scorer,
            &w.board,
            &w.geom,
            &Modifiers::none(),
            Goal::MaxDamage,
            &w.picking,
            8,
        );
        let best = out.best.expect("something is playable");
        let listed = list.iter().find(|c| c.word == best.word).unwrap();
        assert_eq!(best.typed, listed.typed, "{}: the tiles the list shows", best.word);
    }

    /// And the other way: where the toxin would break a hold's ceiling, the same word starts on the
    /// plain O and stays under, rather than being listed as an overshoot.
    #[test]
    fn a_ceiling_starts_the_word_on_the_plain_twin() {
        let mut w = world();
        w.board[0].quality.material = Some("electrumtoxin".into());
        let open = Goal::Hold { below: 1000, lethal: 1000 };
        let immune = Modifiers { toxin_scale: 0, stall: true, ..Modifiers::none() };
        let o = rank_all(&w, &immune, open)
            .into_iter()
            .find(|c| c.typed.tiles.first() == Some(&0) && c.word.matches('O').count() == 1)
            .expect("some word starts on the O");
        let below = o.score + 2;
        let goal = Goal::Hold { below, lethal: below };
        let mods = Modifiers { stall: true, ..Modifiers::none() };
        let c = rank_all(&w, &mods, goal).into_iter().find(|c| c.word == o.word).unwrap();
        assert_eq!(c.typed.tiles.first(), Some(&8), "{} starts on the plain O", o.word);
        assert_eq!(c.tier, Tier::Under, "{} stays under", o.word);
    }

    /// Every word short of the goal, in the advisor's order: hardest first, and between two that hit
    /// equally hard, the one the shared ranking prefers. On a free-turn hold, a word that deals
    /// nothing is just the weakest hit and comes last.
    fn assert_hardest_then_ranked(list: &[Candidate], mods: &Modifiers) -> usize {
        let short = |c: &&Candidate| matches!(c.tier, Tier::Under | Tier::Stall | Tier::Fallback);
        let rows: Vec<&Candidate> = list.iter().filter(short).collect();
        let mut ties = 0;
        for p in rows.windows(2) {
            let (a, b) = (p[0], p[1]);
            let harder = mods.hits_harder((&a.word, a.score), (&b.word, b.score));
            assert_ne!(harder, std::cmp::Ordering::Less, "{} above the harder {}", a.word, b.word);
            if harder == std::cmp::Ordering::Equal && a.tier == b.tier {
                let (ra, rb) = (a.rank.unwrap(), b.rank.unwrap());
                assert!(
                    !rb.better_than(&ra),
                    "{} ranks above {} at the same damage",
                    b.word,
                    a.word
                );
                ties += ra.better_than(&rb) as usize;
            }
        }
        ties
    }

    /// The user, 2026-11-04: a stall *should behave the same as a turn where none of the
    /// candidates kill the target*, and both play the hardest hit with the ranking between equals.
    /// The fixture's braced T gives armour to the words that end on it, which is what separates
    /// words of equal damage.
    #[test]
    fn short_of_the_goal_the_hardest_hit_wins_and_the_rank_breaks_ties() {
        let mut w = world();
        w.board[6].quality.material = Some("woodbraced".into());
        w.picking.prefs =
            w.picking.prefs.with_room(crate::pick::Room { armour: 100, ..Default::default() });

        // Nothing kills a 1000-health enemy.
        let none = Modifiers::none();
        let kill = Goal::RankedKill { need: 1000 };
        let list = rank_all(&w, &none, kill);
        assert!(list.iter().all(|c| c.tier == Tier::Fallback));
        assert!(assert_hardest_then_ranked(&list, &none) > 0, "the fixture must have a ranked tie");
        let out = search(&w.dict, &w.scorer, &w.board, &w.geom, &none, kill, &w.picking, 8);
        let best = out.choice_for(kill).unwrap();
        let head = &list[0];
        assert_eq!(
            (best.score, &head.rank),
            (head.score, &list.iter().find(|c| c.word == best.word).unwrap().rank),
            "Diggle plays the head, or one tied with it"
        );

        // A free-turn hold: the same order under the ceiling, the zero-damage words last.
        let stall = Modifiers { stall: true, ..Modifiers::none() };
        let hold = Goal::Hold { below: 9, lethal: 9 };
        let list = rank_all(&w, &stall, hold);
        assert_hardest_then_ranked(&list, &stall);
        let first_nothing = list.iter().position(|c| c.tier == Tier::Stall);
        let last_chip = list.iter().rposition(|c| c.tier == Tier::Under);
        if let (Some(n), Some(c)) = (first_nothing, last_chip) {
            assert!(c < n, "a word that deals nothing is the weakest hit");
        }
        let out = search(&w.dict, &w.scorer, &w.board, &w.geom, &stall, hold, &w.picking, 8);
        let chip = out.choice_for(hold).unwrap();
        let mine = list.iter().find(|c| c.word == chip.word).unwrap();
        assert_eq!(mine.score, list[0].score, "Diggle's chip is as hard as the list's head");
        assert!(!list[0].rank.unwrap().better_than(&mine.rank.unwrap()), "and ranks with it");
    }

    /// Diggle's own search, not just the advisor's list, takes the Silver chain coif's armour into
    /// its ranking: of the kills on `OYCAACTPORLIGAHJ`, whose `Y` and `H` are silver by default
    /// material, the word it plays gives as much coif armour as any kill can, and some kill gives
    /// none.
    #[test]
    fn diggle_plays_the_kill_that_gives_coif_armour() {
        let mut w = world();
        let need = 3;
        let goal = Goal::RankedKill { need };
        w.picking.prefs = w.picking.prefs.with_room(crate::pick::Room {
            armour: 100,
            silver_armour: 1,
            ..Default::default()
        });
        let mods = Modifiers::none();
        let band: Vec<i64> = rank_all(&w, &mods, goal)
            .iter()
            .filter(|c| c.tier == Tier::InBand)
            .map(|c| c.rank.unwrap().gain)
            .collect();
        let most = *band.iter().max().unwrap();
        assert!(most > 0 && band.iter().any(|g| *g == 0), "the fixture must separate them: {most}");

        let out = search(&w.dict, &w.scorer, &w.board, &w.geom, &mods, goal, &w.picking, 8);
        let played = out.choice_for(goal).expect("a kill is on the board");
        let typed = Typist::new(&w.board, &w.geom).type_word(&played.word).unwrap();
        let gain = crate::pick::gain(&w.board, &typed.tiles, &w.scorer, w.picking.prefs.room);
        assert_eq!(gain, most, "Diggle played {} for {gain} armour, not {most}", played.word);
    }

    /// The advisor's top row is the word Diggle would play, or one it cannot tell apart from it.
    ///
    /// A tie is allowed because Diggle's own winner among equals depends on thread scheduling: the
    /// same tier and score, and for an in-band word, neither Rank better than the other.
    #[test]
    fn ranked_head_matches_choice_for_on_every_fixture() {
        let mut cases: Vec<(String, World, Modifiers, Goal)> = Vec::new();
        for goal in [
            Goal::RankedKill { need: 3 },
            Goal::RankedKill { need: 1000 },
            Goal::Scare { need: 4, below: 7 },
            Goal::Scare { need: 1000, below: 1001 },
            Goal::Scare { need: 0, below: 1 },
            Goal::MaxDamage,
        ] {
            cases.push((format!("crypt {goal:?}"), world(), Modifiers::none(), goal));
        }
        // The rapier, where Diggle's search and the list must agree on reach and on the share rule.
        for (health, armour) in [(2, 200), (400, 600), (5, 3)] {
            let goal = Goal::RankedKill { need: health + armour + DAMAGE_BUFFER };
            cases.push((
                format!("rapier {health}+{armour}"),
                world(),
                rapier(health, armour),
                goal,
            ));
            let goal = Goal::Scare { need: armour + 1, below: health + armour };
            cases.push((
                format!("rapier scare {health}+{armour}"),
                world(),
                rapier(health, armour),
                goal,
            ));
        }
        cases.push(("rapier max damage".into(), world(), rapier(5, 3), Goal::MaxDamage));
        let letters = crate::letters::Weights::load(&game_dir()).unwrap();
        for name in ["combatSaveData-crypt-l0.lua", "combatSaveData-l1-depleted.lua"] {
            let src = std::fs::read_to_string(format!("tests/fixtures/{name}")).unwrap();
            let cs = crate::game::save::parse(&src).unwrap();
            let plan = crate::turn::plan(
                &cs,
                &game_dir(),
                &letters,
                &mut Default::default(),
                Default::default(),
            )
            .unwrap()
            .unwrap();
            let fw = World {
                dict: Dictionary::load(&game_dir()).unwrap(),
                scorer: Scorer::new(&game_dir()).unwrap(),
                board: plan.tiles,
                geom: plan.geom,
                picking: plan.picking,
            };
            cases.push((name.to_string(), fw, plan.mods, plan.goal));
        }
        for (label, w, mods, goal) in &cases {
            let list = rank_all(w, mods, *goal);
            let out = search(&w.dict, &w.scorer, &w.board, &w.geom, mods, *goal, &w.picking, 8);
            let Some(chosen) = out.choice_for(*goal) else {
                assert!(
                    list.is_empty(),
                    "{label}: Diggle has no word but the list has {}",
                    list.len()
                );
                continue;
            };
            let head = &list[0];
            if head.word == chosen.word {
                continue;
            }
            let twin =
                list.iter().find(|c| c.word == chosen.word).expect("Diggle's word is listed");
            assert_eq!(
                head.tier, twin.tier,
                "{label}: head {} vs Diggle's {}",
                head.word, chosen.word
            );
            match (head.rank, twin.rank) {
                // Rank does not carry the score, so two tied band words may score differently.
                (Some(a), Some(b)) => assert!(
                    !a.better_than(&b) && !b.better_than(&a),
                    "{label}: {} and {} are not tied",
                    head.word,
                    chosen.word
                ),
                _ => assert_eq!(
                    mods.hits_harder((&head.word, head.score), (&chosen.word, chosen.score)),
                    std::cmp::Ordering::Equal,
                    "{label}: head {} ({}) vs {} ({})",
                    head.word,
                    head.score,
                    chosen.word,
                    chosen.score
                ),
            }
        }
    }
}
