//! Choosing *which* killing word to play, when more than one kills.
//!
//! [`crate::search::Goal::FirstKill`] took whichever lethal word a thread happened to find first.
//! That is the right answer to "end the exchange" and the wrong answer to "play the fight", because
//! the word also decides what the board looks like afterwards and what the turn is worth beyond the
//! damage. Four things separate two words that both kill:
//!
//! 1. **A wood-only kill pays out**, when the gear for it is held and the payout is not wasted.
//! 2. **Gold and wildcards are held back.** A board's gold tiles are the damage it has in reserve for
//!    the turn a big hit is needed, so a kill that spends one where a plainer word would also have
//!    killed has thrown that reserve away. See [`hoarded`].
//! 3. **Hazard tiles want to reach the bottom.** `tileboard.lua:178` is the game telling the player
//!    so directly: *"I'm sorry. Your special tile got unlucky. Get it to the bottom to get rid of
//!    it."* A `!` tile *"falls off if it's at the bottom of the board at the start of your turn"*
//!    (`:180`), so clearing tiles beneath one is how it leaves.
//! 4. **What is left should look like a board you can play.** See [`crate::letters`].
//!
//! In that order, and the order is the point: 1 and 2 are about value, 3 and 4 are hygiene. A
//! tiebreak never outranks a payout, and none of the four ever outranks killing.
//!
//! ## Everything here is a tiebreak among words that already kill
//!
//! None of this decides *whether* to kill. A candidate that does not kill never reaches these
//! functions, and no amount of tidy letters or falling hazards makes a non-lethal word preferable to
//! a lethal one.

use crate::letters::{self, Target, ALPHABET};
use crate::observe::board::Tile;
use crate::score::Scorer;

/// The material prefix that counts as wood.
///
/// A prefix rather than an equality test because the Targe family introduces **braced wood**
/// (`tileRefillSpecialWoodbraced`), which is wood for the purposes of a wood-only kill and is not
/// spelled `wood`. Matching the prefix covers both and any further wood variant the game adds.
const WOOD: &str = "wood";

/// The material a bomb tile carries (`rpg/effects/material/bomb.lua`).
const BOMB: &str = "bomb";

/// What the player's gear and condition make worth chasing this turn.
///
/// Derived once per turn rather than per candidate: it depends on the save, not on the word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Preferences {
    /// Is a wood-only kill worth constraining the word for?
    pub wood_only_pays: bool,
    /// Damage at which a killing blow's overkill covers the **whole** health deficit, if the
    /// Well-Rested heal is available and wanted. `None` when there is no heal to collect.
    ///
    /// `lethal + 2 * missing`, because the heal is `floor(overkill/2)` capped at the deficit
    /// (`rpgview.lua:1195-1196`). See [`Rank::heals_fully`] for why this is a rank and not a floor.
    pub heal_at: Option<i64>,
    /// The damage a **blow** must reach to be the kill, toxin left out — the goal's own threshold
    /// on a kill goal, `None` otherwise. [`Rank::wood_only`] counts only a blow that clears it:
    /// an enemy the poison finishes dies with no word (`rpgview.enemyDeathChecks`,
    /// `rpgview.lua:1152-1170`), so the wood-kill gear pays nothing — the user's report,
    /// 2026-11-06, of a buckler heal listed for a poison kill. [`rank`]'s `damage` is the blow's.
    pub kill_at: Option<i64>,
    /// How much of what a word can give the player would land — see [`gain`].
    pub room: Room,
    /// The suffix links that would queue infinitive tiles — see [`Infinitives`].
    pub infinitives: Infinitives,
    /// Ornate wooden wildcards queued per run of repeated letters in a word made only of wood: the
    /// value of `onSubmitWoodQueueWoodornateWildcardPerRun`, the Bookkeepers bassanello, zero when
    /// it is not worn. See [`wildcards_queued`].
    pub wildcards_per_run: i64,
}

/// The gear that turns a submitted word into new or better tiles: the suffix links that queue
/// infinitives, and the flags that upgrade tiles.
///
/// ## Queuing
///
/// Each link is an `onSubmitSuffix<FROM>to<TO>` flag. Submitting a word whose suffix is `FROM`
/// queues `TO` onto the board (`utils/effects.lua:126-140`): one in five times as a single tile,
/// gold or `silver3`, else letter by letter (`ui/objects/tile.lua:377-392`). It runs `onSubmit`, so
/// the word need not kill. The suffix is `cacheSuffix`'s, the longest registered one strictly
/// shorter than the word (`utils/effects.lua:106-118`), which is [`crate::gear::table::SUFFIXES_LONGEST_FIRST`].
///
/// The game has eight links ([`LINKS`], `rpg/effects/modifiers/onSubmitSuffix*.lua`), and every one
/// is read from `gearFlags`, whichever item grants it — the user, 2026-11-07, *any queuing links*:
///
/// ```text
///   Infinitives gauntlet  suffixGauntlet*  🧤  ING>ED ED>ES ES>LY LY>ING
///   Scales of justice     scalesOfJustice  ⚖️  ED>ES ES>LY LY>ING ING>ER ER>EST EST>ED
///   the ER/EST gear       (below)          🔁  ER>EST and EST>ER, by item
/// ```
///
/// (`items/infinitives.lua:214-260,271-348`, `items/scalesofjustice.lua:25-114`,
/// `items/erestgear.lua`: Ostrich feather, Heart of gold, and the Unbalanced, Imbalanced and
/// truth scales.) The item decides only the mark and the name the Advisor shows. A link can be
/// switched off by an `…Inactive` status until a shrine wakes it, but the game leaves inactive flags
/// out of `gearFlags` (`rpgview.lua:2458`), so reading the flags is enough.
///
/// ## Upgrading
///
/// On submit, a word ending in a flag's suffix upgrades that many random tiles one material stage,
/// and gilds as many more as could not be upgraded (`wordboard.lua:664-684`): `submitSuffixUpgrade`
/// on -ED -ES -LY -ING, `submitERESTupgrade` on -ER and -EST, `submitERupgrade` and
/// `submitESTupgrade` on one each. A seven-letter word gilds `submit7gild` tiles, the Gilding
/// bastard sword's (`wordboard.lua:655-663`, `items/lengthgear.lua`). See [`Infinitives::with_upgrades`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Infinitives {
    /// Live links, as bits over [`LINKS`].
    live: u8,
    /// The item the links come from, for the Advisor's mark: see [`InfinitiveSource`].
    pub source: Option<InfinitiveSource>,
    /// Tiles upgraded by a word ending -ED, -ES, -LY or -ING, by one ending -ER, and by one ending
    /// -EST.
    upgrades: [i64; 3],
    /// Tiles a seven-letter word gilds (`submit7gild`).
    gild7: i64,
}

/// Every suffix link the game has, `(FROM, TO)`: one modifier file each.
pub const LINKS: [(&str, &str); 8] = [
    ("ING", "ED"),
    ("ED", "ES"),
    ("ES", "LY"),
    ("LY", "ING"),
    ("ING", "ER"),
    ("ER", "EST"),
    ("EST", "ED"),
    ("EST", "ER"),
];

/// The ER/EST gear's keys (`items/erestgear.lua`).
const ER_EST_GEAR: [&str; 6] = [
    "ostrichFeather",
    "heartOfGold",
    "balancingScales",
    "imbalancedScalesER",
    "imbalancedScalesEST",
    "scalesOfBalance",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfinitiveSource {
    Gauntlet,
    Justice,
    /// The ER/EST gear: Ostrich feather, Heart of gold, the lesser scales.
    Comparative,
}

impl InfinitiveSource {
    pub fn emoji(self) -> &'static str {
        match self {
            InfinitiveSource::Gauntlet => "🧤",
            InfinitiveSource::Justice => "⚖️",
            InfinitiveSource::Comparative => "🔁",
        }
    }

    /// What the Advisor calls it.
    pub fn name(self) -> &'static str {
        match self {
            InfinitiveSource::Gauntlet => "Infinitives gauntlet",
            InfinitiveSource::Justice => "Scales of justice",
            InfinitiveSource::Comparative => "ER/EST gear",
        }
    }
}

impl Infinitives {
    /// From the save's `gear` list and its `gearFlags`. The scales are the gauntlet plus the scales
    /// of balance, crafted (`items/scalesofjustice.lua:40-43`), so they win when both could match.
    pub fn from_gear<'a>(
        gear: impl IntoIterator<Item = &'a str>, has_flag: impl Fn(&str) -> bool,
    ) -> Self {
        let gear: Vec<&str> = gear.into_iter().collect();
        let source = if gear.contains(&"scalesOfJustice") {
            Some(InfinitiveSource::Justice)
        } else if gear.iter().any(|g| g.starts_with("suffixGauntlet")) {
            Some(InfinitiveSource::Gauntlet)
        } else if gear.iter().any(|g| ER_EST_GEAR.contains(g)) {
            Some(InfinitiveSource::Comparative)
        } else {
            None
        };
        let live = LINKS
            .iter()
            .enumerate()
            .filter(|(_, (from, to))| has_flag(&format!("onSubmitSuffix{from}to{to}")))
            .fold(0u8, |bits, (i, _)| bits | 1 << i);
        Infinitives { live, source, ..Infinitives::default() }
    }

    /// The tile upgrades a word earns, from each flag's value (`flag`, 0 when not held): see
    /// **Upgrading** above.
    pub fn with_upgrades(self, flag: impl Fn(&str) -> i64) -> Self {
        let both = flag("submitERESTupgrade");
        Infinitives {
            upgrades: [
                flag("submitSuffixUpgrade"),
                both + flag("submitERupgrade"),
                both + flag("submitESTupgrade"),
            ],
            gild7: flag("submit7gild"),
            ..self
        }
    }

    /// How many infinitives submitting `word` queues: one per live link from its suffix.
    pub fn created(&self, word: &str) -> i64 {
        self.effect(word).map_or(0, |e| e.queues.iter().flatten().count() as i64)
    }

    /// How many tiles submitting `word` upgrades or gilds.
    pub fn upgraded(&self, word: &str) -> i64 {
        self.effect(word).map_or(0, |e| e.upgrades + e.gilds)
    }

    /// What submitting `word` does with this gear: the suffixes it queues, and the tiles it
    /// upgrades and gilds. `None` when it does none of them.
    pub fn effect(&self, word: &str) -> Option<TileEffect> {
        let word = word.to_ascii_uppercase();
        let suffix = crate::gear::table::SUFFIXES_LONGEST_FIRST
            .iter()
            .find(|s| word.len() > s.len() && word.ends_with(*s))
            .copied();
        let mut queues = [None; 2];
        let targets = LINKS
            .iter()
            .enumerate()
            .filter(|(i, (from, _))| self.live & (1 << i) != 0 && Some(*from) == suffix)
            .map(|(_, (_, to))| *to);
        for (slot, to) in queues.iter_mut().zip(targets) {
            *slot = Some(to);
        }
        let upgrades = match suffix {
            Some("ER") => self.upgrades[1],
            Some("EST") => self.upgrades[2],
            Some(_) => self.upgrades[0],
            None => 0,
        };
        // `word:len() == 7` (`wordboard.lua:655`): the word's letters, which a ligature tile
        // carries several of.
        let gilds = if word.len() == 7 { self.gild7 } else { 0 };
        let effect = TileEffect { suffix, queues, upgrades, gilds };
        (queues[0].is_some() || upgrades > 0 || gilds > 0).then_some(effect)
    }
}

/// What submitting a word does with the tile gear — see [`Infinitives::effect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TileEffect {
    /// The word's suffix, `ING`, if it has one.
    pub suffix: Option<&'static str>,
    /// The suffixes whose tiles it queues, `ED`: one per live link from [`TileEffect::suffix`].
    pub queues: [Option<&'static str>; 2],
    /// Random tiles its suffix upgrades one material stage, or gilds where none can go higher.
    pub upgrades: i64,
    /// Random tiles it gilds for being seven letters long.
    pub gilds: i64,
}

/// How much a word's armour and healing can actually give, this turn.
///
/// Default is all zero, which makes [`gain`] worth nothing: the safe direction for a caller that
/// does not know, because then the ranking is what it was before gain existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Room {
    /// Armour the player can still gain before the cap, `ceil(maxHealth / (maxArmourHalved and 2 or 1))`
    /// (`overworld.lua:18`).
    pub armour: i64,
    /// Health missing.
    pub health: i64,
    /// Bleed stacks on the player, which eat a Diamond's heal and stop regen.
    pub bleed: i64,
    /// Armour per silver tile in any attacking word: the value of `onAttackGainArmourWordSilverCount`,
    /// the Silver chain coif (`items/armourgear.lua:4-29`), zero when it is not worn.
    pub silver_armour: i64,
}

impl Preferences {
    /// Reads the wood-kill gear out of `gearFlags` and decides whether any of it is worth
    /// constraining the word for **right now**.
    ///
    /// **Flags, not item names**, because flags are what the save carries. Five items touch this and
    /// they are not interchangeable:
    ///
    /// ```text
    ///   Wooden idol          woodenIdol             health
    ///   Wooden buckler       armourBucklerWood              armour
    ///   Wooden idol buckler  armourBucklerIdolWood  health  armour
    ///   Braced buckler       armourTargeIdol        health  armour  braced
    ///   Targe                armourTarge                            braced
    /// ```
    ///
    /// (`items/woodaugmentgear.lua:85,117,154`, `items/idoltarge.lua:4`,
    /// `items/specialtilegear.lua:4`.)
    ///
    /// ## Three payouts, three different reasons they might be worthless
    ///
    /// This used to treat health and armour as one thing behind a single "would the heal land"
    /// gate, which is wrong for every item that grants armour without health — including the
    /// **Wooden buckler**, the one a live run was actually carrying. Its description is
    /// *"Defeating an enemy with a word that only contains wooden tiles gives you 4 armour"*: no
    /// heal is involved, so gating it on being injured and not bleeding withheld it for two reasons
    /// that do not apply.
    ///
    /// - **`onWoodKillGainHealth`** heals 4 *"unless you're bleeding"*, and `rpgview.lua:1080` skips
    ///   the heal branch outright while `bleed` is set. At full health the heal is thrown away. So
    ///   it pays only when **injured and not bleeding**.
    /// - **`onWoodKillGainArmour`** is not a heal. Bleeding does not cancel it and full health does
    ///   not waste it — but armour caps at `ceil(maxHealth / (maxArmourHalved and 2 or 1))`
    ///   (`overworld.lua:18`), so it pays only while there is **room for more armour**.
    /// - **`onWoodKillQueueWoodbraced`** queues a braced wooden tile and cares about none of the
    ///   above. It pays **always**.
    ///
    /// Any one of the three is reason enough, so they are OR-ed rather than ranked: the word is
    /// constrained the same way whichever payout is collecting.
    pub fn from_flags(
        has_flag: impl Fn(&str) -> bool, injured: bool, bleeding: bool, armour_room: bool,
    ) -> Self {
        let heals = has_flag("onWoodKillGainHealth") && injured && !bleeding;
        let armours = has_flag("onWoodKillGainArmour") && armour_room;
        let braced = has_flag("onWoodKillQueueWoodbraced");
        Preferences { wood_only_pays: heals || armours || braced, ..Preferences::default() }
    }

