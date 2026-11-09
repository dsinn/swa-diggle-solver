# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) — before 1.0, a minor version may change
behaviour freely.

## [Unreleased]

### Advisor

- **The Bonus column is now "Mult."**, 76 pixels wide rather than 90, with a "Multiplier" tooltip
  on its header. It shows resists too now (`x0.7 🤬`), and an immune lexicon's tooltip says the
  enemy takes no damage from those words.
- **"Collapse candidates out of combat"** (unticked by default) rolls the window up to its header
  and checkboxes while no fight is on, with an elastic ease, and drops it back to its earlier
  height when a fight starts. A fight that starts or ends partway through turns the window round
  from wherever it is. Unticking it while rolled up drops the window at once; ticking or unticking
  it during a fight does nothing until the fight ends. A maximised or minimised window catches up
  when you restore it. While rolled up, dragging an edge changes only the width, and the height
  remembered between launches is the open one (`advisor::shade`).
- The Advisor has its own window icon, one for in combat and one for out of combat.
- **"Censor profanity"** (unticked by default) pixelates words from the profane book the way the
  game's own censor does, a 4×4 mosaic over each tile with every block the average of what it
  covers, so the tile's ring and the letter's strokes show through as the game's frame and
  lettering do. Only the vowels are censored, so the word can
  still be played. `y` counts as a vowel when neither neighbour is one (`gym`, not `yes`); a word
  with no vowel hides every other letter from the second (`advisor::censor`). It applies whether
  or not you hold the profane book.
- **The checkboxes have a row of their own** under the header line, wrapping onto a second row in
  a narrow window, so the header has the full width.
- **Words that need the mouse are marked.** A 🖱️ column flags any word with a tile the keyboard
  would not pick, worked out with the game's own keyboard rules, in order (`Typist::clicks`). With
  "Underline mouse tiles" ticked (the default; hover it for a tooltip), each tile to click gets a
  green bar beneath it: type the word and click those.
- **Each item in the table explains itself on hover.** Every emoji-bearing item has its own
  tooltip: `+2 ⬜` says "Queues 2 wildcard tiles", `+3 🧪` the toxin the enemy loses before it
  acts, `🤺` why the hit ignores armour, the Mult. column which lexicons the enemy is weak to, and
  the Spell column which spell the damage assumes (`annotate::Item`). A suffix word's 🧤 or ⚖️
  says which tiles the suffix queues, "Ends in -ING: queues E and D tiles, or one ED tile
  (Infinitives gauntlet)" (`Infinitives::effect`).
- **The table shows more of what a word does.** The Gain column shows a queued gem (`+1 💎`), a
  tile upgrade (`+1 ⬆️`), the `!` marks a word clears (`2❗`) and the Bookkeepers bassanello's
  wildcards (`+2 ⬜`). The damage column shows the ice and burn a gem ticks (`30 +1 ❄️`), and bleed
  as its stacks, as the game's preview does (`52 +6 🩸`), with a tooltip saying what bleed does.
- **A cell too narrow for its text ends in `…`**, so a clipped cell can be told from a short one.
  The Damage and Gain columns are left-aligned, so the first item stays in view and only what
  follows it is cut; Damage leads with the hit and the rapier's mark straight after it,
  `21 🤺 +6 🩸`. A lexicon's
  multiplier drops its trailing zeros, `x2` and `x1.5`.
- The Spell column leads with the spell its damage assumes, the one to pick when the game asks
  "Submit as". The effect column says how sure a kill is: `kills - no margin` at exactly lethal,
  `kills - status finishes it` when the hit leaves the enemy alive and its status kills it before
  it acts.
- **The game's own gauntlet, scales and rapier.** The Gain column draws the Infinitives gauntlet's
  and the Scales of justice's item art in place of 🧤 and ⚖️, and the Damage column the rapier's in
  place of 🤺, sized like the emoji beside them. The icons are built into the Advisor, so it needs
  no game checkout to show them. Every gauntlet shows the full gauntlet, a partial one included
  (`InlineIcon`, `assets/advisor/`).
- Corundum is drawn a clear red, Chrysocolla Turquoise and Diamond a very light blue, all with dark
  lettering and without the black ring, which now marks only braced wood and the bleed and toxin
  metals. Inlaid wood is no longer ringed, so a ring on wood always means a metal border, and a
  gold border is drawn in pure gold: the two were hard to tell apart.
- An enemy finished by poison gives no buckler or Well-Rested heal, and the Advisor no longer shows
  one.
- `diggle_advisor --timing-log` writes a timing log, `%LOCALAPPDATA%\DiggleAdvisor\timing.log`, for
  each save from the game's write to the list on screen.

### Combat word selection

Diggle and the Advisor share these: how much damage a word is scored at, and how words are ranked.

#### Damage

- **Words an enemy resists are scored down, not dropped.** A resist used to remove that lexicon's
  words from the search, so against the common human, who takes 0.7x from swears, no swear word
  was ever considered, however much it would heal or armour you. They are now scored the way the
  game scores them: the resist multiplies the word's whole multiplier, gear's share included,
  beside the corner nerf, and an immune lexicon scores zero (`utils/words.lua:219-245`,
  `Modifiers::modifier_for_base`). A zero-damage word can still rank on what it gains.
