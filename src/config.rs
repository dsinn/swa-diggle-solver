use serde::Deserialize;
use std::path::PathBuf;

/// The file that ends a run early, in the working directory.
///
/// The dev's abort, and the only one that works while a run holds the mouse and keyboard: a live run
/// takes both, so the alternative to a file on disk is fighting the run for its own input devices.
pub const STOP_FILE: &str = ".diggle-stop";

/// Whether someone has asked the run to stop — **without consuming the request**.
///
/// Lives here rather than beside the driver so that the long subsystems can consult it without
/// depending on the navigator. Consuming it (deleting the file, ending the run) stays in exactly one
/// place, `navigate::drive`'s top-of-loop, so two readers cannot each swallow half a request.
///
/// ## Why the subsystems have to ask at all
///
/// It used to be checked once per navigator step, and a single step can hold the mouse for minutes:
/// a fight is given `deadline.min(now + 400s)` at one of its three call sites, and an inn will press
/// `Rest` up to `MAX_PRESSES` times at up to `REST_TRIES * REST_WAIT` each. So the request was
/// honoured *eventually* — after up to about seven minutes of a run the dev had already asked to
/// stop, still holding their input devices. That is not what an abort is for.
///
/// Stopping inside a fight is safe and recoverable: `combatSaveData` persists, and a later run
/// rejoins a fight in progress — 1519Z opened on exactly that (`0. resuming a fight already in
/// progress`).
///
/// **Still checked only per step:** the shrine word screen, whose own bound is `max_guesses`
/// attempts at `BEAM_RETURN` (12 s) each. Shorter than either of the above, and interrupting a word
/// mid-guess risks reading as the shrine *failing*, which ends the run for a different reason
/// (#55). Left alone deliberately.
pub fn stop_requested() -> bool {
    stop_requested_in(std::path::Path::new("."))
}

/// [`stop_requested`] against a named directory, so the primitive can be tested without creating
/// the real abort file in the repo root.
///
/// That is not fussiness: a test that writes `.diggle-stop` beside a live run would end it, and a
/// test that deletes one would swallow an abort the dev had just asked for. The one-argument form
/// is the contract everything uses; this exists for the test.
pub fn stop_requested_in(dir: &std::path::Path) -> bool {
    dir.join(STOP_FILE).exists()
}

/// `Default` is derived deliberately: later tasks add fields, and test helpers
/// construct Config with `..Default::default()` so they don't break each time.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    /// Directory containing the game's main.lua (e.g. ../sternly-worded-adventures)
    pub game_dir: PathBuf,
    /// Path to lovec.exe. MUST be the console build; love.exe will not write to our pipe.
    pub lovec_path: PathBuf,
    /// Optional path to mirror the raw verbose log to, for post-hoc inspection.
    #[serde(default)]
    pub log_mirror: Option<PathBuf>,
    /// Explicit save directory. Leave unset to derive it from how we launch:
    /// unfused (`lovec.exe <dir>`) writes to %APPDATA%\LOVE\SternlyWordedAdventures,
    /// while the fused Steam build writes to %APPDATA%\SternlyWordedAdventures.
    #[serde(default)]
    pub save_dir: Option<PathBuf>,
    /// How long a run may hold the mouse and keyboard, in minutes. `0` means no limit.
    ///
    /// The step cap this replaces was a guess about how long a run *ought* to take, and it kept
    /// cutting runs off mid-discovery. Time is the honest bound for a program driving real input:
    /// it maps to the thing actually being spent, and it does not shorten a run that is making
    /// progress the way a step count does.
    ///
    /// Here rather than in the code so that changing it does not need a rebuild — the number is a
    /// judgement about how long you are prepared to be away from the machine, which is yours and
    /// not the program's. `.diggle-stop` in the working directory ends a run early from any state.
    #[serde(default)]
    pub run_minutes: Option<u64>,
    /// Photograph the whole window either side of every tile click. **Off unless asked for.**
    ///
    /// The stray-selection check reports which tile centres changed luminance, and when it fires on
    /// a board nobody was watching there is no way to ask *how* they changed — glow, dimming, an
    /// overlay, a click that never landed at all. A frame from each side of the click answers that,
    /// and it is the one question the log cannot.
    ///
    /// Not on by default because it is a full-window `PrintWindow` per click — ~28 ms measured
    /// (`win::capture`) against the 4.4 ms cheap path the click loop is built around — and a word is
    /// ten or more clicks. It slows every turn and fills the frames directory. Turn it on for the
    /// run that is meant to answer a question, then turn it off.
    #[serde(default)]
    pub debug_click_frames: bool,
}

