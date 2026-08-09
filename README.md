# Diggle Solver

An external Windows program in Rust that plays the LÖVE game *Sternly Worded Adventures* by reading
its window and console and injecting real input. The goal is to clear the anomaly quickly, shrines
included.

**Tested against game version v52.4.** That is the only number in this file, because it is the only
one a reader cannot get more reliably by asking the repo.

**The game is never modified.** Everything here observes the running process — screen captures
compared against templates, and the verbose console on stdout — and acts through Win32 `SendInput`.
If the solver disagrees with the game, the solver is wrong.

## Read this before running anything

- **A live run takes the real mouse and keyboard.** `SendInput` follows the foreground window, so
  every click and keystroke it sends goes wherever focus is. Do not plan to use the machine while a
  run is going. Warn anyone else who might be at it.
- **`.diggle-stop` in the repo root ends a run cleanly.** Create the file and the run stops at the
  top of its next iteration. It is the only brake — `config.toml` ships with `run_minutes = 0`,
  meaning no time limit and no step cap. Delete the file before launching, or the next run stops
  instantly.
- **Never touch `%APPDATA%\SternlyWordedAdventures`.** That is the real Steam save. The sandbox this
  project drives is `%APPDATA%\LOVE\SternlyWordedAdventures`, which is where the unfused `lovec.exe`
  writes. The names differ by one directory level and nothing else.
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

- **`lovec.exe`**, the *console* build. Not `love.exe`: the driver reads the game's stdout and only
  `lovec` writes any. The process name is `lovec`, which matters when checking whether it is running.
- **The unpacked game source**, cloned or copied next to this repo as `../sternly-worded-adventures`.

**3. `config.toml`.** Copy `config.example.toml` and set `lovec_path` for your machine. The example
documents the optional keys.

**4. Check it works** — these need no game:

```powershell
cargo test
cargo build --release
```

## The commands worth knowing

Most of what is in `src/bin/` is not part of the interface — see below. These are:

```powershell
# The driver. Build first: `cargo test --lib` does NOT build binaries.
cargo build --release --bin spike_run
cargo run --release --bin spike_run
```

A run writes two files. **`spike-run-raw.log`** is the live view, appended as it goes, and is what to
watch while a run is in progress — stdout is buffered until exit, so piping the run through `tail`
discards exactly the log that would explain a failure. **`spike-run.md`** is the report, and is
written *only on a clean exit* (`.diggle-stop` counts as clean); after a crash it still holds the
previous run's report, so check its `start at` line before believing it.

```powershell
# Sandbox save checkpoints, so a once-per-island moment can be rehearsed.
cargo run --release --bin checkpoint -- list
cargo run --release --bin checkpoint -- save pre-anomaly
cargo run --release --bin checkpoint -- restore pre-anomaly
```

`restore` overwrites the sandbox save and requires the game to be **closed**. It refuses to operate
on any directory but the sandbox.

```powershell
# Interactive dev driver — the game STAYS RUNNING between commands.
cargo run --release --bin diggle -- launch
cargo run --release --bin diggle -- shot before-click
cargo run --release --bin diggle -- kill
```

Run it with no arguments for the full list. `diggle nav` moves the real cursor; `escape` is never
safe to send, because it maps to goBack-or-options and can strand a run.

```powershell
# Offline, no game needed.
cargo run --release --bin shrine_next     # what to guess next, from colourings already seen
cargo run --release --bin score_compare   # searched vs exact template scoring over the frame corpus
```

## What everything else in `src/bin/` is

The `spike_*` binaries are **probes, not tools**. Each was written to answer one question with a
measurement — how long a click takes to register, whether a ConPTY can be driven, what the console
latency is — and is kept because the answer is cited in a doc comment somewhere and the reader should
be able to re-run the evidence. They are history, not an interface, and most will not do anything
useful today. The same goes for `probe_*`, `crop_template` and `gen_*`, which build committed data
files and only need re-running when their input changes.

If you are looking for the program, it is `spike_run`.

## Where the reasoning lives

In doc comments, deliberately, next to the code they explain — including the arguments that were
tried and rejected, and the live failures that produced each rule. `cargo doc --open` reads well.
Citations of the form `file.lua:123-456` point into `../sternly-worded-adventures` and are meant to
be checked; a claim about the game that cannot cite one is an inference and should say so.
