use serde::Deserialize;
use std::path::PathBuf;

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

/// Resolves [`Config::debug_click_frames`] against the command line, where the flag belongs.
///
/// The config file is the wrong place to reach for something switched on for one run and off again
/// after — it is a file that gets committed, and a `true` left in it slows every fight afterwards
/// with nothing in the log to explain it. So the file holds the default and `--click-frames` /
/// `--no-click-frames` override it for a single invocation.
///
/// **An unrecognised argument is an error, not a shrug.** This flag's entire purpose is to produce
/// photographs, and the way it fails is by producing none — which looks exactly like a run where
/// nothing interesting happened. `--click-frame` or `--clickframes` would do that silently. Refusing
/// costs a restart; accepting costs the run the flag was turned on for.
pub fn click_frames_from_args(args: &[String], default: bool) -> Result<bool, String> {
    let mut on = default;
    for a in args {
        match a.as_str() {
            "--click-frames" => on = true,
            "--no-click-frames" => on = false,
            other => {
                return Err(format!(
                    "unrecognised argument {other:?}; expected --click-frames or --no-click-frames"
                ))
            }
        }
    }
    Ok(on)
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
        assert!(click_frames_from_args(&args(&["--click-frames"]), false).unwrap());
        assert!(!click_frames_from_args(&args(&["--no-click-frames"]), true).unwrap());
    }

    #[test]
    fn no_argument_leaves_the_file_in_charge() {
        assert!(!click_frames_from_args(&[], false).unwrap());
        assert!(click_frames_from_args(&[], true).unwrap());
    }

    #[test]
    fn a_misspelled_flag_is_refused_rather_than_ignored() {
        // The failure mode this guards against is silent: the flag exists to produce photographs,
        // and a typo produces none -- which is indistinguishable from a run where nothing happened.
        // A live run is expensive enough that finding out afterwards is the wrong time.
        let e = click_frames_from_args(&args(&["--click-frame"]), false).unwrap_err();
        assert!(e.contains("--click-frames"), "the error must say the spelling it wanted: {e}");
    }

    #[test]
    fn the_last_flag_wins_so_a_shell_alias_can_be_overridden() {
        assert!(!click_frames_from_args(&args(&["--click-frames", "--no-click-frames"]), false)
            .unwrap());
    }

    #[test]
    fn the_flag_can_be_turned_on_from_the_file() {
        let cfg: Config = toml::from_str(
            "game_dir = \"g\"\nlovec_path = \"l\"\ndebug_click_frames = true\n",
        )
        .expect("parses");
        assert!(cfg.debug_click_frames);
    }
}
