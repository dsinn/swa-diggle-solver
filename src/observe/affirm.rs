//! Reading a button's *state* off the screen, instead of inferring it from the console.
//!
//! Everything that advances a screen in this game is a button, and every button is drawn from one
//! of five images that the game ships as separate files. `buttonType` (`ui/elements/button.lua:1-15`)
//! names them:
//!
//! ```lua
//! up = imageCache('ui/graphics/button/'..img..'-up.'..ext),
//! hover, down, down_hover, inactive, shadow, flat
//! ```
//!
//! So `right-up.png` and `right-inactive.png` are distinct artwork on disk, and matching a capture
//! against them answers the only question that matters before sending input: **can we progress right
//! now?** Not "did the game print that a screen exists" — that is `onActive`, which fires at the
//! *start* of a transition and has misled this project repeatedly — but "is the affirmative control
//! painted, and painted live rather than greyed".
//!
//! ## Why this replaces counting console lines
//!
//! `ui/lorescreen.lua:22` prints `Lore screen: <text>` from `onActive`. Counting those lines answers
//! "how many lore screens were ever constructed", which is not "is one up now" and not "will it take
//! a keypress now". It is also open-loop: our tally and the game's reality can only drift apart,
//! because nothing in that design ever looks at the screen. A template read is closed-loop — the same
//! function that decides to press also confirms the press landed, by the control no longer being
//! there.
//!
//! ## Why the game's own art, and not a golden screenshot
//!
//! A screenshot would bake in whatever happened to be behind the button. The lore screen draws over
//! `bgblurry` of the location parallax (`ui/lorescreen.lua:24-32`), so the backdrop differs by where
//! the player is standing. The shipped PNG has alpha, and [`super::template`] scores only pixels the
//! template actually paints, so the backdrop is excluded by construction rather than by luck.

use crate::win::capture::Frame;
use crate::win::window::{button_center, ButtonSpec};
use std::path::{Path, PathBuf};

use super::template::{find_at_scale_in, Template};

/// What the affirmative slot is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Painted and live. This is the only state that may be pressed.
    Up,
    /// Live, with the cursor over it. Also pressable — the hotspot navigator parks the cursor on
    /// whatever it last snapped to, so this is common rather than exceptional.
    Hover,
    /// Mid-press. Seen when a capture lands inside the 10-frame `down_fade`
    /// (`ui/elements/button.lua:30`); means an input is already in flight.
    Down,
    /// Painted but refusing input. The distinction `Absent` cannot make, and the reason this reads
    /// artwork rather than merely asking "is anything drawn here".
    Inactive,
    /// Nothing recognisable in the slot: no such button on this screen.
    Absent,
}

impl State {
    /// May we send the affirmative now?
    pub fn is_ready(self) -> bool {
        matches!(self, State::Up | State::Hover)
    }
}

/// The five state images for one button type, loaded from the game's own directory.
#[derive(Debug, Clone)]
pub struct ButtonArt {
    pub kind: String,
    up: Template,
    hover: Template,
    down: Template,
    inactive: Template,
}

/// The lore screen's continue button when it carries no label.
///
/// `ui/lorescreen.lua:46-50`:
///
/// ```lua
/// require'ui.elements.button'(extra.buttonText or '', 1, extra.buttonText and 0.75 or 0.9, {
///     type = extra.buttonText and 'default' or 'right',
///     xOffset = -0.75,
///     userFunctionName = 'affirmative',
/// })
/// ```
///
/// `right` is 64x80 (`ui/elements/button.lua:22`), giving centre (1872, 972) at 1920x1080 — the
/// bottom-right corner.
pub const LORE_AFFIRMATIVE: ButtonSpec =
    ButtonSpec { ss_x: 1.0, ss_y: 0.9, os_x: -0.75, os_y: 0.0, w: 64.0, h: 80.0 };