    /// Records the damage at which the Well-Rested heal covers the whole deficit.
    ///
    /// Separate from [`Preferences::from_flags`] because the two answer to different things: the
    /// flags are gear, this is the player's health and charges, and only the search knows the
    /// enemy's. See [`Rank::heals_fully`].
    pub fn healing_at(self, damage: Option<i64>) -> Self {
        Preferences { heal_at: damage, ..self }
    }

    /// The same preferences for a word that is **not meant to kill**: a scare, a hold, a stall.
    ///
    /// Both payouts this clears are paid only on a killing blow. The wood gear's
    /// `onWoodKill*` flags fire on *"defeating an enemy with a word that only contains wooden
    /// tiles"*, and the Well-Rested heal is `floor(overkill/2)` of a kill (`rpgview.lua:1086`). A
    /// word that leaves the enemy standing collects neither, so ranking a scare by them prefers an
    /// all-wood word for a reward that never comes and spends the board's wood doing it. What the
    /// word itself gives — [`gain`], from its own tiles — is untouched, because that lands either way.
    pub fn without_kill_payouts(self) -> Self {
        Preferences { wood_only_pays: false, heal_at: None, kill_at: None, ..self }
    }

    /// Records how much armour and healing would land — see [`gain`].
    pub fn with_room(self, room: Room) -> Self {
        Preferences { room, ..self }
    }

    /// Records the suffix links that would queue infinitive tiles — see [`Infinitives`].
    pub fn with_infinitives(self, infinitives: Infinitives) -> Self {
        Preferences { infinitives, ..self }
    }

    /// Records the bassanello's wildcards per run — see [`wildcards_queued`].
    pub fn with_wildcards_per_run(self, wildcards_per_run: i64) -> Self {
        Preferences { wildcards_per_run, ..self }
    }
}

/// Everything the ranking needs that does not change between candidates.
///
/// Built once per turn and borrowed by every thread. The [`Target`] in particular is worth hoisting:
/// a board cannot change size mid-fight, so its target is computed once and read by every one of the
/// tens of thousands of candidates a scan considers.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Context {
    pub target: Target,
    pub prefs: Preferences,
    /// The gear that queues gems on submission — see [`GemQueue`].
    pub gems: GemQueue,
}

/// How good a lethal candidate is, beyond the fact that it kills.
///
/// Compared in field order: wood-only first, then what it spends from the board's good tiles, then
/// hazard fall, then deviation. Not `Ord`, because two fields are `f64` and a total order over floats
/// would be a lie — [`Rank::better_than`] is the comparison, and it is deliberately the only one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rank {
    /// How many **burning** tiles this word takes off the board. Higher is better, and it is the
    /// first thing asked.
    ///
    /// ## A count, not a threshold, and the dev had to correct me twice to get here
    ///
    /// The single-turn arithmetic says threshold. Damage is flat — `rpgview.lua:31,40` is
    /// `burn ~= 0 and ceil(maxHealth / (burnWeak and 8 or 16)) or 0`, so one burning tile costs
    /// exactly what six do — and `getBurningTileCount(true)` ignores tiles we have *selected*
    /// (`tileboard.lua:873`). So clearing four of five changes this turn's damage not at all, and I
    /// proposed ranking on "did we clear the last one".
    ///
    /// That is wrong over a fight, which is where the damage is taken. The dev, 2026-08-15:
    ///
    /// > let's say most of our entire board becomes burning because some enemy attacks can do that.
    /// > If we manage to clear out the majority of them, then the next turn has a very high chance
    /// > of clearing all burning tiles, whereas if we leave the majority of them and remove very
    /// > few, then next turn the chance of clearing all burning tiles is very low.
    ///
    /// A word is at most a handful of tiles and a board is sixteen, so on a heavily burning board
    /// **the threshold is unreachable this turn** — and a rule that only values reaching it values
    /// nothing at all, declines every partial clear, and never arrives at the turn where zero is
    /// possible. Burning tiles do not go away on their own quickly: `extra.burn` is a turn counter,
    /// and until it expires the tile keeps costing `ceil(maxHealth/16)` every single turn.
    ///
    /// Counting has the threshold inside it for free. Six of six scores higher than five of six, so
    /// a word that finishes the job still wins; when finishing is impossible, the count keeps
    /// driving toward the turn when it is not.
    ///
    /// **Above `wood_only`**, which is the ordering the dev's example is about. The wood payout is
    /// four armour, once. A burning board is three a turn at 36 maximum health, for as many turns as
    /// it takes to clear — so letting a one-off bonus outrank fire is how a run pays that toll
    /// repeatedly. And it costs nothing to prefer: every candidate being compared here has already
    /// cleared the damage threshold.
    ///
    /// ## Imperfect, kept deliberately, and post-MVP work
    ///
    /// The dev's call, 2026-08-15: keep it, and write down that it is not the finished rule. It
    /// rests on two assumptions, and **two items in the game break one each**:
    ///
    /// - **Burning tiles score nothing**, so eating them costs damage. `items/phoenixgear.lua` —
    ///   the **Phoenix feather** — is *"Burning and burnt tiles score half instead of nothing"*
    ///   (flag `bonusBurnTileMult`), plus 20% on fire-related words. Carrying it, fire is far
    ///   cheaper to eat and the count should probably weigh more, not less.
    /// - **A word is the only way off the board.** `items/alchemypieces.lua:159` — the
    ///   **Calcinatory** — is *"Activate to have burning and burnt tiles fall off of the bottom of
    ///   the tile board. Each that does has an additive 25% chance per alchemy piece to queue a gem
    ///   wildcard."* That inverts this field twice over: fire leaves for free without spending word
    ///   slots on it, and each burning tile becomes a *lottery ticket for a gem wildcard*, so eating
    ///   them in a word is destroying a resource. It is an activated item, and Diggle activates
    ///   nothing today — so carrying one would make this ranking actively wrong with no code here
    ///   aware of it.
    ///
    /// A fixed slot in a lexicographic order cannot express either. Neither can it express the case
    /// this ordering is weakest on: a single tile with one turn left burns out on its own, and four
    /// armour is the better take. Post-MVP, and see the task.
    pub fire_eaten: usize,
    /// The word leaves the enemy burning when it attacks, so its direct damage is halved:
    /// `floor(attackDamage * 0.5)` while `statusEffects.burn` is set (`rpgview.lua:1831`), which the
    /// estimate reads as more than one stack before the start-of-turn decay (`:1052-1053`). See
    /// [`crate::search::Modifiers::halves_attack`]. Higher is better.
    ///
    /// **Only where the enemy hits back**: the user, 2026-11-07, *on turns where we fail to kill an
    /// enemy, take into account the damage reduction from setting it on fire*, ranked like the
    /// burning tiles. So it is set only for a word that neither kills nor scares, on a turn the
    /// enemy can land on, and [`crate::pick::rank`] leaves it false: its callers in
    /// [`crate::search`] set it. On those turns the hardest hit comes first, so this, like every
    /// term here, decides only between words that hit equally hard.
    ///
    /// **A yes or no, not health saved.** The save does not carry the enemy's next attack — it keeps
    /// `attacksCycle` and the stance, and the damage is read from the enemy's definition when the
    /// cycle turns (`rpgview.lua:1971-1979`) — and two words that both halve it save the same.
    pub halves_attack: bool,
    /// Hazard tiles (`!` and bombs, see [`is_hazard`]) this word drops all the way to the bottom
    /// row, where a `!` falls off at the start of the next turn (`tileboard.lua:179`). Higher is
    /// better. See [`hazards_cleared`].
    ///
    /// **Above [`Rank::gain`]**, the user's call, 2026-11-04, on a board where every top kill ended
    /// on a braced E for 4 armour and the kills that brought the `!` down gave none. A `!` takes one
    /// of the board's tiles out of play for as long as it stands, so getting rid of it is worth more
    /// than one word's armour. Only reaching the bottom counts here; a partial fall is still the
    /// board-hygiene term [`Rank::hazard_fall`], below everything that pays.
    ///
    /// **Above [`Rank::wood_only`] and [`Rank::heals_fully`] too**, the user's call, 2026-11-07, on
    /// a board where every word under a gold-bordered D was wood only, so all of them earned the
    /// Woodsman tunic's 4 armour and every word that took the D out from under the `!` ranked below
    /// them: *the exclamation mark removal should be prioritized over armour gain*.
    pub hazards_cleared: usize,
    /// Every tile used was wood, and the gear pays for that. Higher is better.
    pub wood_only: bool,
    /// This blow overkills deeply enough for the Well-Rested heal to close the whole deficit.
    ///
    /// **Below `wood_only`, and that ordering is the dev's rule**, 2026-08-15: *first check if
    /// there's a wood-only kill that triggers both the Well-Rested heal and the armour bonus;
    /// otherwise go for the best wood-only kill, and only if that's not possible, go for the
    /// Well-Rested heal.* Two booleans in that order give exactly those four tiers:
    ///
    /// ```text
    ///   wood_only  heals_fully
    ///     true       true       both payouts -- the one to want
    ///     true       false      the gear's armour or health, no heal
    ///     false      true       the heal alone
    ///     false      false      an ordinary kill
    /// ```
    ///
    /// It is a **rank** rather than a threshold on purpose. Asking for the heal as a floor is what
    /// the search used to do, and a floor cannot express "I would rather have the wood word that
    /// heals nothing": every candidate under it is discarded before anything compares them. The
    /// floor drops to plain lethal whenever the wood payout is live, and this field carries the heal
    /// into the comparison instead — see [`crate::search::Goal::killing_blow`].
    pub heals_fully: bool,
    /// Health and armour this word gives the player, as far as they would land: see [`gain`].
    /// Higher is better.
    ///
    /// **Above every board-hygiene term and below the three before it** — the user's call,
    /// 2026-11-03, after `DAU` was listed above `FUD` for the same damage while only `FUD` ends on
    /// a braced tile and gives 4 armour. Below the fire count, which costs every turn the board
    /// burns (see [`Rank::fire_eaten`]), and below the wood-only and full-heal payouts, which the
    /// dev ordered and which already stand for gain of their own.
    pub gain: i64,
    /// The gold this word spends, each tile at its own worth ([`Scorer::tile_score`]): **lower** is
    /// better. See [`hoarded`] for why at worth: a `Q` is a bigger reserve than a `Z`.
    ///
    /// **The first of the tile terms**, the user's call, 2026-11-07: *default to gold being the
    /// highest priority*. Below health and armour, which the player is given outright.
    pub gold_spent: f64,
    /// The gems this word queues ([`GemQueue`]) less the gems it spends ([`gems_spent`]), one each.
    /// **Signed**; higher is better. See [`TileLedger`].
    ///
    /// **Above the infinitives**, the user's call, 2026-11-07, and net because *if the inputs
    /// require more tiles that we want to save than it outputs, that's bad*. A word that spends two
    /// Corundums to queue one scores one gem's loss here, and with no gem gear worn every gem spent
    /// is a loss here too, so two kills differing in gems are told apart before the infinitives.
    pub gems_net: f64,
    /// The wildcards this word queues from the Bookkeepers bassanello ([`wildcards_queued`]) less
    /// the wildcards it spends, one each. **Signed**; higher is better. See [`TileLedger`].
    ///
    /// **One signed figure, like the gems**, the user's call, 2026-11-07: *the bassanello's
    /// wildcard queued should be factored into the wildcard spent step, similar to the net gem
    /// gain*. That replaced two terms: wildcards spent (hoarding) and wildcards queued, the lowest
    /// tile gain, below the infinitives.
    ///
    /// **Above the infinitives**: the user's call the same day, on a board where `AXES` spent the
    /// gold X for an infinitive and outranked `DENS`, which spent nothing — *infinitives should no
    /// longer rank above hoarding*.
    ///
    /// Note this and `wood_only` do **not** subsume each other, though it looks as if they might.
    /// A wildcard's material is `wood0` (`rpg/effects/material/default.lua:4`), which `wood_only`
    /// accepts, so a wood-only kill can still be spending one.
    pub wildcards_net: f64,
    /// Tiles this word upgrades a material stage or gilds, from any gear that does it on submit:
    /// see [`Infinitives::upgraded`]. Higher is better.
    ///
    /// **Second-lowest of what a word gives**, the user's call, 2026-11-07: *tile upgrades (not
    /// just -er and -est; any gear that upgrade tiles) should be the second-lowest priority*. Above
    /// the board-tidiness terms, which the user chose over putting it truly last, where deviation,
    /// which almost never ties, would leave it nothing to decide.
    pub upgrades: i64,
    /// Infinitive tile sets this word queues, from any suffix link: see [`Infinitives`]. Higher is
    /// better.
    ///
    /// Below health and armour, the user's call, 2026-11-04; below every tile kept or queued and
    /// below the upgrades, 2026-11-07: *any queuing links get added as the least priority
    /// tiebreaker*. The lowest of what a word gives, above the board-tidiness terms only.
    pub infinitives: i64,
    /// Ligature tiles this word gets **rid** of. Higher is better. See [`shed`].
    ///
    /// The mirror image of the kept tiles ([`Rank::gold_spent`] and on), and ranked below them on
    /// the dev's call,
    /// 2026-08-17: *ranking the ash usage underneath hoarding works.* Gold is damage held in
    /// reserve and an ash is a tile that clogs the board, so the two want opposite treatment — but
    /// the reserve is worth more than the tidying, and a word must never burn a `Q` to take out the
    /// rubbish.
    pub shed: usize,
    /// Total slots every hazard tile on the board would fall by. Higher is better.
    pub hazard_fall: usize,
    /// Distance of the resulting board from its target letter distribution. **Lower** is better.
    pub deviation: f64,
}

