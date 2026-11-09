# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) — before 1.0, a minor version may change
behaviour freely.

## [Unreleased]

### Advisor

- **Words that need the mouse are marked.** A 🖱️ column flags any word with a tile the keyboard
  would not pick, worked out from the game's own keyboard rules (`Typist::clicks`). With "Underline
  mouse tiles" ticked (the default), each tile to click gets a green bar beneath it.
- **Every item in the table explains itself on hover**: what a gain queues, why a hit ignores
  armour, what bleed does, which lexicons the enemy is weak to or resists, and which spell the
  damage assumes (`annotate::Item`).
- **The table shows more of what a word does.** The Gain column shows queued gems, tile upgrades,
  `!` marks cleared and wildcards; the Damage column shows the ice, burn and bleed beside the hit.
  The Bonus column is now **Mult.**, narrower, and shows resists and immunities as well as
  weaknesses.
- The Spell column leads with the spell to pick when the game asks "Submit as", and the effect
  column says when a kill has no margin or relies on the enemy's status to finish it.
- **"Collapse table out of combat"** (off by default) rolls the window up to its header and
  checkboxes while no fight is on, and drops it back when one starts (`advisor::shade`).
- **"Censor profanity"** (off by default) pixelates the vowels of profane words the way the game's
  own censor does, so the word stays readable enough to play (`advisor::censor`).
- The checkboxes have a row of their own under the header, and "Stay on top" is now "Always on
  top".
- The Gain and Damage columns draw the game's own Infinitives gauntlet, Scales of justice and rapier
  art in place of 🧤, ⚖️ and 🤺. The art is built into the Advisor, so no game checkout is needed.
- Clearer tile colours: the gems are drawn in light, distinct colours, a black ring now marks only
  braced wood and the bleed and toxin metals, and gold borders are pure gold.
- A cell too narrow for its text ends in `…`, and the window has its own icon, one for in combat
  and one for out.
- An enemy finished by poison no longer shows a buckler or Well-Rested heal, which the game doesn't
  give.
- **The list updates faster after each move.** The lexicons and the bestiary are read once per game
  version instead of every turn, which cost up to 5 seconds while another program was writing to
  the disk (`game::version::ByVersion`).
- `diggle_advisor --timing-log` writes `%LOCALAPPDATA%\DiggleAdvisor\timing.log`, timing each save
  from the game's write to the list on screen.

### Combat word selection

Diggle and the Advisor share these: how much damage a word is scored at, and how words are ranked.

#### Damage

- **Words an enemy resists are scored down, not dropped.** A resisted lexicon's words used to be
  left out entirely, even when they would heal or armour you. They are now scored as the game
  scores them, and an immune lexicon scores zero but can still rank on what it gains
  (`Modifiers::modifier_for_base`).
- **The tiles a word uses are chosen, not just its letters.** Where two tiles carry the same letter,
  the ranking picks whichever pays better there, a special tile or its plain twin, and Diggle plays
  the tiles that were scored (`typist::placements`).
- **Spell tiles count**: a Corundum, Chrysocolla, Diamond or Multiocular O in the right place makes
  the word a spell of its school, with Adept or Versed's boost (`spells::tile_school`).
- **Ice, burn and bleed count as damage**, as toxin already did, when they tick before the enemy
  acts (`Modifiers::bleed`).
- **The Totem is modelled**, including on irregular boards (`Geometry::full_column_tiles`).
- Fixed several words being listed as stronger than the game makes them: a cooper ligature no
  longer adds a point the game never gives, a wildcard's letter now spoils a heterogram as it does
  in the game, a word the game force-casts as its only spell is scored as that cast, and the halved
  armour cap rounds up as the game's does. These could make the Advisor call a word a kill when it
  wasn't.
- Diggle is scored as casting the best spell, as the Advisor already was. Pressing the cast button
  isn't written yet, so until it is Diggle can occasionally land short (`src/cast.rs`).
- New tool, `score_check <combatSaveData>`: scores every listed word with the game's own Lua and
  lists any where the solver promises more damage than the game deals.

#### Ranking

- **Valuable tiles are kept**: of two equally good words, the one that spends less gold wins, then
  fewer gems, then fewer wildcards, all ahead of infinitives. Gems are spent only when a plainer
  word wouldn't do the job. Serrated gold is now kept like solid gold; it used to be spent freely.
- **Gems and wildcards queued by gear count**, such as the dragon, walrus and unicorn items and the
  Bookkeepers bassanello, net of what the word spends (`pick::TileLedger`).
