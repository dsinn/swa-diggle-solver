# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) — before 1.0, a minor version may change
behaviour freely.

## [Unreleased]

## [0.2.0] - 2026-11-06

Tested against game version v53.2.

### Added

- **Diggle Advisor** (`diggle_advisor`): a window that shows Diggle's ranking of every playable word
  while you play the game yourself. It sends no input and reads only the Steam save folder, never
  LOVE's. It shows:
  - each letter on its tile in the material's sampled face and ink colours, with rings for metal
    borders, inlaid wood, variants and fire, the historical ligature glyphs, and wildcards drawn
    handwritten;
  - columns for damage, the lexicon bonus (shown as its multiplier and emoji), what the word gives
    you (`+3 ❤️ +4 🛡️`), the held spells it qualifies for, and what it does to the enemy.
  - Each word is scored by its best cast, so Turn undead doubles bone words against the undead.
    The enemy bestiary is read from the game's files.
  - It follows arcade challenge saves (Boggling anxiety) as well as normal runs.
  - It remembers its size, "Stay on top", the log's visibility and height, and the halfling stall
    toggle, in `%LOCALAPPDATA%\DiggleAdvisor\settings.toml`.
- `search::ranked`: every typeable word, in the order Diggle would fall back through them.
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
- Diggle presses nothing while an event's v53.2 combat cutscene plays.
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