impl Rank {
    /// Strictly better, by the stated priority order.
    ///
    /// The two payouts are compared before the hygiene term, and equality on both is what lets
    /// deviation decide. Written as an explicit chain rather than a tuple compare so that the
    /// direction of each term is visible at the point it is applied — `deviation` runs the other way
    /// from the other two, which is exactly the kind of thing a tuple hides.
    pub fn better_than(&self, other: &Rank) -> bool {
        if self.fire_eaten != other.fire_eaten {
            return self.fire_eaten > other.fire_eaten;
        }
        if self.halves_attack != other.halves_attack {
            return self.halves_attack;
        }
        if self.hazards_cleared != other.hazards_cleared {
            return self.hazards_cleared > other.hazards_cleared;
        }
        if self.wood_only != other.wood_only {
            return self.wood_only;
        }
        if self.heals_fully != other.heals_fully {
            return self.heals_fully;
        }
        if self.gain != other.gain {
            return self.gain > other.gain;
        }
        if self.gold_spent != other.gold_spent {
            return self.gold_spent < other.gold_spent;
        }
        if self.gems_net != other.gems_net {
            return self.gems_net > other.gems_net;
        }
        if self.wildcards_net != other.wildcards_net {
            return self.wildcards_net > other.wildcards_net;
        }
        if self.upgrades != other.upgrades {
            return self.upgrades > other.upgrades;
        }
        if self.infinitives != other.infinitives {
            return self.infinitives > other.infinitives;
        }
        if self.shed != other.shed {
            return self.shed > other.shed;
        }
        if self.hazard_fall != other.hazard_fall {
            return self.hazard_fall > other.hazard_fall;
        }
        self.deviation < other.deviation
    }
}

/// Health and armour the word gives the player, counting only what would land.
///
/// Two materials pay out when they are the **last** tile (both have `active = wordTiles[#wordTiles]
/// == tile`), and each counts its kind across the whole word:
///
/// - **Braced wood** (`rpg/effects/material/woodbraced.lua`): 4 armour per braced tile —
///   `countEachMaterialWithCat(wordTiles, 'wood', 'braced')`, and `woodbraced` is the only material
///   whose `wood` category is `braced` — up to [`Room::armour`].
/// - **A Diamond** (`rpg/effects/material/gemheal.lua`): with `val` = 3 per gem in the word (the three
///   `gem*` materials), it heals `max(0, val - bleed)`, takes `val` off the bleed, and adds `val`
///   regen. The heal counts up to [`Room::health`]. Regen heals 1 a turn and not while bleeding
///   (`rpgview.lua:1000-1002`), so it counts only if the bleed is gone, and only up to what is still
///   missing after the heal.
///
/// And one piece of gear, on every word whether it kills or not:
///
/// - **The Silver chain coif** (`onAttackGainArmourWordSilverCount`): `flagVal` armour per silver
///   tile in the word (`rpg/effects/modifiers/onAttackGainArmourWordSilverCount.lua:8,14`). It is
///   an `onAttack` effect, run by `rpgview.dealDamage` after every attack (`rpgview.lua:1247`), so
///   the word need not kill. A tile is silver if its material **or** its border has the `silver`
///   category (`utils/tiles.lua:97-104`) — see [`is_silver`]. Shares the armour cap with braced
///   wood, since both go through `affectPlayerArmour` (`rpgview.lua:164-166`).
///
/// Armour and health count the same, point for point. Its toxin removal is not counted.
pub fn gain(tiles: &[Tile], consumed: &[usize], scorer: &Scorer, room: Room) -> i64 {
    let g = gains(tiles, consumed, scorer, room);
    g.armour + g.health
}

/// What [`gain`] adds up: the armour and the health (heal and regen) a word gives, each only as far
/// as it would land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Gains {
    pub armour: i64,
    pub health: i64,
    /// Infinitive tile sets queued, and the gear queuing them: see [`Infinitives`]. Filled by the
    /// advisor, which has the word; [`gains`] leaves it empty.
    pub infinitives: i64,
    pub infinitives_from: Option<InfinitiveSource>,
    /// What the word does with the tile gear — queues, upgrades, gilds — for the Advisor: see
    /// [`Infinitives::effect`]. Filled by the advisor; [`gains`] leaves it empty.
    pub tiles: Option<TileEffect>,
    /// Wildcards queued: see [`wildcards_queued`]. Filled by the advisor, which has the word;
    /// [`gains`] leaves it at zero.
    pub wildcards: i64,
    /// Gems queued by a gem staff: see [`GemQueue`]. Filled by the advisor, which has the word;
    /// [`gains`] leaves it at zero.
    pub gems: i64,
    /// `!` marks this word clears off the board: see [`exclamations_cleared`]. Filled by the
    /// advisor, which reads the falloff gear from the save; [`gains`] leaves it at zero.
    pub exclamations: usize,
}

/// [`gain`], kept apart, for a caller that shows the two separately.
pub fn gains(tiles: &[Tile], consumed: &[usize], scorer: &Scorer, room: Room) -> Gains {
    let material = |i: &usize| tiles.get(*i).and_then(|t| scorer.material_name(t));
    let count = |kind: &dyn Fn(&str) -> bool| {
        consumed.iter().filter(|i| material(i).is_some_and(kind)).count() as i64
    };
    let silver = room.silver_armour
        * consumed.iter().filter(|&&i| tiles.get(i).is_some_and(|t| is_silver(t, scorer))).count()
            as i64;
    let g = match consumed.last().and_then(material) {
        Some("woodbraced") => {
            Gains { armour: 4 * count(&|m| m == "woodbraced"), ..Gains::default() }
        }
        Some("gemheal") => {
            let val = 3 * count(&|m| m.starts_with("gem"));
            let heal = (val - room.bleed).max(0).min(room.health.max(0));
            let regen = if room.bleed > val { 0 } else { val.min(room.health.max(0) - heal) };
            Gains { health: heal + regen, ..Gains::default() }
        }
        _ => Gains::default(),
    };
    Gains { armour: (g.armour + silver).min(room.armour.max(0)), ..g }
}

/// Whether a tile counts as silver: its material or its border has the `silver` category
/// (`tiles.hasCat`, `utils/tiles.lua:97-104`).
///
/// The materials that have it are `silver2`, `silver3`, `silverbleed`, `silvertoxin`, `electrum`,
/// `electrumbleed` and `electrumtoxin`, and the only border is `silver`
/// (`rpg/effects/material/*.lua`, `rpg/effects/border/silver.lua`). Matched by name prefix, as
/// [`wood_only`] matches wood. The letter's own material counts too: `G H Y B` are `silver2` and
/// `F K W V` are `silver3` by default (`rpg/effects/material/default.lua:18-25`).
pub fn is_silver(tile: &Tile, scorer: &Scorer) -> bool {
    tile.quality.border.as_deref() == Some("silver")
        || scorer
            .material_name(tile)
            .is_some_and(|m| m.starts_with("silver") || m.starts_with("electrum"))
}

/// Is this tile a hazard that wants to reach the bottom of its column?
///
/// Two kinds, named separately by the request and both true here:
///
/// - **Exclamation** — letter `!`, *"Unselectable tile. Falls off if it's at the bottom of the board
///   at the start of your turn"* (`tileboard.lua:180`).
/// - **Bomb** — material `bomb`, *"Inert bomb tile; will explode and destroy edge touching tiles if
///   detonated by something else"* (`rpg/effects/material/bomb.lua:12-13`).
///
/// Digit tiles are deliberately excluded even though `tileboard.lua:181` says a number *"acts like a
/// ! tile"*. They leave by counting down to zero and popping, not by reaching the bottom, so falling
/// does not get rid of one and counting it here would reward a move that achieves nothing.
pub fn is_hazard(tile: &Tile, scorer: &Scorer) -> bool {
    tile.letter == "!" || scorer.material_name(tile).is_some_and(|m| m == BOMB)
}

/// Total number of slots the board's hazard tiles would fall, if `consumed` were removed.
///
/// A tile falls by the number of removed tiles **below** it in its own column. `layout.rs:55` fixes
/// the direction: *"Column-major with row 1 at the bottom"*, so "below" is a strictly smaller row.
///
/// Summed over hazards rather than maximised over them, which is what makes two hazards in one
/// column worth twice as much as one — removing a single tile under both moves both, and that turn
/// has done twice the work.
///
/// A hazard that is itself consumed contributes nothing: it is gone, and a gone tile cannot fall.
/// That understates such a move rather than overstating it, which is the safe direction — the tile
/// left the board, which is the outcome the falling was for.
/// How many hazard tiles removing `consumed` brings down to the bottom row (row 1, `layout.rs:55`).
///
/// A hazard already on the bottom row is not counted: it falls off whatever is played, so no word
/// earns it. Nor is one the word consumes, which [`hazard_fall`] also leaves out.
pub fn hazards_cleared(
    tiles: &[Tile], geometry: &crate::geometry::Geometry, consumed: &[usize], scorer: &Scorer,
) -> usize {
    let removed: Vec<(usize, usize)> =
        consumed.iter().filter_map(|&i| geometry.position(i)).collect();
    tiles
        .iter()
        .enumerate()
        .filter(|(i, t)| is_hazard(t, scorer) && !consumed.contains(i))
        .filter_map(|(i, _)| geometry.position(i))
        .filter(|&(col, row)| {
            row > 1 && row - removed.iter().filter(|&&(c, r)| c == col && r < row).count() == 1
        })
        .count()
}

/// How the start of the player's turn takes `!` tiles off the board (`tileboard.onPlayerTurn`,
/// `tileboard.lua:1263-1278`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Falloff {
    /// The bottom rows a `!` falls off from: `1 + tileboard!bottom+` (`:1266`).
    pub rows: usize,
    /// `tileFalloffWildcardInstead`: a falling tile turns into a wildcard and stays put
    /// (`:1762-1777`), so nothing above it comes down.
    pub to_wildcard: bool,
}

impl Default for Falloff {
    fn default() -> Self {
        Falloff { rows: 1, to_wildcard: false }
    }
}

/// How many `!` marks playing `consumed` takes off the board that would otherwise stay — the
/// user's request, 2026-11-06, for the advisor's Gain column.
///
/// The game's rule, at the start of the next turn (`tileboard.lua:1263-1278`):
///
/// - in the bottom [`Falloff::rows`] of each column, every `!` and every digit tile falls off
///   (`isExclamationOrNumberTile`, `:1200-1202`), top of the zone first
///   (`haveTilesOnRowsFallOffIf`, `:1755-1790`) — **and again** while anything fell, so a `!`
///   that drops into the zone behind a fallen one goes too;
/// - then each `!!`, `!!!` or `!!!!` left in that zone loses one mark (`reduceDownMultiExclamation`,
///   `:1204-1221`, run by `doToBottomTiles`, `:1029-1038`).
///
/// A word helps by taking tiles out from under them, which brings them down (`layout.rs:55`: row 1
/// is the bottom). Counted as the marks that go with the word played **minus** the marks that go
/// whatever is played, so a `!` already at the bottom earns no word anything. A `!` is never in a
/// word itself — it is unselectable (`tileboard.lua:179`).
///
/// Not modelled: what the enemy's turn and the refill add before the check. Both land on top of a
/// column or on random tiles, so this is what the board does if the enemy leaves it alone.
pub fn exclamations_cleared(
    tiles: &[Tile], geometry: &crate::geometry::Geometry, consumed: &[usize], falloff: Falloff,
) -> usize {
    let with = marks_falling(tiles, geometry, consumed, falloff);
    with.saturating_sub(marks_falling(tiles, geometry, &[], falloff))
}

/// The `!` marks the start of the next turn takes off, once `consumed` is gone.
fn marks_falling(
    tiles: &[Tile], geometry: &crate::geometry::Geometry, consumed: &[usize], falloff: Falloff,
) -> usize {
    let falls = |l: &str| l == "!" || l.parse::<i64>().is_ok();
    let mut total = 0;
    let mut start = 0;
    for &rows in &geometry.rows_per_col {
        // Bottom first, as the tiles left above the word settle.
        let mut col: Vec<&str> = (start..start + rows)
            .filter(|i| !consumed.contains(i))
            .filter_map(|i| tiles.get(i).map(|t| t.letter.as_str()))
            .collect();
        start += rows;
        loop {
            let mut fell = false;
            for r in (0..falloff.rows.min(col.len())).rev() {
                if falls(col[r]) {
                    total += usize::from(col[r] == "!");
                    if falloff.to_wildcard {
                        col[r] = ".";
                    } else {
                        col.remove(r);
                    }
                    fell = true;
                }
            }
            if !fell {
                break;
            }
        }
        let multi = |l: &&&str| l.len() > 1 && l.bytes().all(|b| b == b'!');
        total += col.iter().take(falloff.rows).filter(multi).count();
    }
    total
}

pub fn hazard_fall(
    tiles: &[Tile], geometry: &crate::geometry::Geometry, consumed: &[usize], scorer: &Scorer,
) -> usize {
    // Positions of everything being removed, so each hazard is one pass over a small list.
    let removed: Vec<(usize, usize)> =
        consumed.iter().filter_map(|&i| geometry.position(i)).collect();
    let mut total = 0;
    for (i, tile) in tiles.iter().enumerate() {
        if !is_hazard(tile, scorer) || consumed.contains(&i) {
            continue;
        }
        let Some((col, row)) = geometry.position(i) else { continue };
        total += removed.iter().filter(|&&(c, r)| c == col && r < row).count();
    }
    total
}

/// Wildcards submitting `word` on the `consumed` tiles queues, given `per_run` from
/// [`Preferences::wildcards_per_run`].
///
/// The Bookkeepers bassanello (`items/doublesgear.lua:48-94`): *"if each tile is wooden also queues
/// an ornate wooden wildcard per set"*. Its modifier, `onSubmitWoodQueueWoodornateWildcardPerRun`,
/// runs `onSubmit`, so the word need not kill, and queues `runsCount * flagVal` of them
/// (`rpg/effects/modifiers/onSubmitWoodQueueWoodornateWildcardPerRun.lua:14-24`), with nothing that
/// can turn one away (`tileboard.queueSpecial`, `tileboard.lua:367-371`).
///
/// - **Each tile wooden** is `tiles.eachHasCat(wordTiles, 'wood')`, a material or border with the
///   `wood` category (`utils/tiles.lua:97-105,181`). No border has one, and the materials that do
///   are the `wood*` ones, a wildcard's `wood0` among them — exactly [`wood_only`].
/// - **Runs** are `words.countRuns(word)` (`utils/words.lua:194-206`), [`crate::gear::facts::count_runs`]:
///   one per set of repeated letters, so `BOOKKEEPER` is 3 and `AAA` is 1.
pub fn wildcards_queued(
    word: &str, tiles: &[Tile], consumed: &[usize], scorer: &Scorer, per_run: i64,
) -> i64 {
    if per_run <= 0 || !wood_only(tiles, consumed, scorer) {
        return 0;
    }
    per_run * crate::gear::facts::count_runs(&word.to_ascii_uppercase()) as i64
}

