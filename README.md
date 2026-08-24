# Diggle Solver

An external Windows program in Rust that plays the LÖVE game *Sternly Worded Adventures* by reading
its window and console and injecting real input. The goal is to clear the anomaly quickly, shrines
included.

**This was vibe-coded.** Every line of it was written by Claude Opus 5 in Claude Code, from a
standing start, inside a single month of a Claude Pro subscription — begun late July 2026 and at the
state you see here on **2026-08-24**. A human directed it, watched the live runs, ruled on the
design questions and caught the wrong answers; a human did not write the code. However, rulings, and
evidence for designs and changes are plastered all over the doc comments.

**Tested against game version v52.4.** That is the only number in this file, because it is the only
one a reader cannot get more reliably by asking the repo.

**The game is never modified.** Everything here observes the running process — screen captures
compared against templates, and the verbose console on stdout — and acts through Win32 `SendInput`.
If the solver disagrees with the game, the solver is wrong.

## The MVP is done

**A run starting from an empty save has cleared the anomaly**, unattended, in one sitting: the menu
offered `Start` rather than `Continue`, so there was nothing to resume, and it went through hero
select, the overworld, fights, a village, a shop, an inn, three shrines and into the level 8
anomaly.

**Video: <https://youtu.be/ylicM0Z9wWw?si=USGZlkHpEANh6poM>**

That is the whole of the claim. It is one run, it was not clean — a stretch in the middle bounced
between two forests for seven crossings before getting on with it — and the failures it did not hit
are still there. Whole mechanics remain deliberately unimplemented and say so: status effects beyond
the tick that decides the current turn, several classes of screen, the wizards' tower. Where a piece
of the model is missing the run is meant to **report it and carry on with an honest
under-estimate**, not to guess; a `not modelled` or `deferred` line in a turn log is the design
working rather than an oversight.

A run that ends early is still an ordinary outcome, and the log it leaves is the deliverable. The
stop reason names the state that had no handler, which is usually the next thing to build.

## Read this before running anything

- **A live run takes the real mouse and keyboard.** `SendInput` follows the foreground window, so
  every click and keystroke it sends goes wherever focus is. Do not plan to use the machine while
  a run is going. Warn anyone else who might be at it.
- **`.diggle-stop` in the repo root ends a run cleanly.** Create the file and the run stops at the
  top of its next iteration. It is the only brake — `config.toml` ships with `run_minutes = 0`,
  meaning no time limit and no step cap. Delete the file before launching, or the next run stops
  instantly.
- **Never touch `%APPDATA%\SternlyWordedAdventures`.** That is the real Steam save. The sandbox
  this project drives is `%APPDATA%\LOVE\SternlyWordedAdventures`, which is where the unfused
  `lovec.exe` writes. The names differ by one directory level and nothing else.
- **Never modify the game checkout** at `../sternly-worded-adventures`. It is read as evidence —
  templates, dictionary, score tables and source citations all come from it — and editing it would
  make every measurement here a measurement of something else.

## Setting up

Windows only for *running*: `src/win/` is Win32, and every template and coordinate is calibrated at
**1920x1080**. A Mac or Linux box can build and run the test suite; it cannot drive the game.