/// Used when `run_minutes` is absent. Long enough for a full run with rest detours — the run of
/// 2026-08-09 got through 45 steps, two forests, three villages and a mausoleum in under fifteen.
pub const DEFAULT_RUN_MINUTES: u64 = 60;

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self, crate::Error> {
        let text = std::fs::read_to_string(path)?;
        toml::from_str(&text).map_err(|e| crate::Error::Config(e.to_string()))
    }
}

/// How long a run waits, after the game's window exists, before it touches anything.
///
/// The window has to be up first — that is what a screen recorder has to see before it will attach
/// to it — so this is measured from `wait_for_window` and not from launch.
pub const DEFAULT_HOLD_OFF: std::time::Duration = std::time::Duration::from_secs(3);

/// Everything the run takes from the command line, parsed in one pass.
#[derive(Debug, Clone, PartialEq)]
pub struct RunArgs {
    /// Resolved [`Config::debug_click_frames`].
    pub click_frames: bool,
    /// How long to wait after the window appears before seizing the mouse and keyboard.
    pub hold_off: std::time::Duration,
}

/// Reads the command line, and refuses anything it does not recognise.
///
/// ## Why the flags are here rather than in `config.toml`
///
/// The config file is the wrong place for something switched on for one run and off again after —
/// it gets committed, and a `debug_click_frames = true` left in it slows every fight afterwards with
/// nothing in the log to explain it. So the file holds the default and `--click-frames` /
/// `--no-click-frames` override it for a single invocation.
///
/// ## `--delay <seconds>`
///
/// The dev, 2026-08-22: *what command do I use to delay Diggle taking control of the mouse and
/// keyboard? I want to run OBS Studio but it takes a few seconds for it to recognize a newly
/// launched app.* The run launches the game itself, so there is no moment beforehand at which a
/// recorder could be pointed at a window that does not exist yet — the only usable gap is between
/// the window appearing and the first click, which is what this sets. [`DEFAULT_HOLD_OFF`] is the
/// three seconds that were hard-coded there before, so an invocation without the flag is unchanged.
///
/// ## An unrecognised argument is an error, not a shrug
///
/// `--click-frames` exists to produce photographs, and the way it fails is by producing none —
/// which looks exactly like a run where nothing interesting happened. `--click-frame` or
/// `--clickframes` would do that silently. The same is true of `--delay`, whose failure is a run
/// that grabs the mouse while the recorder is still starting, which cannot be undone once a live
/// run is moving. Refusing costs a restart; accepting costs the run the flag was set for.
///
/// **One parser, one pass**, because two parsers walking the same argument list is a way for them to
/// disagree about what is valid — and this project has written that bug down more than once.
pub fn run_args(args: &[String], click_default: bool) -> Result<RunArgs, String> {
    let mut out = RunArgs { click_frames: click_default, hold_off: DEFAULT_HOLD_OFF };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--click-frames" => out.click_frames = true,
            "--no-click-frames" => out.click_frames = false,
            "--delay" => {
                let Some(v) = it.next() else {
                    return Err("--delay wants a number of seconds after it".to_string());
                };
                // Whole seconds. A recorder is being waited on, and nobody needs a third of one.
                let secs: u64 = v
                    .parse()
                    .map_err(|_| format!("--delay wants a number of seconds, not {v:?}"))?;
                // Capped, because a mistyped delay is silent in exactly the way the flags above are:
                // the run sits there looking launched. Ten minutes is far past any recorder.
                if secs > 600 {
                    return Err(format!("--delay {secs} is longer than ten minutes; that is a typo"));
                }
                out.hold_off = std::time::Duration::from_secs(secs);
            }
            other => {
                return Err(format!(
                    "unrecognised argument {other:?}; expected --click-frames, --no-click-frames \
                     or --delay <seconds>"
                ))
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_without_the_debug_flag_leaves_click_photography_off() {
        // The defaulted field must not change what an existing config.toml does. Photographing
        // every click costs a full-window capture per click; turning that on by accident would slow
        // every fight and nothing in the log would say why.
        let cfg: Config = toml::from_str(
            "game_dir = \"g\"\nlovec_path = \"l\"\nrun_minutes = 0\n",
        )
        .expect("a config without the flag still parses");
        assert!(!cfg.debug_click_frames);
    }

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_command_line_overrides_the_file_in_both_directions() {
        assert!(run_args(&args(&["--click-frames"]), false).unwrap().click_frames);
        assert!(!run_args(&args(&["--no-click-frames"]), true).unwrap().click_frames);
    }

    #[test]
    fn no_argument_leaves_the_file_in_charge() {
        assert!(!run_args(&[], false).unwrap().click_frames);
        assert!(run_args(&[], true).unwrap().click_frames);
    }

    /// The gap a screen recorder needs, and the three ways of asking for it wrongly.
    ///
    /// Every failure here is silent in the same way the click-frame flags are: a run that seizes the
    /// mouse early cannot be called back, and a run that waits ten minutes looks like one that hung.
    #[test]
    fn the_hold_off_is_read_from_the_command_line_and_defaults_to_what_was_hard_coded() {
        assert_eq!(run_args(&[], false).unwrap().hold_off, DEFAULT_HOLD_OFF);
        assert_eq!(
            run_args(&args(&["--delay", "15"]), false).unwrap().hold_off,
            std::time::Duration::from_secs(15)
        );
        // Zero is a real answer: it is what someone who is not recording wants.
        assert_eq!(
            run_args(&args(&["--delay", "0"]), false).unwrap().hold_off,
            std::time::Duration::from_secs(0)
        );
        // And it composes with the flag it shares a parser with.
        let both = run_args(&args(&["--delay", "10", "--click-frames"]), false).unwrap();
        assert_eq!(both.hold_off, std::time::Duration::from_secs(10));
        assert!(both.click_frames);

        assert!(run_args(&args(&["--delay"]), false).unwrap_err().contains("seconds"));
        assert!(run_args(&args(&["--delay", "soon"]), false).unwrap_err().contains("soon"));
        assert!(run_args(&args(&["--delay", "6000"]), false).unwrap_err().contains("typo"));
    }

    #[test]
    fn a_misspelled_flag_is_refused_rather_than_ignored() {
        // The failure mode this guards against is silent: the flag exists to produce photographs,
        // and a typo produces none -- which is indistinguishable from a run where nothing happened.
        // A live run is expensive enough that finding out afterwards is the wrong time.
        let e = run_args(&args(&["--click-frame"]), false).unwrap_err();
        assert!(e.contains("--click-frames"), "the error must say the spelling it wanted: {e}");
    }

    #[test]
    fn the_last_flag_wins_so_a_shell_alias_can_be_overridden() {
        assert!(!run_args(&args(&["--click-frames", "--no-click-frames"]), false)
            .unwrap()
            .click_frames);
    }

    #[test]
    fn the_flag_can_be_turned_on_from_the_file() {
        let cfg: Config = toml::from_str(
            "game_dir = \"g\"\nlovec_path = \"l\"\ndebug_click_frames = true\n",
        )
        .expect("parses");
        assert!(cfg.debug_click_frames);
    }

    /// The abort's whole contract: a file named exactly this, in the directory asked about.
    #[test]
    fn the_stop_file_is_seen_when_it_is_there_and_not_when_it_is_not() {
        let dir = std::env::temp_dir().join(format!("diggle-stop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        // The negative first, so a directory that always answers `true` cannot pass this.
        assert!(!stop_requested_in(&dir), "nothing has been asked for yet");

        std::fs::write(dir.join(STOP_FILE), b"").expect("temp file");
        assert!(stop_requested_in(&dir), "the dev's abort must be seen");

        // **Reading must not consume.** Two subsystems and the driver all ask; if any of them
        // cleared it, the others would never see the request and the run would carry on.
        assert!(stop_requested_in(&dir), "asking twice must give the same answer");

        // A near-miss must not fire. The name is the whole interface and a typo is silent.
        std::fs::remove_file(dir.join(STOP_FILE)).expect("remove");
        std::fs::write(dir.join("diggle-stop"), b"").expect("temp file");
        assert!(!stop_requested_in(&dir), "the leading dot is part of the name");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The name is documented in `SETUP.md` and printed by the run's own header, so it is an
    /// interface rather than an implementation detail.
    #[test]
    fn the_stop_file_is_named_what_the_docs_say() {
        assert_eq!(STOP_FILE, ".diggle-stop");
    }
}