/// What a gem is worth to the ranking, spent or queued: **1, flat**. The user, 2026-11-07: *every
/// gem tile should be worth 1 flat*. Each gem was first priced at its tile's score — 3, but 4 for a
/// Y, whose cooper ligature adds a point (`rpg/effects/ligature/cooperY.lua`) — and that point
/// ranked `NOXA`, spending a Corundum N and the gold X, above `TOWKAY`, spending a Diamond Y alone.
pub const GEM_WORTH: f64 = 1.0;

/// The gear that queues a gem when a word is submitted — tiles [`hoarded`] would rather keep.
///
/// The user, 2026-11-07: *when we have an item that can queue a tile that we want to save, such as
/// the staff of the dragon that enqueues a corundum tile on fire word submission, rank that over the
/// infinitives gauntlet and scales of justice gain* — net of what the word spends, because a word
/// that uses up more of those tiles than it brings back is a loss. See [`Rank::gems_net`].
///
/// Three `onSubmit` modifiers, so the word need not kill, each queuing `flagVal` gems through
/// `tileboard.queueFlaggedTile`, which nothing turns away (`tileboard.lua:373-377`):
///
/// | flag                               | worn on                                   | when the word …                        | queues     |
/// |------------------------------------|-------------------------------------------|----------------------------------------|------------|
/// | `onSubmitFireDictWordQueueGemfire` | Corundum dragon, Staff of the dragon      | is in the `fire` dictionary            | Corundum   |
/// | `onSubmitIceDictWordQueueGemice`   | Chrysocolla walrus, Staff of the walrus   | is in the `ice` dictionary             | Chrysocolla|
/// | `onSubmit4MatchingQueueGemheal`    | Diamond unicorn, Staff of the unicorn     | has one letter at least four times     | Diamond    |
///
/// (`items/gemstaves.lua`, `rpg/effects/modifiers/<flag>.lua`.) The dictionaries are the game's own,
/// read at run time ([`crate::lexica`]), as `dictionaries.fire.find` reads them. "Four matching" is
/// `words.hash(word)`'s most common count over the word's characters (`utils/words.lua:162-174`).
///
/// **Not modelled:** `onCastFullColumnQueueGemheal` (`items/boardshapes.lua`), a Diamond for a spell
/// cast through the board's middle column. It hangs on which spell is cast and on the board's shape,
/// neither of which the ranking sees.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GemQueue {
    pub fire: i64,
    pub ice: i64,
    pub four_matching: i64,
    fire_words: std::sync::Arc<std::collections::HashSet<String>>,
    ice_words: std::sync::Arc<std::collections::HashSet<String>>,
}

impl GemQueue {
    /// Reads the flags from `rpg.player.gearFlags` and the word lists from `lexica`.
    pub fn from_save(cs: &crate::game::save::Table, lexica: &crate::lexica::Lexica) -> Self {
        let flag = |f: &str| {
            cs.path(&format!("rpg.player.gearFlags.{f}"))
                .and_then(|v| v.as_f64())
                .map(|v| v as i64)
                .unwrap_or(0)
        };
        let words = |key: &str, n: i64| match n > 0 {
            true => std::sync::Arc::new(lexica.words_for(key).cloned().unwrap_or_default()),
            false => Default::default(),
        };
        let (fire, ice) =
            (flag("onSubmitFireDictWordQueueGemfire"), flag("onSubmitIceDictWordQueueGemice"));
        GemQueue {
            fire,
            ice,
            four_matching: flag("onSubmit4MatchingQueueGemheal"),
            fire_words: words("fire", fire),
            ice_words: words("ice", ice),
        }
    }

    /// For tests: the given counts, with the word lists.
    pub fn with_words(fire: (i64, &[&str]), ice: (i64, &[&str]), four_matching: i64) -> Self {
        let set =
            |w: &[&str]| std::sync::Arc::new(w.iter().map(|s| s.to_ascii_uppercase()).collect());
        GemQueue {
            fire: fire.0,
            ice: ice.0,
            four_matching,
            fire_words: set(fire.1),
            ice_words: set(ice.1),
        }
    }

    /// How many gems submitting `word` queues.
    pub fn queued(&self, word: &str) -> i64 {
        if self.fire <= 0 && self.ice <= 0 && self.four_matching <= 0 {
            return 0;
        }
        let upper = word.to_ascii_uppercase();
        let mut n = 0;
        if self.fire > 0 && self.fire_words.contains(&upper) {
            n += self.fire;
        }
        if self.ice > 0 && self.ice_words.contains(&upper) {
            n += self.ice;
        }
        if self.four_matching > 0 {
            let mut counts = std::collections::HashMap::new();
            for c in upper.chars() {
                *counts.entry(c).or_insert(0) += 1;
            }
            if counts.values().any(|&k| k >= 4) {
                n += self.four_matching;
            }
        }
        n
    }
}

/// The kept tiles a word spends against the ones its gear queues, kind by kind: gold at its worth,
/// gems and wildcards one each.
///
/// The user, 2026-11-07: count a queued tile only net of what the word spends; rank gold first,
/// then the gems, and the wildcards below; every gem worth 1 flat. Each kind is netted against
/// its own:
///
/// ```text
///   gold_spent    = the worth of the gold spent           Rank::gold_spent (lower is better)
///   gems_net      = gems queued - gems spent              Rank::gems_net (signed)
///   wildcards_net = wildcards queued - wildcards spent    Rank::wildcards_net (signed)
/// ```
///
/// All three rank above the infinitives.
///
/// **The gem net is signed**, the user's correction the same day: on a board where `PUDDLY` spent
/// three gems for an infinitive and `UNCLAYED` one gem for nothing, `PUDDLY` ranked first, because
/// a loss was floored at zero and judged below the infinitives. *The net loss of gems is only one*.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TileLedger {
    /// The worth of the gold the word uses up.
    pub gold_spent: f64,
    /// The gems it uses up: a Corundum, a Chrysocolla, and a Diamond unless healing is wanted.
    pub gems_spent: f64,
    /// The plain wildcards it uses up.
    pub wildcards_spent: f64,
    /// The gems queued.
    pub gems: f64,
    /// The wildcards queued.
    pub wildcards: f64,
}

impl TileLedger {
    pub fn of(
        word: &str, tiles: &[Tile], consumed: &[usize], scorer: &Scorer, picking: &Context,
    ) -> Self {
        let prefs = picking.prefs;
        let heal_wanted = prefs.room.health > 0;
        let mut ledger = TileLedger {
            gems: GEM_WORTH * picking.gems.queued(word) as f64,
            wildcards: WILDCARD_WORTH
                * wildcards_queued(word, tiles, consumed, scorer, prefs.wildcards_per_run) as f64,
            ..TileLedger::default()
        };
        for t in consumed.iter().filter_map(|&i| tiles.get(i)) {
            match kept_kind(t, scorer, heal_wanted) {
                Kept::Gold(worth) => ledger.gold_spent += worth,
                Kept::Gem => ledger.gems_spent += GEM_WORTH,
                Kept::Wildcard => ledger.wildcards_spent += WILDCARD_WORTH,
                Kept::Not => {}
            }
        }
        ledger
    }

    pub fn gems_net(&self) -> f64 {
        self.gems - self.gems_spent
    }

    pub fn wildcards_net(&self) -> f64 {
        self.wildcards - self.wildcards_spent
    }
}

/// Does this word use nothing but wood?
///
/// Empty is not wood-only: a word that used no tiles could not have killed anything, and answering
/// `true` for it would make an impossible candidate outrank every real one.
pub fn wood_only(tiles: &[Tile], consumed: &[usize], scorer: &Scorer) -> bool {
    !consumed.is_empty()
        && consumed.iter().all(|&i| {
            tiles.get(i).and_then(|t| scorer.material_name(t)).is_some_and(|m| m.starts_with(WOOD))
        })
}

/// A wildcard: the letter is `.` until something is typed into it.
///
/// `letterMaterials['.'] = 'wood0'` and `score.wood0 = 0`
/// (`rpg/effects/material/default.lua:4,38`), so a wildcard is worth **nothing** on its own and
/// nothing in the scorer has any reason to keep one. What makes it precious is the other half: it
/// takes whatever letter we ask for, so it is the tile that turns a word we cannot spell into one we
/// can. That value does not appear in any score, which is why it has to be named here.
const WILDCARD: &str = ".";

/// What a wildcard counts for when deciding what a word spends. See [`hoarded`].
///
/// The dev's number, 2026-08-10, and the reasoning is in the size of it: **1, not 10.** A wildcard is
/// worth conserving, but nothing like as much as a gold tile — so this is set low enough that it can
/// never argue a gold tile into the fire, and high enough that between two words which spend the same
/// gold, the one that also burns a wildcard loses.
const WILDCARD_WORTH: f64 = 1.0;

/// The material a word should not be spending unless the kill needs it.
const GOLD: &str = "gold";

/// Is `material` gold: solid gold, or one of its serrated variants, `goldbleed` and `goldtoxin`?
/// Each says `material = 'gold'` among its categories and scores 10 like solid gold
/// (`rpg/effects/material/goldbleed.lua`, `goldtoxin.lua`), so each is held in reserve the same.
///
/// The user's board, 2026-11-07: `SEEDBED` spent a bleed gold S and a toxin gold D and ranked
/// first, as if it spent nothing, because only the material named `gold` was counted.
fn is_gold(material: &str) -> bool {
    let family = ["bleed", "toxin"].iter().find_map(|v| material.strip_suffix(v));
    family.unwrap_or(material) == GOLD
}

/// What this word spends from the board's stock of tiles worth keeping. Lower is better.
///
/// The dev's rule, 2026-08-09: **do not spend a solid gold tile until we cannot land a kill without
/// it.** Gold is what a board holds for the moment a big hit is needed, and spending a `Q` on a kill
/// a wood word would also have made is the waste this exists to stop.
///
/// ## Solid gold, and that means the material, not the border
///
/// [`Scorer::material_name`] resolves the two cases in one call — an explicit `bg` where gear has
/// upgraded the tile, and otherwise the material the letter implies, which makes **Z, X, J and Q**
/// gold by nature (`rpg/effects/material/default.lua:25-28`). The dev settled that both are hoarded:
/// the point of the rule is damage held in reserve, and a natural `Q` is the biggest reserve there
/// is. A *bordered* gold tile is a different thing — `rpg/effects/border/gold.lua` is an overlay that
/// can sit on any material — and is deliberately not counted.
///
/// ## Cost is the tile's own worth, which buys the post-MVP refinement for nothing
///
/// A gold tile costs what [`Scorer::tile_score`] says it is worth, so `Z = 10` and `Q = 40` are not
/// the same expenditure. That was raised as a later task — *minimise the value of the gold spent* —
/// and falls out of pricing preciousness at value rather than counting tiles. Nothing was added for
/// it; a flat cost would have been the same amount of code and less true.
///
/// A tile has one preciousness, so the reasons are `max`ed rather than summed: a wildcard that
/// gear has turned gold is priced as gold, which it is.
///
/// ## Attack gems, and the Diamond when no healing is wanted
///
/// The user, 2026-11-07: *implement the same for attack tiles like corundum, chrysocolla, and when
/// we don't need healing, diamond.* A **Corundum** (`gemfire`) or **Chrysocolla** (`gemice`) is the
/// board's arcane spell in reserve — at the front of a word it adds its status and, with Adept, half
/// as much damage again (see [`crate::spells::tile_school`]) — so a kill that a plainer word would
/// also have made should not spend one. A **Diamond** (`gemheal`) is a heal in reserve, and is
/// free to spend only while `heal_wanted`: health is missing ([`Room::health`]).
///
/// Each gem counts [`GEM_WORTH`], 1 flat — the user, 2026-11-07, after a Y's cooper point made
/// one gem cost more than another. The ranking weighs gold, gems and wildcards as separate terms
/// ([`TileLedger`]); this sum, gold at its worth and the rest one each, is the single figure the
/// search uses to break ties between placements of a word.
pub fn hoarded(tiles: &[Tile], consumed: &[usize], scorer: &Scorer, heal_wanted: bool) -> f64 {
    consumed
        .iter()
        .filter_map(|&i| tiles.get(i))
        .map(|t| match kept_kind(t, scorer, heal_wanted) {
            Kept::Gold(worth) => worth,
            Kept::Gem => GEM_WORTH,
            Kept::Wildcard => WILDCARD_WORTH,
            Kept::Not => 0.0,
        })
        .sum()
}

/// The gems among `consumed`, one each: a Corundum, a Chrysocolla, and a Diamond unless healing is
/// wanted.
pub fn gems_spent(tiles: &[Tile], consumed: &[usize], scorer: &Scorer, heal_wanted: bool) -> f64 {
    consumed
        .iter()
        .filter_map(|&i| tiles.get(i))
        .filter(|t| kept_kind(t, scorer, heal_wanted) == Kept::Gem)
        .count() as f64
        * GEM_WORTH
}

/// Why a tile is worth keeping, if it is. A tile has one reason: a wildcard that gear has made
/// gold is gold, and a gem wildcard a gem.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Kept {
    /// Solid gold, at the tile's own worth.
    Gold(f64),
    Gem,
    Wildcard,
    Not,
}

fn kept_kind(t: &Tile, scorer: &Scorer, heal_wanted: bool) -> Kept {
    match scorer.material_name(t) {
        Some(m) if is_gold(m) => Kept::Gold(scorer.tile_score(t).max(WILDCARD_WORTH)),
        Some("gemfire" | "gemice") => Kept::Gem,
        Some("gemheal") if !heal_wanted => Kept::Gem,
        _ if t.letter == WILDCARD => Kept::Wildcard,
        _ => Kept::Not,
    }
}