/// The overworld's "show functions for current location and centre the screen" button.
///
/// **Not the world-map overlay.** That is a different control — `overworld.lua:1427-1438`, a `small`
/// button at `ss(1, 0)` carrying `icon = map.png`, whose handler is `setActiveMode(waypointselect)`.
/// Opening an overlay of the *parent* world would indeed be no use for navigating a subworld. This
/// button changes no mode at all: it calls `refreshAreaButtons` and `centreScreenOnPlayer()`, and
/// `playerAbstract` is set from `locationData[playerLocation]`, which inside a subworld is the
/// subworld's own node table. So it centres within the village.
///
/// `overworldview.lua:483-494`:
///
/// ```lua
/// local showAreaButtonsButton = require'ui.elements.button'('', 0, 0.85, {
///     type = 'right',
///     tooltip = 'Show functions for current location and centre the screen.',
///     xOffset = 0.5,
/// ```
///
/// Same `right` artwork as [`LORE_AFFIRMATIVE`], so one [`ButtonArt`] reads both — bottom-left
/// rather than bottom-right, giving centre (32, 918) at 1920x1080.
///
/// ## Why reading it matters rather than just clicking where it lives
///
/// It is **not always on screen**. `(0, 0.85)` is also where `Combat`, `Attack`, `Enter`, `Gather`,
/// `Rest`, `Open` and `Wake up` are built across the generators, and on a fresh load into a subworld
/// one of those occupies the slot. Clicking blind there presses whichever is present — on a combat
/// node, starting a fight we did not choose. Those are labelled `default` buttons and this is a
/// `right` arrow, so the fingerprint separates them; the position alone does not.
///
/// Verified from a live capture taken inside a corrupted village: after one click on empty map,
/// `right-up.png` matched at (0, 878) — centre (32, 918) — with inliers 1.0000 and error 0.0003,
/// against 0.026 for the same template elsewhere on the same frame.
///
/// It is restored by clicking **empty map space**: `core:mousereleased` falls through to
/// `overworld.clearAreaButtons(); overworld.insertObject(showAreaButtonsButton)` when the release
/// was over no location (`:1479-1485`). That is the whole recovery, and it works inside a subworld
/// as well as out — the branch tests only whether a location was under the cursor.
pub const SHOW_AREA_BUTTONS: ButtonSpec =
    ButtonSpec { ss_x: 0.0, ss_y: 0.85, os_x: 0.5, os_y: 0.0, w: 64.0, h: 80.0 };

/// Slack, in unscaled pixels, allowed around the computed top-left when searching.
///
/// Not zero, because `button_center` rounds to whole pixels and the art is drawn with a shadow
/// (`right-shadow.png`) that can bias the visual centre. Small, because the search is exhaustive at
/// `step = 1` — pixel art misaligns badly at even one pixel off, so widening this costs accuracy as
/// well as time.
const SEARCH_SLACK: i32 = 6;

/// Inlier fraction below which a state is not considered present.
///
/// Applied to the *best* of the five states: the states differ from each other far less than any of
/// them differs from arbitrary art, so the threshold's job is presence and the argmax's job is which.
///
/// ## 0.80, raised from 0.55 after a live stall
///
/// The old value was chosen as "well under a clean match but well over an unrelated background". The
/// second half of that was never measured against the background that matters. The **empty overworld
/// slot** — the ordinary state of the screen this program spends most of its time on — scores 0.62,
/// comfortably over 0.55, so the map reads as carrying a live affirmative.
///
/// What that cost, from one run's log:
///
/// ```text
///   genuine affirmative, a real text screen   1.00   margin 0.11
///   the empty map slot                        0.62   margin 0.07   <- read as `Up`
///   other Absent readings on the map          0.27 - 0.43
/// ```
///
/// The run reached the overworld after a fight, read the phantom, pressed `Space` six times, gave up,
/// and did it again every iteration until the step budget ran out.
///
/// **The wasted presses were not the worst of it.** `Space` on the overworld is the affirmative, and
/// it can activate the selected location's button. It was harmless only because the node underfoot
/// was a cleared crypt whose `Combat` button was greyed out; on an uncleared one it would have
/// started a fight nothing had chosen. This project's rule is that `Space` is never sent to a screen
/// whose affirmative has not genuinely been read — and a 0.62 reading against a 0.55 bar is not
/// genuinely reading it.
///
/// 0.80 sits in the measured gap: 0.20 below the genuine reading and 0.18 above the phantom.
///
/// **The genuine sample is small** — two readings, both 1.00, from one run. It is placed nearer the
/// impostor than the midpoint for that reason, so a partially drawn affirmative has room to be
/// recognised. If a real text screen is ever missed at this bar, its score is in the log and the fix
/// is a measurement rather than a guess.
const PRESENT: f64 = 0.80;

