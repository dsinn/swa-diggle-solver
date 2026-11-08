# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) — before 1.0, a minor version may change
behaviour freely.

## [Unreleased]

### Added

- **The word's tiles are chosen, not only its letters.** Where two tiles carry the same letter, the
  ranking tries each and keeps the better: a serrated, toxin, gem or braced tile where its position
  pays, the plain twin where it would do nothing (`typist::placements`). Diggle plays the tiles
  that were scored.
- **Spell tiles boost the word.** A Corundum or Chrysocolla first (a Chrysocolla last too), a
  Diamond last, or a Multiocular O makes a plain word a spell of its school, boosted by Adept or
  Versed's +50% (`spells::tile_school`).
- **Ice and burn from a gem count as damage**, like toxin: a Chrysocolla first ticks its ice, a
  Corundum its burn, before the enemy acts. The damage column shows each, `30 +1 ❄️`.
- **Gems queued by gear count in the ranking.** The dragon (a fire word queues a Corundum), the
  walrus (an ice word, a Chrysocolla) and the unicorn (a letter four times, a Diamond), staves and
  trinkets alike, rank a word that queues a gem above the infinitives. The Gain column shows it,
  `+1 💎` (`pick::GemQueue`).
- The Bookkeepers bassanello's wildcards count in the ranking, as the lowest of the tile gains,
  below the infinitives.
- Both are counted **net of what the word spends**, each against its own kind: a word that uses up
  two Corundums to queue one, or two wildcards to queue one, is ranked as having lost one
  (`pick::TileLedger`).
- Advisor's Gain column shows the `!` marks a word clears (`2❗`) and the bassanello's wildcards
  (`+2 ⬜`).
- Advisor's Spell column leads with the spell its damage assumes, the one to pick when the game
  asks "Submit as". The effect column says how sure a kill is: `kills - no margin` at exactly
  lethal, `kills - status finishes it` when the hit leaves the enemy alive and its status kills it
  before it acts.
- `score_check <combatSaveData>`: scores every listed word with the game's own Lua (`words.score`,
  the modifier files, the spells) and lists any where the solver promises more damage than the game
  deals.
- `diggle_advisor --timing-log` writes a timing log, `%LOCALAPPDATA%\DiggleAdvisor\timing.log`, for
  each save from the game's write to the list on screen.
- Groundwork for Diggle to cast spells: which "Submit as" slot holds the spell, which key casts it,
  and a check for spells the game has that the solver does not model (`src/cast.rs`). Not yet
  pressed.

### Changed

- **Gems are kept.** A Corundum or Chrysocolla, and a Diamond while at full health, is spent only
  when a plainer word would not do the job; between equal placements the plain twin is shown. Of
  two kills, the one that loses fewer gems ranks first, above an infinitive (gold still ranks
  below one).
- Diggle is scored as casting the best spell, as the Advisor's user is. Pressing the button is not
  written yet, so until it is Diggle can land short where a spell scored higher.
- Advisor draws Corundum a clear red.
- **Advisor shows the game's own gauntlet and scales.** The Gain column draws the Infinitives
  gauntlet's and the Scales of justice's item art in place of 🧤 and ⚖️, sized like the emoji
  beside them. The two icons are built into the Advisor, so it needs no game checkout to show
  them. Every gauntlet shows the full gauntlet, a partial one included (`InlineIcon`,
  `assets/advisor/`).

### Fixed

- **Words the Advisor called kills fell short.** With `alwaysCastSpell` (the game's default), a word
  with exactly one spell is cast as that spell: a metal word under Book of metal hits only for what
  your missing armour does not absorb, and a lone Four heal or Vowels hits for nothing. Both are now
  scored that way.
- The halved armour cap rounds up, as the game's does; odd maximum health made Metal look a point
  stronger.
- An enemy finished by poison gives no buckler or Well-Rested heal, and the Advisor no longer
  shows one.

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