/// Ligatures the board is better off without, by **the game's own name** for them.
///
/// The dev, 2026-08-17: *we need a way to try to get rid of the ash (æ ligature) because there are
/// so few words that can use it.*
///
/// Named rather than matched on the letters, because the name is what the game writes
/// (`ligature = 'ash'`, `rpg/enemies/slimes.lua:27`) and the letters are not a reliable handle — a
/// two-character tile is as likely to be a `QU`, which is the opposite of a problem.
///
/// **Only the ones that are hard to spend belong here.** `ui/objects/tile.lua:40-51` lists the full
/// set — `QU`, `thorn`, `eth`, `TH`, `ash`, `ethel`, `ED`, `ES`, `LY`, `ING` — and most of them are
/// *good*: `ING` and `ED` are the commonest suffixes in English and `QU` carries the `U` that makes
/// a `Q` playable at all. Adding one here says "spend this when convenient", which is a claim about
/// how few words can use it and wants the same measurement the shrine dictionaries got, not a guess.
const SHED: &[&str] = &["ash"];

/// Is this a tile we would rather the board stopped holding?
fn sheddable(t: &Tile) -> bool {
    // A **wildcard's** `ligature` is a letter-class pattern like `[AEIOU]` rather than a name
    // (`crate::observe::board::Quality::ligature`), and the two uses are told apart by the tile's
    // letter. Excluding wildcards outright is the cheap half of that test, and a wildcard is the
    // last tile we would want to shed in any case.
    !t.is_wildcard() && t.quality.ligature.as_deref().is_some_and(|l| SHED.contains(&l))
}

/// How many tiles worth being rid of this word takes off the board. Higher is better.
///
/// The inverse of [`hoarded`], deliberately built the same way — same walk over `consumed`, same
/// shape — so the two read as the pair they are.
///
/// **A count, not a value.** `hoarded` prices gold at [`Scorer::tile_score`] because a `Q` really is
/// a bigger reserve than a `Z`. There is no matching scale here: an ash is not *worth* anything, it
/// is in the way, and one of them is one of them. If a second entry ever joins [`SHED`] with a
/// genuinely different nuisance value, this is where that would be priced.
///
/// ## What this does and does not reach
///
/// [`Rank`] only ever compares words that **already kill** — see this module's header. So the ash
/// leaves the board opportunistically, on turns where more than one word is lethal and the choice
/// is free. On a turn with a single killing word, or none, nothing here applies. That matches the
/// dev's framing — *only skip the ash if we can't achieve what we otherwise want for this turn* —
/// and it is worth being clear that the turns where a clogged board hurts most are the ones this
/// cannot help with. Reaching those means ranking non-lethal candidates, which is a separate job.
pub fn shed(tiles: &[Tile], consumed: &[usize]) -> usize {
    consumed.iter().filter_map(|&i| tiles.get(i)).filter(|t| sheddable(t)).count()
}

/// Letter counts of what would be left standing.
pub fn remaining_counts(tiles: &[Tile], consumed: &[usize]) -> [usize; ALPHABET] {
    letters::counts_of(
        tiles
            .iter()
            .enumerate()
            .filter(|(i, _)| !consumed.contains(i))
            .map(|(_, t)| t.letter.as_str()),
    )
}

/// How many burning tiles the word takes off the board. See [`Rank::fire_eaten`].
pub fn fire_eaten(tiles: &[Tile], consumed: &[usize]) -> usize {
    consumed.iter().filter(|&&i| tiles.get(i).and_then(|t| t.burn()).is_some_and(|b| b > 0)).count()
}