- **Clearing a `!` matters more.** Bringing one to the bottom outranks every payout, and bringing
  one partway down ranks above keeping wildcards.
- **Under Express mail, what a word queues ranks first**, since a queued tile can be played the
  same turn (`Preferences::queue_first`).
- **Burning the enemy counts when it will hit back**: between two non-killing words that hit
  equally hard, the one that halves the enemy's next attack ranks first (`Rank::halves_attack`).
- **Tile upgrades and queued infinitives rank last** among gains, now counted from every item that
  upgrades, gilds or queues tiles on submit.
- Fixed a hurt player being ranked as unhurt without Well-Rested, so a Diamond's heal went
  uncredited. Missing health is now read from the save.

## [0.2.0] - 2026-11-06

Tested against game version v53.2.

### Added

#### Diggle Advisor

`diggle_advisor`, a new binary: a window that shows Diggle's ranking of every playable word while
you play the game yourself. It sends no input and reads only the Steam save folder, never LOVE's
(`src/advisor/watch.rs`).

- It renders each letter on its tile in the material's sampled face and ink colours, with rings for
  metal borders, inlaid wood, variants and fire, the historical ligature glyphs, and wildcards drawn
  handwritten.
- There are columns for damage, the lexicon bonus (its multiplier and emoji), what the word gives
  you (`+3 ❤️ +4 🛡️`), the held spells it qualifies for, and what it does to the enemy.
- Each word is scored by its best cast (`SpellBook`, `src/spells.rs`), so Turn undead doubles bone
  words against the undead. The save doesn't record undeadness, so `advisor::bestiary` reads it
  from the game's enemy and name files.
- It follows arcade challenge saves (Boggling anxiety) as well as normal runs.
- It remembers its size, "Stay on top", the log's visibility and height, and the halfling stall
  toggle, in `%LOCALAPPDATA%\DiggleAdvisor\settings.toml`.
- To support it, a turn's plan is built outside the fight loop (`turn::plan`), and
  `search::ranked` lists every typeable word in the order Diggle would fall back through them.
  `ranked_head_matches_choice_for_on_every_fixture` holds its head to Diggle's own choice.

#### Misc

- Rewards are chosen by their bonus, which the v53.2 reward screen now prints.
- Modelled in the ranking:
  - The rapier: a word that starts and ends with the same letter ignores armour. Words that don't
    kill are ranked by the share of health left that they remove.
  - The letter-chaining curse (`wordRequirementAdjacent`) and Enchanted chain's +25%, including the
    hexagonal boards' missing diagonals.
  - What a word gives the player: braced wood's armour, a Diamond's heal and regen, and the Silver
    chain coif's armour per silver tile. These rank above board hygiene.
  - Infinitives queued by the Infinitives gauntlet or the Scales of justice.
  - Toxin applied by a word counts as damage.
  - A word that brings a "!" or bomb to the bottom row outranks gains.
- Halfling stall: on even turns, when the enemy can't hit a halfling, Diggle keeps the enemy alive
  rather than killing it.

### Changed

- The frugal kill is gone, and Well-Rested stacks are no longer held in reserve. Any wound, with the
  heal live and no bleed, aims for a kill that heals fully.
- Every "hardest hit" fallback breaks ties with the shared ranking, not by search order.

### Fixed

- The shrine word lists are regenerated for v53.2's dictionary.
- A reward bonus no longer hides the offers listed after it.
- Diggle presses nothing while an event's combat cutscene plays (`ui/rpgcutscene.lua`, new in v53.2
  for random character encounters: the Highwayman, Woodsman and Injured paladin events). The
  cutscene binds affirmative to *skip*, so `clear_text_screen` could skip it and spend its later
  presses on the event's choices; it now waits the cutscene out.
- The saved board is read without its letter queue, so words are no longer typed against tiles
  that aren't there.
- Skeletons are always killed, even when Turn undead has terrified them.
- Tests read the Well-Rested saves from committed fixtures, and the hygiene sweep works on CRLF
  checkouts.

## [0.1.0] - 2026-08-24

First public release. A run from an empty save cleared the level 8 anomaly unattended, on game
version v52.4. See the README for the video and the limits of that claim.

[Unreleased]: https://github.com/dsinn/swa-diggle-solver/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/dsinn/swa-diggle-solver/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/dsinn/swa-diggle-solver/releases/tag/v0.1.0