/// How far the winner must beat the runner-up for the state to be called with confidence.
///
/// The five images are the same silhouette in different shades, so a mid-fade capture legitimately
/// sits between two of them. Below this margin the call is a coin flip, and callers that care should
/// settle and re-read rather than act on it.
pub const DISTINCT: f64 = 0.05;

/// One read of a button slot.
#[derive(Debug, Clone, Copy)]
pub struct Reading {
    pub state: State,
    /// Inlier fraction of the winning template.
    pub score: f64,
    /// Winner minus runner-up. Compare against [`DISTINCT`].
    pub margin: f64,
}

impl ButtonArt {
    /// Loads one button type's artwork from an unpacked game directory.
    ///
    /// `ext` follows `buttonType`'s own rule: the arrow and tab types ship as `png`, the rest as
    /// `jpg` (`ui/elements/button.lua:16-28`). Only the alpha-bearing PNG types are useful here — a
    /// JPEG button is a solid rectangle, so its "opaque pixels" are the whole box including the
    /// backdrop it was authored against.
    pub fn load(game_dir: &Path, kind: &str) -> Result<ButtonArt, Box<dyn std::error::Error>> {
        let dir: PathBuf = game_dir.join("ui").join("graphics").join("button");
        let one = |suffix: &str| -> Result<Template, Box<dyn std::error::Error>> {
            Template::load(&dir.join(format!("{kind}-{suffix}.png")))
        };
        Ok(ButtonArt {
            kind: kind.to_string(),
            up: one("up")?,
            hover: one("up-hover")?,
            down: one("down")?,
            inactive: one("inactive")?,
        })
    }

    /// Reads the slot `spec` names in `frame`.
    ///
    /// The search is bounded to the slot rather than the screen. That is not only a speed choice: an
    /// unbounded search would happily find the same arrow somewhere else on a screen that has
    /// several, and report a control we are not about to press.
    /// Reads the slot from a capture of just that slot.
    ///
    /// The button never moves, so grabbing the whole window to look at a 64x80 corner is around 300
    /// times the pixels for the same answer — and this runs in a poll loop while waiting for text
    /// screens. `client` is the full client size, which the slot's position is derived from;
    /// `origin` is where the crop was taken.
    pub fn read_cropped(
        &self, frame: &Frame, spec: &ButtonSpec, client: (i32, i32), origin: (i32, i32),
    ) -> Reading {
        self.read_at(frame, spec, client, origin)
    }

    /// The slot's crop rectangle for a given client size: `(x, y, w, h)`, with slack for the search.
    pub fn crop_rect(spec: &ButtonSpec, client_w: i32, client_h: i32) -> (i32, i32, i32, i32) {
        let s = crate::layout::scale(client_w, client_h);
        let (cx, cy) = button_center(spec, client_w, client_h);
        let (tw, th) = ((spec.w * s).round() as i32, (spec.h * s).round() as i32);
        let pad = SEARCH_SLACK + 2;
        let x = (cx - tw / 2 - pad).max(0);
        let y = (cy - th / 2 - pad).max(0);
        let w = (tw + pad * 2).min(client_w - x);
        let h = (th + pad * 2).min(client_h - y);
        (x, y, w, h)
    }