- **The word's tiles are chosen, not only its letters.** Where two tiles carry the same letter, the
  ranking tries each and keeps the better: a serrated, toxin, gem or braced tile where its position
  pays, the plain twin where it would do nothing (`typist::placements`). Diggle plays the tiles
  that were scored.
- **Spell tiles boost the word.** A Corundum or Chrysocolla first (a Chrysocolla last too), a
  Diamond last, or a Multiocular O makes a plain word a spell of its school, boosted by Adept or
  Versed's +50% (`spells::tile_school`).
- **Ice, burn and bleed count as damage**, like toxin: a Chrysocolla first ticks its ice, a
  Corundum its burn, before the enemy acts. A bleed tile that starts the word (6 bleed per serrated
  tile) or the weapon's bleed costs the enemy its tick (`Modifiers::bleed`).
- **The Totem is modelled** (`wordScoreBonusFullColumn`): half a point of base score per tile in
  every column a word uses all of, on irregular boards at each column's own height
  (`Geometry::full_column_tiles`). On a `hex4` board, `ANHYDRASES` was listed at 28 and dealt 39.
- **A cooper X, Y or Z no longer scores a point the game never gives.** The ligature's +1
  (`scoreAdd`) is dead in the game itself: `tiles.score` shadows the ligature table with the
  tile's ligature name, so it is never added. `YOWIE` on a cooper Y was listed at 16 and a kill;
  the game previews 14.
- **A wildcard's letter now spoils a heterogram, as it does in the game.** The game checks the
  word upper-cased, so `FOOTER` with its second O typed into a wildcard repeats a letter; the
  solver counted the wildcard as its own letter, added the heterogram's +25% and listed
  `FICTIONER` at 63 and a kill where the game shows 52.
- **Words the Advisor called kills fell short.** With `alwaysCastSpell` (the game's default), a word
  with exactly one spell is cast as that spell: a metal word under Book of metal hits only for what
  your missing armour does not absorb, and a lone Four heal or Vowels hits for nothing. Both are now
  scored that way.
- The halved armour cap rounds up, as the game's does; odd maximum health made Metal look a point
  stronger.
- Diggle is scored as casting the best spell, as the Advisor's user is. Pressing the button is not
  written yet, so until it is Diggle can land short where a spell scored higher. The groundwork is
  in: which "Submit as" slot holds the spell, which key casts it, and a check for spells the game
  has that the solver does not model (`src/cast.rs`).
- `score_check <combatSaveData>`: scores every listed word with the game's own Lua (`words.score`,
  the modifier files, the spells) and lists any where the solver promises more damage than the game
  deals. It runs on boards with the Totem.

#### Ranking

- **Kept tiles rank gold first, then gems, then wildcards, all above infinitives.** Of two kills,
  the one that spends less gold wins; then the one that loses fewer gems, each gem counting one
  whatever its letter; then fewer wildcards. So `DENS` beats `AXES`, which spends the gold X for 🧤,
  and `TOWKAY` (a Diamond Y) beats `NOXA` (a Corundum N and the gold X). A Corundum or Chrysocolla,
  and a Diamond while at full health, is spent only when a plainer word would not do the job;
  between equal placements the plain twin is shown.
- **Gems queued by gear count.** The dragon (a fire word queues a Corundum), the walrus (an ice
  word, a Chrysocolla) and the unicorn (a letter four times, a Diamond), staves and trinkets alike,
  rank a word that queues a gem above the infinitives (`pick::GemQueue`). The Bookkeepers
  bassanello's wildcards count too. Both are counted **net of what the word spends**, each against
  its own kind and one tile each: a word that uses up two Corundums to queue one, or two wildcards
  to queue one, is ranked as having lost one (`pick::TileLedger`).
- **Serrated gold is kept like solid gold.** A bleed or toxin gold tile was spent freely: the
  ranking only counted the material named `gold`, so `SEEDBED`, using a bleed gold S and a toxin gold D,
  topped a list of kills that spent no gold.
- **A kill that brings a `!` to the bottom outranks the wood-only and full-heal payouts**, as it
  already outranked armour and health. A board where every plain word earned the Woodsman tunic's
  4 armour listed the words that took the D out from under the `!` at #130; they are now first.
- **A `!` brought partway down ranks above the wildcards a word keeps**, below gold and gems. It
  used to be a board-tidiness tiebreaker, near the very end.
- **Under Express mail, what a word queues ranks first.** Submitting doesn't end the turn, so a
  queued tile can be played the same turn: gems, then wildcards, then infinitives now rank above
  bringing a `!` to the bottom and every payout below it (`Preferences::queue_first`).
- **Burning the enemy counts when it will hit back.** A burning enemy's direct hit is halved, so
  between two non-killing words that hit equally hard, the one that leaves it burning (a Corundum
  first, the Fire spell cast, burn-attack gear) ranks first (`Rank::halves_attack`).
- **Tile upgrades and queued infinitives are the last two gains ranked**: tiles kept, then an ash
  shed, then upgrades, then infinitives, with only the board's letter balance below. Upgrades
  count from any gear that upgrades or gilds tiles on submit: the Scales of justice and the ER/EST
  scales on suffix words, the Gilding bastard sword on seven-letter words. Queued infinitives count
  from every suffix link, so the Ostrich feather, Heart of gold and the lesser scales now count too
  (🔁).
- Without Well-Rested, a hurt player was ranked as unhurt: a Diamond played last was not credited
  with its heal, and Diamonds were kept as if at full health. Missing health is now read from the
  save itself.

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