/// The full ranking for one candidate.
#[allow(clippy::too_many_arguments)]
pub fn rank(
    word: &str, tiles: &[Tile], geometry: &crate::geometry::Geometry, consumed: &[usize],
    scorer: &Scorer, picking: &Context, damage: i64,
) -> Rank {
    let (target, prefs) = (&picking.target, picking.prefs);
    let ledger = TileLedger::of(word, tiles, consumed, scorer, picking);
    Rank {
        fire_eaten: fire_eaten(tiles, consumed),
        // Set by the caller, which knows whether the enemy will hit back: see the field.
        halves_attack: false,
        wood_only: prefs.wood_only_pays
            && prefs.kill_at.is_none_or(|at| damage >= at)
            && wood_only(tiles, consumed, scorer),
        heals_fully: prefs.heal_at.is_some_and(|at| damage >= at),
        hazards_cleared: hazards_cleared(tiles, geometry, consumed, scorer),
        gain: gain(tiles, consumed, scorer, prefs.room),
        gold_spent: ledger.gold_spent,
        gems_net: ledger.gems_net(),
        wildcards_net: ledger.wildcards_net(),
        upgrades: prefs.infinitives.upgraded(word),
        infinitives: prefs.infinitives.created(word),
        shed: shed(tiles, consumed),
        hazard_fall: hazard_fall(tiles, geometry, consumed, scorer),
        deviation: target.deviation(&remaining_counts(tiles, consumed)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Geometry;
    use std::path::PathBuf;

    fn scorer() -> Option<Scorer> {
        let dir = PathBuf::from("../sternly-worded-adventures");
        if !dir.join("rpg/effects/material/default.lua").is_file() {
            eprintln!("SKIP: game source not present");
            return None;
        }
        Some(Scorer::new(&dir).expect("scoring tables load"))
    }

    /// What a word spends, read off the game's own tables rather than asserted from memory.
    ///
    /// The dev's rule is about *solid gold*, so the fixture has to prove the scorer agrees which
    /// letters those are before the costs mean anything.
    #[test]
    fn serrated_gold_is_gold() {
        let Some(sc) = scorer() else { return };
        let mut t = board(&["D", "S", "E"]);
        t[0].quality.material = Some("goldtoxin".into());
        t[1].quality.material = Some("goldbleed".into());
        t[2].quality.material = Some("bronzetoxin".into());
        let ctx = Context::default();
        let spent = |i: &[usize]| TileLedger::of("DSE", &t, i, &sc, &ctx).gold_spent;
        assert_eq!(spent(&[0]), sc.tile_score(&t[0]), "toxin gold at its worth");
        assert_eq!(spent(&[1]), sc.tile_score(&t[1]), "bleed gold at its worth");
        assert!(spent(&[0]) >= 10.0, "worth what solid gold is");
        assert_eq!(spent(&[2]), 0.0, "serrated bronze is not gold");
        assert!(!is_gold("golden") && !is_gold("bleed") && is_gold("gold"));
    }

    #[test]
    fn gold_letters_cost_what_they_are_worth_and_plain_ones_cost_nothing() {
        let Some(sc) = scorer() else { return };
        let tiles = board(&["Q", "Z", "E", "A"]);
        // If this is wrong the fixture is wrong, not the code, and every assertion below would be
        // passing for the wrong reason.
        for (i, letter) in ["Q", "Z"].iter().enumerate() {
            assert_eq!(
                sc.material_name(&tiles[i]).as_deref(),
                Some(GOLD),
                "{letter} is solid gold"
            );
        }
        assert_eq!(sc.material_name(&tiles[2]).as_deref(), Some("wood"), "E is not");

        assert_eq!(
            hoarded(&tiles, &[2, 3], &sc, false),
            0.0,
            "a word of plain letters spends nothing"
        );
        // `score.Q = 40`, `score.Z` falls through to `score.gold = 10`
        // (`rpg/effects/material/default.lua:33-41`). So the two are not interchangeable, which is
        // the whole of the deferred "spend the cheapest gold" refinement, arriving for free.
        let q = hoarded(&tiles, &[0], &sc, false);
        let z = hoarded(&tiles, &[1], &sc, false);
        assert!(z > 0.0 && q > z, "Q ({q}) should cost more to spend than Z ({z})");
        assert_eq!(hoarded(&tiles, &[0, 1], &sc, false), q + z, "and spending both costs both");
    }

    /// A wildcard is worth nothing to the scorer and something to us.
    ///
    /// `letterMaterials['.'] = 'wood0'` with `score.wood0 = 0`, so nothing in the scoring model has
    /// any reason to keep one — its value is that it takes whatever letter we ask for. The dev's
    /// number is **1**: enough to break a tie, never enough to argue a gold tile into the fire.
    #[test]
    fn a_wildcard_is_worth_keeping_but_never_more_than_gold() {
        let Some(sc) = scorer() else { return };
        let tiles = board(&[".", "Z", "E"]);
        assert_eq!(sc.tile_score(&tiles[0]), 0.0, "the game scores a wildcard at nothing");
        assert_eq!(
            hoarded(&tiles, &[0], &sc, false),
            WILDCARD_WORTH,
            "and we still would rather keep it"
        );

        // The point of the number being small: burning a wildcard must never look worse than
        // burning gold, or the rule would spend the reserve to protect the convenience.
        assert!(
            hoarded(&tiles, &[0], &sc, false) < hoarded(&tiles, &[1], &sc, false),
            "a wildcard has to cost less than the cheapest gold tile"
        );
        assert_eq!(hoarded(&tiles, &[2], &sc, false), 0.0, "and a plain letter is free to spend");
    }

    /// The user's rule, 2026-11-07: Corundum and Chrysocolla are kept like gold, a Diamond only while
    /// nothing needs healing — and every gem counts 1, flat, below the cheapest gold.
    #[test]
    fn attack_gems_are_kept_and_a_diamond_until_healing_is_wanted() {
        let Some(sc) = scorer() else { return };
        let tiles = vec![
            of("F", "gemice"),
            of("T", "gemfire"),
            of("E", "gemheal"),
            of("A", "wood"),
            Tile::plain("Z"),
            Tile::plain("."),
        ];
        for i in [0, 1, 2] {
            let gem = hoarded(&tiles, &[i], &sc, false);
            assert_eq!(gem, GEM_WORTH, "tile {i} counts one, whatever its score");
            assert!(gem < hoarded(&tiles, &[4], &sc, false), "tile {i}");
        }
        assert_eq!(hoarded(&tiles, &[2], &sc, true), 0.0, "a Diamond is free when injured");
        assert_eq!(hoarded(&tiles, &[0, 1], &sc, true), 2.0, "the attack gems never are");
        assert_eq!(hoarded(&tiles, &[3], &sc, false), 0.0, "plain wood stays free");
        // A gem wildcard is a gem: one reason to keep a tile, not two.
        let mut wild_gem = Tile::plain(".");
        wild_gem.quality.material = Some("gemice".into());
        assert_eq!(gems_spent(&[wild_gem], &[0], &sc, false), GEM_WORTH);
    }

    /// Fire is cleared by the count, and a partial clear is real progress.
    ///
    /// **This test exists because I got the rule wrong and argued for the wrong one.** The
    /// single-turn arithmetic says a partial clear buys nothing — damage is flat, so four of five
    /// costs exactly what five of five costs — and I proposed ranking on "did we clear the last
    /// one". The dev's correction is the one that holds over a fight: a word is a handful of tiles
    /// and a board is sixteen, so on a heavily burning board the threshold cannot be reached this
    /// turn, and a rule that only values reaching it declines every partial clear and never arrives
    /// at the turn where zero is possible.
    ///
    /// So the assertions are deliberately about the *middle* of the range, where a threshold rule
    /// and a counting rule disagree. A test that only checked "all five beats four" would pass under
    /// both and prove nothing.
    #[test]
    fn a_partial_fire_clear_beats_a_smaller_one_and_outranks_the_wood_payout() {
        let r = |fire: usize, wood: bool| Rank {
            fire_eaten: fire,
            halves_attack: false,
            wood_only: wood,
            heals_fully: false,
            gain: 0,
            gems_net: 0.0,
            gold_spent: 0.0,
            wildcards_net: 0.0,
            upgrades: 0,
            shed: 0,
            hazard_fall: 0,
            hazards_cleared: 0,
            infinitives: 0,
            deviation: 0.0,
        };
        // Neither of these clears the board, so a threshold rule calls them equal. They are not.
        assert!(r(4, false).better_than(&r(2, false)), "four off the board beats two");
        assert!(r(1, false).better_than(&r(0, false)), "and one beats none");
        // Above the wood payout, which is the ordering the dev's example turns on: four armour once
        // against `ceil(maxHealth/16)` every turn until the board stops burning.
        assert!(r(3, false).better_than(&r(0, true)), "fire outranks the wood bonus");
        // And finishing the job still wins, because the count contains the threshold.
        assert!(r(5, false).better_than(&r(4, true)));
        // Equal fire falls through to the payouts, unchanged.
        assert!(r(2, true).better_than(&r(2, false)));
    }

    fn of(letter: &str, material: &str) -> Tile {
        Tile {
            letter: letter.into(),
            quality: crate::observe::board::Quality {
                material: Some(material.into()),
                ..Default::default()
            },
        }
    }

    fn room(armour: i64, health: i64, bleed: i64) -> Room {
        Room { armour, health, bleed, silver_armour: 0 }
    }

    /// The Silver chain coif: `flagVal` armour per silver tile, by material or by border, in any
    /// word — last or not, kill or not — and capped with everything else at the armour cap.
    #[test]
    fn the_silver_coif_gives_armour_per_silver_tile() {
        let Some(sc) = scorer() else { return };
        let mut bordered = of("A", "wood");
        bordered.quality.border = Some("silver".into());
        let t = vec![of("A", "silver2"), bordered, of("T", "wood"), of("E", "electrum")];
        let coif = |armour| Room { silver_armour: 1, ..room(armour, 0, 0) };
        assert_eq!(
            gains(&t, &[0, 1, 2, 3], &sc, coif(100)).armour,
            3,
            "material, border, electrum"
        );
        assert_eq!(gains(&t, &[2], &sc, coif(100)).armour, 0, "plain wood is not silver");
        assert_eq!(gains(&t, &[0, 1, 2, 3], &sc, coif(2)).armour, 2, "only up to the cap");
        assert_eq!(gains(&t, &[0, 1, 2, 3], &sc, room(100, 0, 0)).armour, 0, "not worn");
        let doubled = Room { silver_armour: 2, ..room(100, 0, 0) };
        assert_eq!(gains(&t, &[0, 3], &sc, doubled).armour, 4, "the flag's value is per tile");
    }

    /// The Bookkeepers bassanello: `runs * flagVal` wildcards for a word whose every tile is wood,
    /// kill or not (`rpg/effects/modifiers/onSubmitWoodQueueWoodornateWildcardPerRun.lua`).
    #[test]
    fn the_bassanello_queues_a_wildcard_per_run_of_an_all_wood_word() {
        let Some(sc) = scorer() else { return };
        let wood =
            |w: &str| -> Vec<Tile> { w.chars().map(|c| of(&c.to_string(), "wood")).collect() };
        let t = wood("BOOKKEEPER");
        let all: Vec<usize> = (0..t.len()).collect();
        assert_eq!(wildcards_queued("BOOKKEEPER", &t, &all, &sc, 1), 3, "the item's own example");
        assert_eq!(wildcards_queued("bookkeeper", &t, &all, &sc, 2), 6, "per flag, any case");
        assert_eq!(wildcards_queued("BOOKKEEPER", &t, &all, &sc, 0), 0, "not worn");
        // As the board deals them, B and K are silver, so the same word pays nothing.
        let dealt = board(&["B", "O", "O", "K", "K", "E", "E", "P", "E", "R"]);
        assert_eq!(wildcards_queued("BOOKKEEPER", &dealt, &all, &sc, 1), 0, "not all wood");
        let t =
            vec![of("T", "wood"), of(".", "wood0"), of("S", "woodbraced"), of("S", "woodornate")];
        assert_eq!(wildcards_queued("TOSS", &t, &[0, 1, 2, 3], &sc, 1), 1, "every wood counts");
        assert_eq!(
            wildcards_queued("TOAST", &wood("TOAST"), &[0, 1, 2, 3, 4], &sc, 1),
            0,
            "no run"
        );
        assert_eq!(wildcards_queued("AAA", &wood("AAA"), &[0, 1, 2], &sc, 1), 1, "a run is a set");
    }

    /// Below health and armour: gold, then gems, then wildcards, then the infinitives.
    #[test]
    fn tile_terms_rank_gold_then_gems_then_wildcards_then_infinitives() {
        let r = |gain: i64, gold: f64, gems: f64, wildcards: f64, infinitives: i64| Rank {
            fire_eaten: 0,
            halves_attack: false,
            wood_only: false,
            heals_fully: false,
            hazards_cleared: 0,
            gain,
            gold_spent: gold,
            gems_net: gems,
            wildcards_net: wildcards,
            upgrades: 0,
            infinitives,
            shed: 0,
            hazard_fall: 0,
            deviation: 0.0,
        };
        assert!(r(4, 9.0, 0.0, 0.0, 0).better_than(&r(0, 0.0, 0.0, 0.0, 0)), "armour first");
        assert!(r(0, 0.0, -1.0, 0.0, 0).better_than(&r(0, 11.0, 0.0, 0.0, 0)), "then gold");
        assert!(r(0, 0.0, 0.0, -2.0, 0).better_than(&r(0, 0.0, -1.0, 0.0, 1)), "then gems");
        assert!(r(0, 0.0, 0.0, 0.0, 0).better_than(&r(0, 0.0, 0.0, -1.0, 1)), "then wildcards");
        assert!(r(0, 0.0, 0.0, 1.0, 0).better_than(&r(0, 0.0, 0.0, 0.0, 1)), "a wildcard gained");
        assert!(r(0, 0.0, 0.0, 0.0, 1).better_than(&r(0, 0.0, 0.0, 0.0, 0)), "then infinitives");
    }

    /// Each kind nets against its own: gold at its worth, gems and wildcards one each.
    #[test]
    fn the_ledger_nets_each_kind_against_its_own() {
        let l = |gold_spent, gems_spent, wildcards_spent, gems, wildcards| {
            let l = TileLedger { gold_spent, gems_spent, wildcards_spent, gems, wildcards };
            (l.gold_spent, l.gems_net(), l.wildcards_net())
        };
        assert_eq!(l(0.0, 0.0, 0.0, 0.0, 0.0), (0.0, 0.0, 0.0), "nothing either way");
        assert_eq!(l(11.0, 0.0, 0.0, 0.0, 0.0), (11.0, 0.0, 0.0), "the gold X");
        assert_eq!(l(0.0, 1.0, 0.0, 0.0, 0.0), (0.0, -1.0, 0.0), "a gem spent is a gem lost");
        assert_eq!(l(0.0, 1.0, 0.0, 1.0, 0.0), (0.0, 0.0, 0.0), "one Corundum spent to queue one");
        assert_eq!(l(0.0, 2.0, 0.0, 1.0, 0.0), (0.0, -1.0, 0.0), "two spent to queue one");
        assert_eq!(
            l(0.0, 0.0, 2.0, 0.0, 1.0),
            (0.0, 0.0, -1.0),
            "two wildcards spent to queue one"
        );
        assert_eq!(l(0.0, 0.0, 1.0, 0.0, 2.0), (0.0, 0.0, 1.0), "one wildcard spent to queue two");
        assert_eq!(
            l(11.0, 0.0, 0.0, 1.0, 0.0),
            (11.0, 1.0, 0.0),
            "a gem queued does not pay for gold"
        );
    }

    /// The user's board, 2026-11-07: `NOXA` spends a Corundum N and the gold X, `TOWKAY` a Diamond
    /// Y, whose cooper ligature adds a point. One gem each, so the gold decides.
    #[test]
    fn every_gem_counts_one_and_gold_decides() {
        let Some(sc) = scorer() else { return };
        let mut t = board(&["N", "O", "X", "A", "Y"]);
        t[0].quality.material = Some("gemfire".into());
        t[2].quality.material = Some("gold".into());
        t[4].quality.material = Some("gemheal".into());
        t[4].quality.ligature = Some("cooperY".into());
        assert!(sc.tile_score(&t[4]) > sc.tile_score(&t[0]), "the Y scores a point more");
        let ctx = Context::default();
        let noxa = TileLedger::of("NOXA", &t, &[0, 1, 2, 3], &sc, &ctx);
        let oya = TileLedger::of("OYA", &t, &[1, 4, 3], &sc, &ctx);
        assert_eq!((noxa.gems_net(), oya.gems_net()), (-1.0, -1.0), "one gem each");
        assert!(noxa.gold_spent > 0.0 && oya.gold_spent == 0.0);
    }

    /// The user's board, 2026-11-07: `PUDDLY` spends three gems and queues an infinitive,
    /// `UNCLAYED` spends one and queues nothing. One gem lost beats three, infinitive or not.
    #[test]
    fn a_smaller_gem_loss_outranks_an_infinitive() {
        let r = |gems_net: f64, infinitives: i64| Rank {
            fire_eaten: 0,
            halves_attack: false,
            wood_only: false,
            heals_fully: false,
            hazards_cleared: 0,
            gain: 0,
            gems_net,
            infinitives,
            gold_spent: 0.0,
            wildcards_net: 0.0,
            upgrades: 0,
            shed: 0,
            hazard_fall: 0,
            deviation: 0.0,
        };
        assert!(r(-3.0, 0).better_than(&r(-9.0, 1)), "UNCLAYED over PUDDLY");
        assert!(r(-3.0, 1).better_than(&r(-3.0, 0)), "equal gems: the infinitive decides");
    }

    /// The gem staves queue on the game's dictionaries and on four of a letter.
    #[test]
    fn gem_gear_queues_on_its_words() {
        let q = GemQueue::with_words((1, &["blaze"]), (2, &["frost"]), 1);
        assert_eq!(q.queued("BLAZE"), 1, "a fire word, any case");
        assert_eq!(q.queued("frost"), 2, "per flag");
        assert_eq!(q.queued("CAT"), 0);
        assert_eq!(q.queued("BEEKEEPER"), 1, "five Es");
        assert_eq!(q.queued("BOOKKEEPER"), 0, "three is not four");
        assert_eq!(GemQueue::default().queued("BLAZE"), 0, "not worn");
    }

    /// The real save flag and dictionary: the Staff of the dragon's fire words.
    #[test]
    fn the_staff_of_the_dragon_reads_the_fire_dictionary() {
        let game = PathBuf::from("../sternly-worded-adventures");
        let Ok(lex) = crate::lexica::Lexica::load(&game) else { return };
        let fire = lex.words_for("fire").expect("fire dictionary");
        let word = fire.iter().min().unwrap().clone();
        let cs = crate::game::save::parse(
            "return { rpg = { player = { gearFlags = { onSubmitFireDictWordQueueGemfire = 1 } } } }",
        )
        .unwrap();
        let q = GemQueue::from_save(&cs, &lex);
        assert_eq!(q.queued(&word), 1, "{word}");
        assert_eq!(q.queued("ZZZZQ"), 0);
        let bare = crate::game::save::parse("return {}").unwrap();
        assert_eq!(GemQueue::from_save(&bare, &lex), GemQueue::default());
    }

    /// The letters whose default material is silver count without any upgrade
    /// (`rpg/effects/material/default.lua:18-25`).
    #[test]
    fn silver_letters_count_by_their_default_material() {
        let Some(sc) = scorer() else { return };
        let t = board(&["G", "H", "Y", "B", "F", "K", "W", "V", "A"]);
        let silver: Vec<bool> = t.iter().map(|x| is_silver(x, &sc)).collect();
        assert_eq!(silver, [true, true, true, true, true, true, true, true, false]);
    }

    /// Braced wood as the last tile: 4 armour per braced tile in the word
    /// (`rpg/effects/material/woodbraced.lua`), only as much as fits under the armour cap.
    #[test]
    fn braced_wood_last_gives_four_armour_per_braced_tile() {
        let Some(sc) = scorer() else { return };
        let t = vec![of("F", "wood"), of("U", "woodbraced"), of("D", "woodbraced")];
        assert_eq!(gain(&t, &[0, 1, 2], &sc, room(100, 0, 0)), 8, "two braced, the last of them");
        assert_eq!(gain(&t, &[2, 1, 0], &sc, room(100, 0, 0)), 0, "braced, but not last");
        assert_eq!(gain(&t, &[0, 1, 2], &sc, room(3, 0, 0)), 3, "only up to the cap");
        assert_eq!(gain(&t, &[0, 1, 2], &sc, room(0, 0, 0)), 0, "at the cap it is worth nothing");
    }

    /// A Diamond last: per gem in the word, 3 healing less bleed, bleed down by 3, and 3 regen
    /// (`rpg/effects/material/gemheal.lua`). Regen heals 1 a turn and not while bleeding
    /// (`rpgview.lua:1000-1002`), so it counts only once the bleed is gone, and only up to what is
    /// still missing after the heal.
    #[test]
    fn a_diamond_last_gives_its_heal_and_regen_up_to_what_is_missing() {
        let Some(sc) = scorer() else { return };
        let t = vec![of("C", "gemice"), of("A", "wood"), of("T", "gemheal")];
        let word = [0, 1, 2]; // two gems: 6 healing, 6 regen
        assert_eq!(gain(&t, &word, &sc, room(0, 10, 0)), 6 + 4, "regen fills the rest");
        assert_eq!(gain(&t, &word, &sc, room(0, 20, 0)), 6 + 6);
        assert_eq!(gain(&t, &word, &sc, room(0, 20, 2)), 4 + 6, "bleed eats the heal, then clears");
        assert_eq!(gain(&t, &word, &sc, room(0, 20, 9)), 0, "still bleeding: no heal, no regen");
        assert_eq!(gain(&t, &word, &sc, room(0, 0, 0)), 0, "at full health it is worth nothing");
        assert_eq!(gain(&t, &[2, 1, 0], &sc, room(0, 20, 0)), 0, "the Diamond must be last");
    }

    /// The user's rule, 2026-11-03: what a word gives the player outranks board hygiene, but not
    /// the fire count or the two payouts ranked above it.
    #[test]
    fn gain_outranks_hygiene_but_not_fire_or_the_payouts() {
        let r = |fire: usize, wood: bool, gain: i64, hoarded: f64, deviation: f64| Rank {
            fire_eaten: fire,
            halves_attack: false,
            wood_only: wood,
            heals_fully: false,
            gain,
            gems_net: 0.0,
            gold_spent: hoarded,
            wildcards_net: 0.0,
            upgrades: 0,
            shed: 0,
            hazard_fall: 0,
            hazards_cleared: 0,
            infinitives: 0,
            deviation,
        };
        assert!(r(0, true, 4, 0.0, 11.6).better_than(&r(0, true, 0, 0.0, 11.0)), "FUD over DAU");
        assert!(r(0, false, 4, 5.0, 0.0).better_than(&r(0, false, 0, 0.0, 0.0)), "over hoarding");
        assert!(r(0, false, 8, 0.0, 0.0).better_than(&r(0, false, 4, 0.0, 0.0)), "more is better");
        assert!(
            r(0, true, 0, 0.0, 0.0).better_than(&r(0, false, 8, 0.0, 0.0)),
            "wood payout first"
        );
        assert!(r(1, false, 0, 0.0, 0.0).better_than(&r(0, false, 8, 0.0, 0.0)), "fire first");
    }

    /// The dev's four tiers, 2026-08-15, in the order they stated them.
    ///
    /// > if a Well-Rested heal only heals for 3 while a wood-only kill provides 4 armour, we should
    /// > first check if there's a wood-only kill that triggers both the Well-Rested heal and armour
    /// > bonus; otherwise, go for the best wood-only kill, and only if that's not possible, go for
    /// > the Well-Rested heal.
    ///
    /// Deliberately asserted with the *other* fields set against each tier — the heal-only word is
    /// given a spotless board and the wood word a wasteful one — because that is what proves the
    /// order is the priority and not an accident of the hygiene terms.
    #[test]
    fn a_wood_kill_outranks_a_heal_it_cannot_also_collect() {
        let tier = |wood: bool, heal: bool, hoarded: f64| Rank {
            fire_eaten: 0,
            halves_attack: false,
            wood_only: wood,
            heals_fully: heal,
            gain: 0,
            gems_net: 0.0,
            gold_spent: hoarded,
            wildcards_net: 0.0,
            upgrades: 0,
            shed: 0,
            hazard_fall: 0,
            hazards_cleared: 0,
            infinitives: 0,
            deviation: 0.0,
        };
        let both = tier(true, true, 99.0);
        let wood_only_kill = tier(true, false, 99.0);
        let heal_only = tier(false, true, 0.0);
        let ordinary = tier(false, false, 0.0);

        assert!(both.better_than(&wood_only_kill), "both payouts beat one, spendthrift or not");
        assert!(wood_only_kill.better_than(&heal_only), "the gear's payout outranks the heal");
        assert!(heal_only.better_than(&ordinary), "and a heal still beats no payout at all");
        // Transitive, so the whole order holds rather than just its neighbours.
        assert!(both.better_than(&ordinary));
        assert!(wood_only_kill.better_than(&ordinary));
        assert!(!heal_only.better_than(&wood_only_kill), "strictly, in one direction only");
    }

    /// With no heal to collect, the field must not sway anything.
    #[test]
    fn the_heal_rank_is_inert_when_there_is_no_heal() {
        let prefs = Preferences { wood_only_pays: true, ..Preferences::default() };
        // `rank` is what sets the field, and `heal_at: None` is how the caller says the heal is
        // unavailable -- no charge, cancelled by gear, or already at full health.
        assert!(!prefs.heal_at.is_some_and(|at| 9999 >= at), "no threshold, so nothing clears it");
        let with = Preferences::default().healing_at(Some(20));
        assert!(with.heal_at.is_some_and(|at| 20 >= at), "exactly the threshold heals fully");
        assert!(!with.heal_at.is_some_and(|at| 19 >= at), "a point short does not");
    }

    /// Two words that both kill: the one that keeps the good tiles wins.
    ///
    /// The rule in the shape it is actually used — a comparison between candidates, not a score.
    #[test]
    fn between_two_kills_the_one_that_spends_less_wins() {
        let plain = Rank {
            fire_eaten: 0,
            halves_attack: false,
            wood_only: false,
            heals_fully: false,
            gain: 0,
            gems_net: 0.0,
            gold_spent: 0.0,
            wildcards_net: 0.0,
            upgrades: 0,
            shed: 0,
            hazard_fall: 0,
            hazards_cleared: 0,
            infinitives: 0,
            deviation: 9.0,
        };
        let golden = Rank {
            fire_eaten: 0,
            halves_attack: false,
            wood_only: false,
            heals_fully: false,
            gain: 0,
            gems_net: 0.0,
            gold_spent: 40.0,
            wildcards_net: 0.0,
            upgrades: 0,
            shed: 0,
            hazard_fall: 0,
            hazards_cleared: 0,
            infinitives: 0,
            deviation: 0.0,
        };
        assert!(plain.better_than(&golden), "a tidier board is not worth a Q");
        assert!(!golden.better_than(&plain));

        // Below the payout, though. `wood_only` is gear paying out for real, and this is a saving.
        let paid = Rank {
            fire_eaten: 0,
            halves_attack: false,
            wood_only: true,
            heals_fully: false,
            gain: 0,
            gems_net: 0.0,
            gold_spent: 40.0,
            wildcards_net: 0.0,
            upgrades: 0,
            shed: 0,
            hazard_fall: 0,
            hazards_cleared: 0,
            infinitives: 0,
            deviation: 9.0,
        };
        assert!(paid.better_than(&plain), "a payout in hand outranks a tile kept back");

        // And the wildcard case the dev asked for: same gold either way, so do not also burn one.
        let gold_only = Rank {
            fire_eaten: 0,
            halves_attack: false,
            wood_only: false,
            heals_fully: false,
            gain: 0,
            gems_net: 0.0,
            gold_spent: 10.0,
            wildcards_net: 0.0,
            upgrades: 0,
            shed: 0,
            hazard_fall: 0,
            hazards_cleared: 0,
            infinitives: 0,
            deviation: 0.0,
        };
        let gold_and_wild = Rank {
            fire_eaten: 0,
            halves_attack: false,
            wood_only: false,
            heals_fully: false,
            gain: 0,
            gems_net: 0.0,
            gold_spent: 11.0,
            wildcards_net: 0.0,
            upgrades: 0,
            shed: 0,
            hazard_fall: 0,
            hazards_cleared: 0,
            infinitives: 0,
            deviation: 0.0,
        };
        assert!(gold_only.better_than(&gold_and_wild), "no reason to spend a wildcard as well");
    }

    /// An ash is spent when spending it is free, and never at the cost of a gold tile.
    ///
    /// Both halves of the dev's rule, 2026-08-17: *only skip the ash if we can't achieve what we
    /// otherwise want for this turn*, and *ranking the ash usage underneath hoarding works.*
    ///
    /// The second assertion is the one with teeth. Placed above `hoarded` this rule would trade a
    /// `Q` — the board's biggest reserve — for the sake of tidying, which is precisely the waste
    /// `hoarded` exists to prevent, and the failure would only ever show up as a fight lost several
    /// turns later.
    #[test]
    fn an_ash_is_shed_when_it_is_free_and_never_at_the_price_of_gold() {
        let r = |hoarded: f64, shed: usize, deviation: f64| Rank {
            fire_eaten: 0,
            halves_attack: false,
            wood_only: false,
            heals_fully: false,
            gain: 0,
            gems_net: 0.0,
            gold_spent: hoarded,
            wildcards_net: 0.0,
            upgrades: 0,
            shed,
            hazard_fall: 0,
            hazards_cleared: 0,
            infinitives: 0,
            deviation,
        };
        // Nothing else to separate them, so getting rid of the ash decides it.
        assert!(r(0.0, 1, 0.0).better_than(&r(0.0, 0, 0.0)), "with all else equal, dump the ash");
        assert!(!r(0.0, 0, 0.0).better_than(&r(0.0, 1, 0.0)), "strictly, in one direction");

        // **Underneath hoarding.** A Q is worth 40; an ash is worth being rid of. Not that much.
        assert!(
            r(0.0, 0, 0.0).better_than(&r(40.0, 1, 0.0)),
            "a reserve kept back beats an ash spent"
        );

        // Above the hygiene terms, though, so it is not merely decorative.
        assert!(r(0.0, 1, 99.0).better_than(&r(0.0, 0, 0.0)), "shedding beats a tidier board");
    }

    /// The predicate reads the game's ligature **name**, and only the ones we want gone.
    ///
    /// `QU` is the control and the reason this is not a test about two-character tiles: it is a
    /// ligature, it is not a nuisance, and a rule that shed every ligature would throw away the `U`
    /// that makes a `Q` playable.
    #[test]
    fn only_the_ligatures_worth_losing_count_as_shed() {
        let lig = |letter: &str, name: &str| Tile {
            letter: letter.to_string(),
            quality: crate::observe::board::Quality {
                ligature: Some(name.to_string()),
                ..Default::default()
            },
        };
        let tiles = vec![lig("AE", "ash"), lig("QU", "QU"), Tile::plain("E"), lig("AE", "ash")];

        assert_eq!(shed(&tiles, &[0]), 1, "the ash counts");
        assert_eq!(shed(&tiles, &[1]), 0, "the QU does not — it is a ligature we want");
        assert_eq!(shed(&tiles, &[2]), 0, "and a plain letter is not a ligature at all");
        assert_eq!(shed(&tiles, &[0, 3]), 2, "two ashes are worth two");
        assert_eq!(shed(&tiles, &[]), 0, "a word that spends nothing sheds nothing");
    }

    /// Two columns of three. Flat indices are column-major with row 1 at the bottom
    /// (`layout.rs:55`), so column 1 is 0,1,2 bottom-to-top and column 2 is 3,4,5.
    fn two_columns_of_three() -> Geometry {
        Geometry { rows_per_col: vec![3, 3], ..Default::default() }
    }

    fn board(letters: &[&str]) -> Vec<Tile> {
        letters.iter().map(|l| Tile::plain(l)).collect()
    }

    /// The case from the request, verbatim: one column holding two exclamation tiles, and a
    /// candidate that removes a single tile beneath both. Each hazard moves down one slot, so the
    /// move is worth **2** rather than 1.
    #[test]
    fn two_hazards_over_one_removed_tile_count_twice() {
        let Some(sc) = scorer() else { return };
        let g = two_columns_of_three();
        // Column 1, bottom to top: E, !, ! — so removing the E drops both hazards one slot each.
        let tiles = board(&["E", "!", "!", "A", "B", "C"]);
        assert_eq!(hazard_fall(&tiles, &g, &[0], &sc), 2);
    }

    #[test]
    fn only_tiles_below_a_hazard_in_its_own_column_move_it() {
        let Some(sc) = scorer() else { return };
        let g = two_columns_of_three();
        // Column 1: E, !, R. Column 2: A, B, C.
        let tiles = board(&["E", "!", "R", "A", "B", "C"]);
        // Below it, same column: the hazard falls one.
        assert_eq!(hazard_fall(&tiles, &g, &[0], &sc), 1);
        // Above it, same column: nothing moves.
        assert_eq!(hazard_fall(&tiles, &g, &[2], &sc), 0);
        // A whole other column: nothing moves.
        assert_eq!(hazard_fall(&tiles, &g, &[3, 4, 5], &sc), 0);
        // Both tiles under it would be two slots, but only one is under it.
        assert_eq!(hazard_fall(&tiles, &g, &[0, 2], &sc), 1);
    }

    #[test]
    fn a_board_with_no_hazards_scores_zero_however_much_is_removed() {
        let Some(sc) = scorer() else { return };
        let g = two_columns_of_three();
        let tiles = board(&["E", "A", "R", "T", "H", "S"]);
        assert_eq!(hazard_fall(&tiles, &g, &[0, 1, 2, 3, 4, 5], &sc), 0);
    }

    /// Wood is the material of ordinary letters, so this has to be checked through the scorer rather
    /// than through `quality.material`, which is `None` on almost every tile.
    #[test]
    fn wood_only_reads_the_letter_implied_material_not_just_an_explicit_one() {
        let Some(sc) = scorer() else { return };
        let tiles = board(&["E", "A", "R"]);
        let all_wood =
            tiles.iter().all(|t| sc.material_name(t).is_some_and(|m| m.starts_with(WOOD)));
        // If this is false the fixture is wrong, not the code -- and the assertion below would then
        // be passing for the wrong reason.
        assert!(
            all_wood,
            "expected plain letters to resolve to wood: {:?}",
            tiles.iter().map(|t| sc.material_name(t)).collect::<Vec<_>>()
        );
        assert!(wood_only(&tiles, &[0, 1, 2], &sc));
        assert!(!wood_only(&tiles, &[], &sc), "an empty word cannot have killed anything");
    }

    /// The five wood items, each by the flags it actually carries. Named after the items rather
    /// than after "plain" and "braced", because lumping them cost a live run its buckler.
    #[test]
    fn each_wood_item_pays_for_its_own_reasons() {
        let idol = |f: &str| f == "onWoodKillGainHealth";
        let buckler = |f: &str| f == "onWoodKillGainArmour";
        let idol_buckler = |f: &str| idol(f) || buckler(f);
        let braced = |f: &str| idol_buckler(f) || f == "onWoodKillQueueWoodbraced";
        // (injured, bleeding, armour_room)
        let hurt = (true, false, true);
        let full = (false, false, true);
        let bleeding = (true, true, true);
        let capped = (false, false, false);
        // Injured, so the heal would land, but armour has nowhere to go. The state that separates
        // the two payouts -- without it, "armour full" and "not injured" are never seen apart.
        let capped_but_hurt = (true, false, false);
        let pays = |g: &dyn Fn(&str) -> bool, (i, b, a): (bool, bool, bool)| {
            Preferences::from_flags(g, i, b, a).wood_only_pays
        };

        // The **Wooden idol** is a heal and only a heal: injured and not bleeding, or nothing.
        assert!(pays(&idol, hurt));
        assert!(!pays(&idol, full), "a heal at full health is thrown away");
        assert!(!pays(&idol, bleeding), "rpgview.lua:1080 skips the heal branch");
        assert!(pays(&idol, capped_but_hurt), "armour being full says nothing about a heal");

        // The **Wooden buckler** is armour and only armour -- the one a live run was carrying, and
        // the case the old single gate got wrong in both directions.
        assert!(pays(&buckler, full), "4 armour is worth having at full health");
        assert!(pays(&buckler, bleeding), "bleeding does not cancel armour");
        assert!(!pays(&buckler, capped), "but a full armour bar does");

        // The **Wooden idol buckler** has both, so either reason is enough.
        assert!(pays(&idol_buckler, full), "the armour still pays");
        assert!(pays(&idol_buckler, capped_but_hurt), "and at full armour the heal does");
        assert!(
            !pays(&idol_buckler, capped),
            "neither payout is available: no heal wanted, no room"
        );

        // The **Braced buckler** adds a queued tile, which cares about none of it.
        for state in [hurt, full, bleeding, capped] {
            assert!(pays(&braced, state), "onWoodKillQueueWoodbraced always pays");
        }
    }

    /// The single case that made the split necessary, stated on its own.
    ///
    /// A live run carried `armourBucklerWood` at 1/20 and the preference *was* on -- but only
    /// because it happened to be injured. At full health, or while bleeding, the old rule would have
    /// declined 4 free armour for reasons that belong to a heal the item does not grant.
    #[test]
    fn armour_is_not_a_heal_and_is_not_gated_like_one() {
        let buckler = |f: &str| f == "onWoodKillGainArmour";
        assert!(Preferences::from_flags(buckler, false, true, true).wood_only_pays);
        // The only thing that switches it off is having nowhere to put the armour.
        assert!(!Preferences::from_flags(buckler, true, false, false).wood_only_pays);
    }

    /// The Targe pays for ending a word on braced wood, not for a wood-only word, so it must not
    /// switch this on. Its flags are `tileRefillSpecialWoodbraced` / `tileQueueSpecialWoodbraced`
    /// (`items/specialtilegear.lua:31-34`) and neither is a wood-*kill* flag.
    #[test]
    fn the_targe_alone_does_not_make_wood_only_worth_chasing() {
        let targe =
            |f: &str| f == "tileRefillSpecialWoodbraced" || f == "tileQueueSpecialWoodbraced";
        // No combination of health and bleeding makes it pay, because it has no wood-kill flag at all.
        for injured in [true, false] {
            for bleeding in [true, false] {
                for room in [true, false] {
                    assert!(
                        !Preferences::from_flags(targe, injured, bleeding, room).wood_only_pays
                    );
                }
            }
        }
    }

    /// Which words queue infinitives, read off the live links: the longest registered suffix
    /// decides, a bare suffix is not a suffixed word, and an inactive link queues nothing.
    #[test]
    fn infinitives_follow_the_live_links_from_the_word_suffix() {
        // The burning-board save's gauntlet: ING>ED, ED>ES, ES>LY live, LY>ING inactive.
        let flags = ["onSubmitSuffixINGtoED", "onSubmitSuffixEDtoES", "onSubmitSuffixEStoLY"];
        let has = |f: &str| flags.contains(&f);
        let g = Infinitives::from_gear(["goldIdol", "suffixGauntlet"], has);
        assert_eq!(g.source, Some(InfinitiveSource::Gauntlet));
        assert_eq!(g.created("ROASTED"), 1);
        assert_eq!(g.created("singing"), 1, "case does not matter");
        assert_eq!(g.created("LOVELY"), 0, "LY>ING is inactive");
        assert_eq!(g.created("ED"), 0, "the suffix must be shorter than the word");
        assert_eq!(g.created("BIGGEST"), 0, "EST is the suffix, and the gauntlet has no EST link");
        assert_eq!(g.created("CAT"), 0);

        // The scales' own six links: ING goes to ER, not the gauntlet's ED.
        let six = [
            "onSubmitSuffixEDtoES",
            "onSubmitSuffixEStoLY",
            "onSubmitSuffixLYtoING",
            "onSubmitSuffixINGtoER",
            "onSubmitSuffixERtoEST",
            "onSubmitSuffixESTtoED",
        ];
        let j = Infinitives::from_gear(["scalesOfJustice"], |f| six.contains(&f));
        assert_eq!(j.source, Some(InfinitiveSource::Justice));
        assert_eq!(j.created("BIGGEST"), 1);
        assert_eq!(j.created("ROASTED"), 1);
        assert_eq!(j.effect("SINGING").unwrap().queues, [Some("ER"), None]);

        // Any queuing link counts, whichever item grants it — the user, 2026-11-07. The Scales of
        // truth carry ER>EST and EST>ER.
        let truth = ["onSubmitSuffixERtoEST", "onSubmitSuffixESTtoER"];
        let t = Infinitives::from_gear(["scalesOfBalance"], |f| truth.contains(&f));
        assert_eq!(t.source, Some(InfinitiveSource::Comparative));
        assert_eq!(t.created("BIGGEST"), 1);
        assert_eq!(t.effect("BIGGER").unwrap().queues, [Some("EST"), None]);
        assert_eq!(t.created("ROASTED"), 0);
    }

    /// What a word does with the tile gear: the Scales queue the next suffix and upgrade a tile on
    /// each suffix word, -ER and -EST through their own flag; the lesser ER/EST scales only
    /// upgrade; the Gilding bastard sword gilds on seven letters.
    #[test]
    fn a_word_queues_upgrades_and_gilds() {
        let six = |f: &str| {
            ["EDtoES", "EStoLY", "LYtoING", "INGtoER", "ERtoEST", "ESTtoED"]
                .iter()
                .any(|l| f == format!("onSubmitSuffix{l}"))
        };
        let flags = |f: &str| match f {
            "submitSuffixUpgrade" | "submitERESTupgrade" => 1,
            _ => 0,
        };
        let j = Infinitives::from_gear(["scalesOfJustice"], six).with_upgrades(flags);
        let e = |w: &str| j.effect(w).unwrap();
        assert_eq!((e("RUNNING").suffix, e("RUNNING").queues), (Some("ING"), [Some("ER"), None]));
        assert_eq!((e("BIGGER").queues, e("BIGGER").upgrades), ([Some("EST"), None], 1));
        assert_eq!((e("BIGGEST").queues, e("BIGGEST").upgrades), ([Some("ED"), None], 1));
        assert_eq!(j.upgraded("JUMPED"), 1);
        assert_eq!(j.effect("CAT"), None, "no suffix");
        let lesser = Infinitives::default().with_upgrades(|f| (f == "submitERESTupgrade") as i64);
        assert_eq!(
            lesser.effect("BIGGER").map(|e| (e.queues, e.upgrades)),
            Some(([None, None], 1))
        );
        assert_eq!(lesser.effect("JUMPED"), None, "-ED is not theirs");
        assert_eq!(lesser.created("BIGGER"), 0, "upgrading is not queuing");
        let sword = Infinitives::default().with_upgrades(|f| 2 * (f == "submit7gild") as i64);
        assert_eq!(sword.upgraded("PLAYING"), 2, "seven letters");
        assert_eq!(sword.upgraded("PLAYS"), 0);
        assert_eq!(
            sword.effect("PLAYING").map(|e| (e.suffix, e.upgrades, e.gilds)),
            Some((Some("ING"), 0, 2))
        );
    }

    /// Upgrades rank just above the queued infinitives, both above the board-tidiness terms and
    /// below every tile kept: the user's order, 2026-11-07.
    #[test]
    fn upgrades_then_queues_are_the_last_gains() {
        let r = |wildcards_net: f64, upgrades: i64, infinitives: i64, shed: usize| Rank {
            fire_eaten: 0,
            halves_attack: false,
            wood_only: false,
            heals_fully: false,
            hazards_cleared: 0,
            gain: 0,
            gold_spent: 0.0,
            gems_net: 0.0,
            wildcards_net,
            upgrades,
            infinitives,
            shed,
            hazard_fall: 0,
            deviation: 0.0,
        };
        assert!(r(0.0, 0, 0, 0).better_than(&r(-1.0, 1, 1, 0)), "a wildcard kept first");
        assert!(r(0.0, 1, 0, 0).better_than(&r(0.0, 0, 1, 0)), "an upgrade over a queue");
        assert!(r(0.0, 0, 1, 0).better_than(&r(0.0, 0, 0, 1)), "a queue over tidiness");
    }

    /// The live board, cut to one column: `A D ! P` bottom to top. Only removing both tiles under
    /// the `!` brings it to the bottom row; one of them is a fall of 1 and clears nothing.
    #[test]
    fn a_hazard_is_cleared_only_when_it_reaches_the_bottom_row() {
        let Some(sc) = scorer() else { return };
        let g = Geometry { rows_per_col: vec![4], ..Default::default() };
        let tiles = board(&["A", "D", "!", "P"]);
        assert_eq!(hazards_cleared(&tiles, &g, &[0, 1], &sc), 1);
        assert_eq!(hazards_cleared(&tiles, &g, &[1], &sc), 0, "row 2 is not the bottom");
        assert_eq!(hazards_cleared(&tiles, &g, &[3], &sc), 0, "above it does nothing");
        let low = board(&["!", "D", "A", "P"]);
        assert_eq!(hazards_cleared(&low, &g, &[1, 2], &sc), 0, "already at the bottom: not earned");
    }

    /// The user, on that board: a kill that clears the `!` beats one that ends on braced wood for
    /// 4 armour.
    #[test]
    fn clearing_a_hazard_outranks_a_gain() {
        let r = |hazards_cleared: usize, gain: i64| Rank {
            fire_eaten: 0,
            halves_attack: false,
            wood_only: true,
            heals_fully: false,
            hazards_cleared,
            gain,
            infinitives: 0,
            gems_net: 0.0,
            gold_spent: 0.0,
            wildcards_net: 0.0,
            upgrades: 0,
            shed: 0,
            hazard_fall: 0,
            deviation: 0.0,
        };
        assert!(r(1, 0).better_than(&r(0, 4)));
        assert!(r(1, 4).better_than(&r(1, 0)), "and the gain still breaks the tie");
    }

    /// The user's board, 2026-11-07: `FIR` is wood only and earns the Woodsman tunic's armour,
    /// `DRIP` spends the gold-bordered D under the `!` and brings it down. The `!` wins, over the
    /// wood payout and the full heal both.
    #[test]
    fn clearing_a_hazard_outranks_the_wood_and_heal_payouts() {
        let r = |hazards_cleared: usize, wood_only: bool, heals_fully: bool| Rank {
            fire_eaten: 0,
            halves_attack: false,
            wood_only,
            heals_fully,
            hazards_cleared,
            gain: 0,
            infinitives: 0,
            gems_net: 0.0,
            gold_spent: 0.0,
            wildcards_net: 0.0,
            upgrades: 0,
            shed: 0,
            hazard_fall: 0,
            deviation: 0.0,
        };
        assert!(r(1, false, false).better_than(&r(0, true, true)));
        assert!(r(1, true, false).better_than(&r(1, false, true)), "then wood, then the heal");
    }

    /// Below health and armour, and below hoarding: the user's `AXES` and `DENS`, 2026-11-07.
    #[test]
    fn infinitives_rank_below_gain_and_below_hoarding() {
        let r = |gain: i64, infinitives: i64, hoarded: f64| Rank {
            fire_eaten: 0,
            halves_attack: false,
            wood_only: false,
            heals_fully: false,
            hazards_cleared: 0,
            gain,
            infinitives,
            gems_net: 0.0,
            gold_spent: hoarded,
            wildcards_net: 0.0,
            upgrades: 0,
            shed: 0,
            hazard_fall: 0,
            deviation: 0.0,
        };
        assert!(r(1, 0, 0.0).better_than(&r(0, 2, 0.0)), "a point of health or armour first");
        assert!(r(0, 0, 0.0).better_than(&r(0, 1, 11.0)), "DENS over AXES: keep the gold X");
        assert!(r(0, 1, 11.0).better_than(&r(0, 0, 11.0)), "the same gold: the infinitive decides");
    }

    #[test]
    fn a_payout_outranks_tidier_letters_and_wood_outranks_a_falling_hazard() {
        let wood = Rank {
            fire_eaten: 0,
            halves_attack: false,
            wood_only: true,
            heals_fully: false,
            gain: 0,
            gems_net: 0.0,
            gold_spent: 0.0,
            wildcards_net: 0.0,
            upgrades: 0,
            shed: 0,
            hazard_fall: 0,
            hazards_cleared: 0,
            infinitives: 0,
            deviation: 99.0,
        };
        let hazard = Rank {
            fire_eaten: 0,
            halves_attack: false,
            wood_only: false,
            heals_fully: false,
            gain: 0,
            gems_net: 0.0,
            gold_spent: 0.0,
            wildcards_net: 0.0,
            upgrades: 0,
            shed: 0,
            hazard_fall: 5,
            hazards_cleared: 0,
            infinitives: 0,
            deviation: 1.0,
        };
        assert!(wood.better_than(&hazard), "a wood-only kill outranks any number of falls");

        let falls = Rank {
            fire_eaten: 0,
            halves_attack: false,
            wood_only: false,
            heals_fully: false,
            gain: 0,
            gems_net: 0.0,
            gold_spent: 0.0,
            wildcards_net: 0.0,
            upgrades: 0,
            shed: 0,
            hazard_fall: 2,
            hazards_cleared: 0,
            infinitives: 0,
            deviation: 50.0,
        };
        let tidy = Rank {
            fire_eaten: 0,
            halves_attack: false,
            wood_only: false,
            heals_fully: false,
            gain: 0,
            gems_net: 0.0,
            gold_spent: 0.0,
            wildcards_net: 0.0,
            upgrades: 0,
            shed: 0,
            hazard_fall: 1,
            hazards_cleared: 0,
            infinitives: 0,
            deviation: 0.0,
        };
        assert!(falls.better_than(&tidy), "a falling hazard outranks a tidier board");

        let a = Rank {
            fire_eaten: 0,
            halves_attack: false,
            wood_only: false,
            heals_fully: false,
            gain: 0,
            gems_net: 0.0,
            gold_spent: 0.0,
            wildcards_net: 0.0,
            upgrades: 0,
            shed: 0,
            hazard_fall: 1,
            hazards_cleared: 0,
            infinitives: 0,
            deviation: 3.0,
        };
        let b = Rank {
            fire_eaten: 0,
            halves_attack: false,
            wood_only: false,
            heals_fully: false,
            gain: 0,
            gems_net: 0.0,
            gold_spent: 0.0,
            wildcards_net: 0.0,
            upgrades: 0,
            shed: 0,
            hazard_fall: 1,
            hazards_cleared: 0,
            infinitives: 0,
            deviation: 4.0,
        };
        assert!(a.better_than(&b), "with the payouts equal, lower deviation wins");
        assert!(!b.better_than(&a));
        assert!(!a.better_than(&a), "better_than is strict");
    }

    /// `A D ! P`, bottom to top: the `!` is cleared once both tiles under it go, as in
    /// [`hazards_cleared`]; one of them is not enough, and one already at the bottom earns nothing.
    #[test]
    fn exclamations_are_cleared_from_the_bottom_row() {
        let g = Geometry { rows_per_col: vec![4], ..Default::default() };
        let one = Falloff::default();
        let tiles = board(&["A", "D", "!", "P"]);
        assert_eq!(exclamations_cleared(&tiles, &g, &[0, 1], one), 1);
        assert_eq!(exclamations_cleared(&tiles, &g, &[1], one), 0, "row 2 is not the bottom");
        assert_eq!(exclamations_cleared(&tiles, &g, &[3], one), 0, "above it does nothing");
        let low = board(&["!", "D", "A", "P"]);
        assert_eq!(exclamations_cleared(&low, &g, &[1, 2], one), 0, "falls whatever is played");
        // `tileboard!bottom+`: two bottom rows, so one tile out from under is enough.
        let two = Falloff { rows: 2, ..one };
        assert_eq!(exclamations_cleared(&tiles, &g, &[1], two), 1);
    }

    /// The falloff repeats while anything fell (`tileboard.lua:1264-1276`), so a stack of `!` and
    /// digits comes off whole; with `tileFalloffWildcardInstead` the first becomes a wildcard and
    /// holds the rest up. A `!!` loses one mark and stays.
    #[test]
    fn exclamations_cascade_and_multi_marks_lose_one() {
        let g = Geometry { rows_per_col: vec![5], ..Default::default() };
        let one = Falloff::default();
        let stack = board(&["A", "!", "3", "!", "B"]);
        assert_eq!(exclamations_cleared(&stack, &g, &[0], one), 2, "both, the digit between");
        let held = Falloff { to_wildcard: true, ..one };
        assert_eq!(exclamations_cleared(&stack, &g, &[0], held), 1, "the wildcard holds the rest");
        let multi = board(&["A", "!!", "B", "C", "D"]);
        assert_eq!(exclamations_cleared(&multi, &g, &[0], one), 1, "one mark of two");
    }

    /// A wood-only word earns the wood-kill credit only if its blow makes the kill: an enemy the
    /// poison finishes dies with no word, so the buckler pays nothing. `rank`'s damage is the
    /// blow's, toxin left out.
    #[test]
    fn a_poison_kill_earns_no_wood_credit() {
        let Some(sc) = scorer() else { return };
        let g = Geometry { rows_per_col: vec![3], ..Default::default() };
        let tiles = vec![of("C", "wood"), of("A", "wood"), of("T", "wood")];
        let prefs = Preferences { wood_only_pays: true, kill_at: Some(10), ..Default::default() };
        let r = |damage| {
            rank(
                "CAT",
                &tiles,
                &g,
                &[0, 1, 2],
                &sc,
                &Context { prefs, ..Default::default() },
                damage,
            )
        };
        assert!(r(10).wood_only, "the blow kills");
        assert!(!r(9).wood_only, "the toxin would finish it");
        let open = Preferences { kill_at: None, ..prefs };
        assert!(
            rank(
                "CAT",
                &tiles,
                &g,
                &[0, 1, 2],
                &sc,
                &Context { prefs: open, ..Default::default() },
                0
            )
            .wood_only
        );
    }
}