**1. Rust**, via [rustup](https://rustup.rs) or `winget install Rustlang.Rustup`. Rust on Windows
needs the MSVC linker, which rustup does not supply — install Visual Studio Build Tools with
**Desktop development with C++** ticked:

```powershell
winget install Microsoft.VisualStudio.2022.BuildTools
```

**2. The game.** Two separate things are needed:

- **`lovec.exe`**, the *console* build. Not `love.exe`: the driver reads the game's stdout and
  only `lovec` writes any. The process name is `lovec`, which matters when checking whether it is
  running.
- **The unpacked game source**, cloned or copied next to this repo as
  `../sternly-worded-adventures`.

**3. `config.toml`.** Copy `config.example.toml` and set `lovec_path` for your machine. The example
documents the optional keys.

**4. Check it works** — these need no game:

```powershell
cargo test
cargo build --release
```

## The driver

```powershell
# Build first: `cargo test --lib` does NOT build binaries.
cargo build --release --bin spike_run
cargo run --release --bin spike_run
```

A run writes two files. **`spike-run-raw.log`** is the live view, appended as it goes, and is what
to watch while a run is in progress — stdout is buffered until exit, so piping the run through
`tail` discards exactly the log that would explain a failure. **`spike-run.md`** is the report, and
is written *only on a clean exit* (`.diggle-stop` counts as clean); after a crash it still holds the
previous run's report, so check its `start at` line before believing it.

## Save checkpoints

Snapshots of the sandbox profile, so a once-per-island moment can be rehearsed instead of walked to.

```powershell
cargo run --release --bin checkpoint            # `list` is the default
cargo run --release --bin checkpoint -- list    # every checkpoint, and the live save
cargo run --release --bin checkpoint -- save <name>
cargo run --release --bin checkpoint -- restore <name>
cargo run --release --bin checkpoint -- clear   # wipe the profile: run, unlocks, history
```

`restore` and `clear` overwrite the sandbox save and require the game to be **closed**. Both refuse
to operate on any directory but the sandbox.

`clear` is how a from-nothing run is set up — after it the menu offers `Start` rather than
`Continue`. It takes a rescue copy first, always to `checkpoints/before-clear`, and does not ask:
the whole profile is about to go and there is no undo.

## The interactive driver

`diggle` is the dev console. Unlike `spike_run` it is one-shot: the game **stays running** between
commands, so you can launch it once and then poke at it.

```powershell
cargo run --release --bin diggle -- launch
cargo run --release --bin diggle -- shot before-click
cargo run --release --bin diggle -- kill
cargo run --release --bin diggle              # the full list, printed
```

| | |
|---|---|
| **Session** | |
| `launch` | spawn the game detached, record its pid, wait for the window |
| `kill` | terminate it |
| `where` | cursor position, client size, fingerprint hashes |
| **Looking** | |
| `shot <name>` | capture to `spike-frames-live/<name>.{bmp,png}` |
| `hash <x0> <y0> <x1> <y1>` | fingerprint one client rect — *did this region change?* |
| `watch <secs>` | frame-delta trace; read the shape, not the value |
| `find <sprite.png> [step]` | sweep a sprite over the live frame, report where it matches |
| **Acting** | |
| `key <name>` | `return` \| `space` \| `backspace` \| `up` \| `down` \| `left` \| `right` |
| `type <text>` | letters via `WM_CHAR` — what combat tile selection uses |
| `click <x> <y>` | one click in client pixels, with a before/after frame diff |
| `nav <x> <y>` | arrow-navigate toward a point, verified via `GetCursorPos` |
| `hold <dir> <ms>` | hold one arrow (`u`\|`d`\|`l`\|`r`) |
| `walk <dirs>` | a sequence of them, e.g. `walk d,d,r,r,u,l` |
| **Whole steps** | these own the console; see the note below |
| `overworld [secs]` | launch, read the verbose log, parse adjacency dumps, report |
| `travel [key]` | one travel step end to end: read, pan, select, Travel |
| **Instruments** | |
| `probe [hx] [hy] [step]` | find selectable map nodes without recognising anything |
| `pantest [dir] [ms]` | how far one held arrow pans the overworld map |
| `selftest <x> <y> <size>` | crop-and-track against an unchanged frame; must report `1.000` |
| **Offline** | no game needed |
| `save [key]` | dump a top-level key of the sandbox save (default `mainSaveData`) |
| `solve <letters> <health> [armour] [threads]` | the best combat play for a board |
| `findpng <template> <frame.png> [x0 y0 x1 y1]` | score a template against a saved frame |
| `croppng <in> <out> <x0> <y0> <x1> <y1>` | cut a template out of a saved frame |

Three things to know. **`nav` moves the real cursor.** **`escape` is never safe to send**, which is
why there is no command for it: it maps to goBack-or-options and can strand a run. And **`overworld`
and `travel` launch their own game** — LÖVE attaches to its *parent's* console, so whoever reads the
log has to be the process that started it, which makes those two exclusive with the
`launch`/`where`/`shot` workflow.

## Other tools

```powershell
cargo run --release --bin shrine_next     # what to guess next, from colourings already seen
cargo run --release --bin score_compare   # searched vs exact template scoring over the frame corpus
```

## What everything else in `src/bin/` is

The `spike_*` binaries are **probes, not tools**. Each was written to answer one question with a
measurement — how long a click takes to register, whether a ConPTY can be driven, what the console
latency is — and is kept because the answer is cited in a doc comment somewhere and the reader
should be able to re-run the evidence. They are history, not an interface, and most will not do
anything useful today. The same goes for `probe_*`, `crop_template` and `gen_*`, which build
committed data files and only need re-running when their input changes.

If you are looking for the program, it is `spike_run`.

## Roadmap

Buckets rather than a list, because the list churns daily. Roughly in the order they matter.

- **The observation channel.** One defect outranks everything else here: the driver's own log is
  printed into the same console it scrapes, which tears the game's output mid-line and has minted
  at least one phantom location that persists in the map cache. Fixing it is small; everything
  measured off the console is downstream of it.
- **Routing that cannot cycle.** The door ranking and the router price the same graph differently,
  and two vantages can each measure the other as nearer. The memory-based guards that break these
  loops work, but they treat symptoms. One cost model, and a rule that a move must strictly
  improve.
- **Interaction robustness.** Validate before every press and retry with a limit — the project's
  most repeated bug by a distance. Some of this is done per-button and wants making systemic,
  including protection against the game's own hotspot highlight, which changes what a button looks
  like.
- **Game mechanics not yet implemented.** The wizards' tower, treasure chests on arrival, looting
  a sacked village, the beggar's toll, and reading boons back out of the console.
- **Combat depth.** Status effects beyond the current turn's tick, and burning-tile ranking for
  the classes that care about it.
- **Map and camera.** Reconstructing world coordinates well enough to use distance as a heuristic,
  and zooming rather than dragging to fetch an off-screen node.
- **Documentation debt.** Finished designs still live in a scratch ledger and belong in doc
  comments next to the code.

## Where the reasoning lives

In doc comments, deliberately, next to the code they explain — including the arguments that were
tried and rejected, and the live failures that produced each rule. `cargo doc --open` reads well.
Citations of the form `file.lua:123-456` point into `../sternly-worded-adventures` and are meant to
be checked; a claim about the game that cannot cite one is an inference and should say so.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Short version: this is a pet project with high churn, so
pull requests are unlikely to be reviewed promptly — but discussion is very welcome in the *Sternly
Worded Adventures* Discord at <https://discord.gg/tBDWhB7BCm>.