    pub fn read(&self, frame: &Frame, spec: &ButtonSpec) -> Reading {
        let (w, h) = (frame.width, frame.height);
        self.read_at(frame, spec, (w, h), (0, 0))
    }

    fn read_at(
        &self, frame: &Frame, spec: &ButtonSpec, client: (i32, i32), origin: (i32, i32),
    ) -> Reading {
        let (w, h) = client;
        let s = crate::layout::scale(w, h);
        let (cx, cy) = button_center(spec, w, h);
        let (tw, th) = ((spec.w * s).round() as i32, (spec.h * s).round() as i32);
        let (x0, y0) = (cx - tw / 2 - origin.0, cy - th / 2 - origin.1);
        let bounds = Some((
            x0 - SEARCH_SLACK,
            y0 - SEARCH_SLACK,
            x0 + SEARCH_SLACK,
            y0 + SEARCH_SLACK,
        ));

        let mut scored: Vec<(State, f64)> = Vec::with_capacity(4);
        for (state, tpl) in [
            (State::Up, &self.up),
            (State::Hover, &self.hover),
            (State::Down, &self.down),
            (State::Inactive, &self.inactive),
        ] {
            if let Some(m) = find_at_scale_in(frame, tpl, s, 1, bounds) {
                scored.push((state, m.inliers));
            }
        }
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        match scored.first() {
            Some(&(state, score)) if score >= PRESENT => {
                let runner_up = scored.get(1).map(|r| r.1).unwrap_or(0.0);
                Reading { state, score, margin: score - runner_up }
            }
            Some(&(_, score)) => Reading { state: State::Absent, score, margin: 0.0 },
            None => Reading { state: State::Absent, score: 0.0, margin: 0.0 },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The coordinate the whole gate depends on. Derived, not measured: if `ss_y` or the 64x80 size
    /// is ever misread, the search window sits over empty screen and every read says `Absent` —
    /// which is indistinguishable from "no button" and would send us back to guessing.
    #[test]
    fn the_lore_affirmative_sits_in_the_bottom_right_corner() {
        assert_eq!(button_center(&LORE_AFFIRMATIVE, 1920, 1080), (1872, 972));
        // Scales with the window like every other button.
        assert_eq!(button_center(&LORE_AFFIRMATIVE, 960, 540), (936, 486));
    }

    /// The bottom-left twin. Shares its slot with the area buttons, so this coordinate is only safe
    /// to click once the slot has been *read* as showing the arrow.
    #[test]
    fn show_area_buttons_sits_in_the_bottom_left_corner() {
        assert_eq!(button_center(&SHOW_AREA_BUTTONS, 1920, 1080), (32, 918));
        assert_eq!(button_center(&SHOW_AREA_BUTTONS, 960, 540), (16, 459));
    }

    #[test]
    fn only_live_states_may_be_pressed() {
        assert!(State::Up.is_ready());
        assert!(State::Hover.is_ready());
        // The two that have burned us: a greyed button and an empty slot both look like "nothing is
        // happening" to a console-line counter.
        assert!(!State::Inactive.is_ready());
        assert!(!State::Absent.is_ready());
        // Already pressed: pressing again is the double-click that cost 45 s on `Finish`.
        assert!(!State::Down.is_ready());
    }

    /// Skipped rather than failed when the game is not unpacked beside us, so the suite still runs
    /// on a machine without it — but loud about what it did not check.
    #[test]
    fn the_arrow_artwork_loads_from_the_game() {
        let game = Path::new("../sternly-worded-adventures");
        if !game.join("ui/graphics/button/right-up.png").exists() {
            eprintln!("skipped: no unpacked game at {}", game.display());
            return;
        }
        let art = ButtonArt::load(game, "right").expect("right-* artwork");
        assert_eq!((art.up.width, art.up.height), (64, 80));
        // Alpha is the whole reason this beats a screenshot: a fully opaque template would score the
        // backdrop along with the button.
        assert!(art.up.opaque_fraction(super::super::template::ALPHA_MIN) < 1.0);
    }
}
