//! Reading a shrine's word grid off the screen, so [`crate::shrine`] can actually play one.
//!
//! The solver has always been the easy half. It was built, tested against 71,268 puzzles, and could
//! not be used, because nothing could tell it what the game had just shown. This is that half: where
//! the tiles are, and what colour each one is.
//!
//! ## Why the grid can be computed rather than found
//!
//! `shrineview.lua:39-44` derives the whole layout from one number, the length of the word:
//!
//! ```lua
//! local maxGuesses = math.clamp(math.ceil(30/wordLength), 6, 10)
//! local scale      = math.min(1, 6/maxGuesses)
//! local letterSize = math.floor(118*scale+0.5)
//! local width      = wordLength*letterSize
//! local height     = maxGuesses*letterSize
//! ```
//!
//! and `buildDrawDataTable(width, height, 0.5, 0.35)` (`shrineview.lua:146`) centres that block at
//! (0.5, 0.35) of the client — with `x2`/`y2` defaulting to 0, the transform's origin is
//! `(+width/2, +height/2)`, so the anchor is the block's **centre**, not its corner (`main.lua:260-268`).
//!
//! So there is nothing to search for. Given the word length, every tile rect is arithmetic.
//! Confirmed live at 1920x1080 on a four-letter shrine: predicted top-left (782, 22) with 89 px
//! tiles, and all four sampled tile centres landed square on their tiles.
//!
//! ## The colours, and the trap underneath them
//!
//! `shrineview.lua:163-178` tints the marble tile itself — `setColor(green)` then `print(letter,
//! tileFont, ...)` — and draws the letter white on top only for already-submitted rows (`:199`). So
//! the tile *background* carries the answer and the glyph is an occlusion, which is why every sample
//! here is a **mean over the tile's middle** rather than a single pixel.
//!
//! The trap: an *empty* tile is drawn at `alpha 0.25` (`:159-160`), so it is mostly whatever scene is
//! behind it. On a grassy shrine the bottom rows read `130 155 82` — green-ish, with G > R > B. Take
//! a fixed distance to a green reference and those tiles are closer to "green" than to anything else
//! on offer. This is the project's standing lesson about calibrating against the *confusable* state
//! rather than against background, met again: plain grass is the confusable here, not black.
//!
//! Two things defuse it, and both are needed:
//!
//! 1. **Classify by channel relationships, not by distance to a reference.** How much of the tile the
//!    letter covers changes the mean a lot — a live `i` on yellow read `175 143 16` where an `a` on
//!    yellow read `146 124 34`, which is 39 units apart and would have broken a distance cut placed
//!    at 40. The *relations* (R high, G high, B low) survive that; the absolute values do not.
//! 2. **A brightness gate.** Submitted tiles are dark (max channel <= 83 measured); empty ones are
//!    bright (min max-channel 155 over grass, 190-229 over sky). Without the gate an empty tile over
//!    pale sky reads as neutral and would classify as grey.
//!
//! The caller also knows which rows it has submitted, so "is this row coloured yet" never has to be
//! guessed: [`read_row`] returns `None` until every tile classifies, which is the readiness check for
//! the animation as well as the rejection signal for a word the dictionary refused.
//!
//! ## Never read the answer
//!
//! `shrineview.lua:266` writes the answer into the save on the first submit —
//! `setFlag(data.dataKey..'word', word)`. It is right there, in a file this program already parses,
//! and reading it would make every shrine free. That is cheating and it is out of scope permanently,
//! along with deriving the word from the world seed. The grid on screen is the only input.

use crate::shrine::Pattern;
use crate::win::capture::Frame;

/// Where the block is anchored, in normalized client coordinates (`shrineview.lua:146`).
const ANCHOR: (f64, f64) = (0.5, 0.35);

/// The unscaled tile size the game scales down from (`shrineview.lua:41`).
const BASE_LETTER: f64 = 118.0;

/// Shrine words run 4..=7 letters (`getLength`, `shrineview.lua:17-20`, clamped to 4..7).
pub const MIN_LENGTH: usize = 4;
pub const MAX_LENGTH: usize = 7;

/// What one tile is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    /// Right letter, right place.
    Green,
    /// Right letter, wrong place.
    Yellow,
    /// Not in the word (or already accounted for).
    Grey,
}

impl Colour {
    fn digit(self) -> u16 {
        match self {
            Colour::Grey => 0,
            Colour::Yellow => 1,
            Colour::Green => 2,
        }
    }
}

/// The tile grid for a word of a given length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub length: usize,
    pub max_guesses: usize,
    pub tile: i32,
    /// Block top-left in client pixels.
    pub origin: (i32, i32),
}

impl Layout {
    /// Mirrors `shrineview.lua:39-44` exactly, then places the block by its centre.
    ///
    /// `client_w`/`client_h` are the client area; the game positions by a *fraction* of it and scales
    /// game units by [`crate::layout::scale`], which is the same model every other button uses.
    pub fn new(length: usize, client_w: i32, client_h: i32) -> Layout {
        let max_guesses = (30f64 / length as f64).ceil().clamp(6.0, 10.0) as usize;
        // `scale` is recomputed from the rounded letterSize in the game (`:42`), but only the rounded
        // letterSize is ever used for geometry, so that second assignment does not matter here.
        let s = (6.0 / max_guesses as f64).min(1.0);
        let letter = (BASE_LETTER * s + 0.5).floor();
        let ui = crate::layout::scale(client_w, client_h);
        let tile = (letter * ui).round() as i32;
        let width = tile * length as i32;
        let height = tile * max_guesses as i32;
        let cx = (client_w as f64 * ANCHOR.0).round() as i32;
        let cy = (client_h as f64 * ANCHOR.1).round() as i32;
        Layout { length, max_guesses, tile, origin: (cx - width / 2, cy - height / 2) }
    }

    /// Centre of the tile at 1-based `(row, col)`, in client pixels.
    pub fn tile_center(&self, row: usize, col: usize) -> (i32, i32) {
        let (ox, oy) = self.origin;
        (
            ox + (col as i32 - 1) * self.tile + self.tile / 2,
            oy + (row as i32 - 1) * self.tile + self.tile / 2,
        )
    }

    pub fn width(&self) -> i32 {
        self.tile * self.length as i32
    }

    pub fn height(&self) -> i32 {
        self.tile * self.max_guesses as i32
    }
}

/// Which word length puts its first tile where we just saw one.
///
/// Typing a single letter marks row 1 column 1 and nothing else, so the bounding box of what changed
/// *is* one tile: its width gives `letterSize` and its left edge gives the block's left edge. Both
/// then follow, because the block is centred:
///
/// ```text
///   length = (2 * (centre_x - left)) / tile
/// ```
///
/// Checked against all four layouts at 1920x1080 — (4, 89), (5, 118), (6, 118), (7, 118) — which are
/// distinguishable precisely because length 4 is the only one whose `maxGuesses` exceeds 6 and so the
/// only one that scales its tiles down.
///
/// Preferred over hunting for the faint empty grid, which over grass is barely separable from the
/// scene behind it — the same confusability that makes the colour classifier need a brightness gate.
pub fn length_from_tile(left: i32, tile: i32, client_w: i32) -> Option<usize> {
    if tile <= 0 {
        return None;
    }
    let cx = (client_w as f64 * ANCHOR.0).round() as i32;
    let span = 2 * (cx - left);
    if span <= 0 {
        return None;
    }
    // Round rather than truncate: the block's left edge is a rounded quantity itself.
    let n = ((span as f64) / (tile as f64)).round() as i32;
    let n = usize::try_from(n).ok()?;
    (MIN_LENGTH..=MAX_LENGTH).contains(&n).then_some(n)
}

/// Mean RGB over the middle of a tile, as `(r, g, b)`.
///
/// The box is `tile/3` either side of the centre, so it scales with the layout and stays clear of the
/// tile's rounded edge and the gap to its neighbour. Stride 3 because this runs once per tile per
/// turn and exactness buys nothing.
fn sample(frame: &Frame, cx: i32, cy: i32, radius: i32) -> Option<(u8, u8, u8)> {
    let (mut rs, mut gs, mut bs, mut n) = (0u64, 0u64, 0u64, 0u64);
    let mut y = cy - radius;
    while y <= cy + radius {
        let mut x = cx - radius;
        while x <= cx + radius {
            if x < 0 || y < 0 || x >= frame.width || y >= frame.height {
                return None;
            }
            let i = ((y as usize) * (frame.width as usize) + x as usize) * 4;
            let px = frame.bgra.get(i..i + 3)?;
            bs += px[0] as u64;
            gs += px[1] as u64;
            rs += px[2] as u64;
            n += 1;
            x += 3;
        }
        y += 3;
    }
    (n > 0).then(|| ((rs / n) as u8, (gs / n) as u8, (bs / n) as u8))
}

/// Green needs this much more green than red and than blue.
///
/// Live greens: `34 124 34` (G-R 90, G-B 90) and `25 141 25` (116, 116). The nearest thing that is
/// not green is an empty tile over grass at `130 155 82`, whose G-R is 25. Halfway is ~57; 40 is
/// placed nearer the impostor because the real margin above it is enormous.
const GREEN_OVER: i32 = 40;

/// Yellow needs this much more red than blue.
///
/// Live yellows: `146 124 34` (R-B 112) and `175 143 16` (159). Empty-over-grass is 48. 60 sits just
/// above the impostor with 52 to spare below the weaker real sample.
const YELLOW_R_OVER_B: i32 = 60;

/// ...and this much more green than blue, which is what separates yellow from a warm scene tint.
const YELLOW_G_OVER_B: i32 = 40;

/// Red may fall this far below green and still be yellow rather than green.
const YELLOW_R_UNDER_G: i32 = 40;

/// Grey's channels must agree within this.
const GREY_SPREAD: i32 = 25;

/// ...and grey must be dark. Live greys ran 65..83 at the maximum channel; the *dimmest* empty tile
/// measured was `152 172 163`, max 172. 130 splits that with 47 above the greys and 42 below the
/// empties.
const GREY_MAX: i32 = 130;

/// Classifies one sampled tile, or `None` if it is not a submitted tile at all.
///
/// **The palette is hardcoded, and that is an MVP shortcut with a known expiry.** The game reads
/// `userConfig.interface.shrine.green` / `.yellow` / `.grey` (`shrineview.lua:65-67`) and only falls
/// back to `{0,0.8,0}`, `{1,0.8,0}`, `{0.3,0.3,0.3}` when the player has not set them — and the
/// options screen lets them set any colour at all (`ui/options.lua:207-217`). A player with a
/// recoloured palette would make every read here wrong. Worse, `userConfig.interface.shrine.patterns`
/// (`shrineview.lua:186`) subtract-blends an overlay texture onto the tile centre, which is exactly
/// where this samples. Reading those settings out of the config is the correct fix and is deferred,
/// not forgotten.
pub fn classify(rgb: (u8, u8, u8)) -> Option<Colour> {
    let (r, g, b) = (rgb.0 as i32, rgb.1 as i32, rgb.2 as i32);
    if g >= r + GREEN_OVER && g >= b + GREEN_OVER {
        return Some(Colour::Green);
    }
    if r >= b + YELLOW_R_OVER_B && g >= b + YELLOW_G_OVER_B && r + YELLOW_R_UNDER_G >= g {
        return Some(Colour::Yellow);
    }
    let max = r.max(g).max(b);
    if max < GREY_MAX
        && (r - g).abs() <= GREY_SPREAD
        && (g - b).abs() <= GREY_SPREAD
        && (r - b).abs() <= GREY_SPREAD
    {
        return Some(Colour::Grey);
    }
    None
}

/// Reads one submitted row as a [`Pattern`], or `None` if it is not fully coloured yet.
///
/// `None` covers three different situations on purpose, because the caller's response to all three is
/// the same — look again:
///
/// - the row is still animating in;
/// - the guess was **rejected** by `utils.dictionary` (`shrineview.lua:261`), so the row never
///   coloured and the word is still sitting there uncommitted;
/// - the row was never submitted.
///
/// Distinguishing them needs a timeout, not a different read, which is the caller's business.
pub fn read_row(frame: &Frame, layout: &Layout, row: usize) -> Option<Pattern> {
    let radius = (layout.tile / 3).max(1);
    let mut code: Pattern = 0;
    // Base-3, least significant digit first, matching `crate::shrine::feedback`.
    for col in (1..=layout.length).rev() {
        let (cx, cy) = layout.tile_center(row, col);
        let colour = classify(sample(frame, cx, cy, radius)?)?;
        code = code * 3 + colour.digit();
    }
    Some(code)
}

/// Mean absolute RGB difference over a box, per pixel per channel.
fn box_change(a: &Frame, b: &Frame, cx: i32, cy: i32, radius: i32) -> Option<f64> {
    if a.width != b.width || a.height != b.height {
        return None;
    }
    let (mut sum, mut n) = (0u64, 0u64);
    let mut y = cy - radius;
    while y <= cy + radius {
        let mut x = cx - radius;
        while x <= cx + radius {
            if x < 0 || y < 0 || x >= a.width || y >= a.height {
                return None;
            }
            let i = ((y as usize) * (a.width as usize) + x as usize) * 4;
            let (pa, pb) = (a.bgra.get(i..i + 3)?, b.bgra.get(i..i + 3)?);
            for c in 0..3 {
                sum += (pa[c] as i32 - pb[c] as i32).unsigned_abs() as u64;
            }
            n += 3;
            x += 2;
        }
        y += 2;
    }
    (n > 0).then(|| sum as f64 / n as f64)
}

/// How much more the winning candidate must have moved than the runner-up.
///
/// A letter appearing in a tile is a large, local change; scene animation is a small, global one.
/// Requiring a margin means an ambiguous reading is reported as unknown rather than resolved by a
/// coin toss onto the wrong grid — where every subsequent sample would land in the gaps between
/// tiles and read as nothing.
const LENGTH_MARGIN: f64 = 4.0;

/// Which word length is this, judged by where a single typed letter landed?
///
/// The four possible grids put row 1 column 1 in four well-separated places at 1920x1080 — centres
/// x = 826, 724, 665, 606 for lengths 4, 5, 6, 7 — so one keystroke distinguishes them. Compare the
/// four candidate tile boxes between a frame taken before the keystroke and one after, and the tile
/// that gained a letter is the one that moved.
///
/// Deliberately **not** a whole-frame difference. The shrine scene animates continuously — the
/// character idles, the plants sway, cloud drifts across the sky the top row sits against — so a
/// bounding box of "everything that changed" would be the whole screen. Sampling only the four
/// places the answer can be turns that animation from a confound into a small common-mode offset.
pub fn infer_length(before: &Frame, after: &Frame, client_w: i32, client_h: i32) -> Option<usize> {
    let mut scored: Vec<(f64, usize)> = Vec::new();
    for len in MIN_LENGTH..=MAX_LENGTH {
        let l = Layout::new(len, client_w, client_h);
        let (cx, cy) = l.tile_center(1, 1);
        // `tile/4`, not the `tile/3` used for colour sampling: at 118 px tiles the wider box makes
        // the length-5 and length-6 candidates overlap (685..763 against 626..704), and two
        // candidates sharing pixels is exactly what the margin test cannot resolve. At a quarter
        // they are disjoint by a pixel, so each candidate is scored on evidence only it can see.
        if let Some(d) = box_change(before, after, cx, cy, (l.tile / 4).max(1)) {
            scored.push((d, len));
        }
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    match scored.as_slice() {
        [(best, len), (second, _), ..] if best - second >= LENGTH_MARGIN => Some(*len),
        _ => None,
    }
}

/// How long to let a screen transition run before acting on what is behind it.
///
/// **A fixed wait, deliberately, after a measured check failed to be one.**
///
/// This was [`TranslationWatch`](crate::observe::settle::TranslationWatch), on the reasoning that a
/// transition translates nearly the whole frame while a settled scene moves only a few percent — so
/// the end of the transition could be detected rather than waited out. A live run says otherwise:
/// every call reported `scene settled: false`, meaning it never converged and burned its entire
/// 6-second timeout. The probe that followed succeeded, but it succeeded *because six seconds had
/// passed*, not because anything was detected. That is a sleep wearing a check's clothing, which is
/// worse than a sleep: it reports a verdict nobody should trust.
///
/// The cause is scene-specific and worth recording. `TranslationWatch` settles once the frame diff
/// falls to a quarter of the peak it saw, which suits the combat walk it was written for. The shrine
/// screen never gets that quiet — the character idles, the plants sway, and cloud drifts across the
/// sky the grid's top row sits against — so the residue stays above a quarter of the peak
/// indefinitely.
///
/// So: **four seconds, and it is now a deadline rather than a sleep.** The game's own
/// `transitionDuration` defaults to 0.5 s (`utils/defaultconfig.lua:5`), so this is eight times the
/// nominal fade.
///
/// It used to be a flat two-second `sleep` that every consumer re-checked afterwards, on the
/// reasoning that nothing downstream depended on it being exactly right. That reasoning was sound
/// and is exactly why this became a poll: [`crate::act::wait_for`] returns the *instant* the screen
/// is recognisable, so a shrine that opens in 200 ms costs 200 ms, and the four seconds are only
/// ever paid by a press that genuinely did not land. The old sleep paid two seconds every time and
/// still could not tell those two cases apart.
const VISIT_OPENS_WITHIN: std::time::Duration = std::time::Duration::from_secs(4);

/// The overworld `Visit` button on a completed shrine.
///
/// `ss(0, 0.85)` with `xOffset = 0.75` and the 250x100 `default`
/// (`overworld/locations/shrine.lua:62-63`), so the centre is `0 + 250*0.75 = 187.5`, `1080*0.85 =
/// 918`. Verified live: the button rendered exactly there.
///
/// **It shares its slot with `Combat`** (`:74-76`), which is shown by the complementary condition —
/// `currentAreaIsNotcomplete` against `currentAreaIsComplete`. So the slot always holds exactly one
/// of them, and pressing it on an *uncleared* shrine starts a fight instead of opening the word. The
/// caller must know the area is complete; that is why [`play`] is only reached through the map's
/// `completed` flag rather than by seeing a button.
pub const VISIT_CLICK: (i32, i32) = (188, 918);

/// How many times to press `Visit` before accepting that the shrine will not open.
///
/// **Three, because the overworld demonstrably swallows presses.** Live 2026-08-17 at `shrine1sub1`
/// the dev *heard the button's own click* and the screen never changed; `gave-up.png` caught the map
/// still up with `Visit` un-pressed, and the run stopped on an unplayed shrine.
///
/// That sound is the evidence that makes a retry the right remedy rather than a guess.
/// `button_down` plays inside `mousepressed` (`ui/elements/button.lua:278-281`) and only on a button
/// that is both `show` and `active`, so the press was delivered, in bounds, to a live `Visit`. What
/// never happened was the *release*: `mousereleased` (`:289-295`) bails on `if not down then return`,
/// and `down` is per button **instance**. Any rebuild of the area buttons between our press and our
/// release hands the release to a fresh object whose `down` is nil, and it is dropped in silence —
/// `core.refreshAreaButtons` (`overworldview.lua:474-481`) rebuilds exactly that list. **That last
/// step is inference, not a reading**: it is the only path I found that clears `down` without
/// running `activate`, but nothing yet proves it is the one that fired. The retry does not depend on
/// it being right — it only depends on the loss being intermittent, which the sound proves.
const VISIT_ATTEMPTS: usize = 3;

/// Presses `Visit` until the shrine screen is up, and says whether it ever was.
///
/// The confirmation is [`crate::act::SHRINE_GOBACK`], the same plaque [`consecrate`] has always
/// checked and the same one `identify` reads. This run measured its discrimination for free: on the
/// subworld map it scored **0.1311** against a 0.90 bar, so "the plaque is absent" and "we are still
/// looking at the map" are the same statement.
///
/// **The retry is guarded, because a stray press here is not harmless.** `Visit` sits at (188, 918)
/// and the shrine screen's own `Go back` occupies that coordinate once it is up — `SHRINE_GOBACK`'s
/// template spans it, and its click point is (113, 972) in the same plaque. So pressing again into a
/// screen that opened *late* would shut the shrine the previous press just opened, turning a
/// recoverable stall into a silent walk-away. Hence the second look before every retry: `wait_for`
/// last sampled a poll ago, and this closes that gap.
///
/// (An older note here claimed this coordinate collided with `Consecrate` and `Pray`. It does not —
/// both are at (1733, 972), the far side of the screen. The collision is with `Go back`, which is
/// the more dangerous one, and the reason that note is now corrected rather than merely deleted.)
fn open_the_shrine(
    win: &crate::win::window::GameWindow,
    input: &dyn crate::win::input::Input,
    log: &mut String,
) -> Result<bool, crate::Error> {
    for attempt in 1..=VISIT_ATTEMPTS {
        input.click(VISIT_CLICK.0, VISIT_CLICK.1)?;
        let opened = crate::act::wait_for(
            win,
            &crate::act::SHRINE_GOBACK,
            crate::act::SHRINE_GOBACK_PRESENT,
            VISIT_OPENS_WITHIN,
        );
        if opened.found() {
            log.push_str(&match attempt {
                1 => "  shrine: entered\n".to_string(),
                n => format!("  shrine: entered on press {n} — the overworld swallowed the first {}\n", n - 1),
            });
            return Ok(true);
        }
        log.push_str(&format!(
            "  shrine: press {attempt} of {VISIT_ATTEMPTS} did not open the screen (back plaque \
             best {:.4} over {} looks)\n",
            opened.best, opened.looks
        ));
        // Close the race described above before pressing again.
        let late = crate::act::score_exact(win, &crate::act::SHRINE_GOBACK).unwrap_or(0.0);
        if late >= crate::act::SHRINE_GOBACK_PRESENT {
            log.push_str(&format!(
                "  shrine: the screen arrived at {late:.4} as we were giving up on it — not \
                 pressing again, that would press `Go back`\n"
            ));
            return Ok(true);
        }
    }
    Ok(false)
}

/// How long to wait for the shrine screen to come **back** after a consecration.
///
/// Consecrating is a round trip, not an exit. `shrine.lua:250-253` takes `returnMode =
/// getActiveMode()` when `areaUnused(key)` — which is exactly the case where a blessing is still
/// owed — then `setActiveMode(overworld)` (`:283`) hands the screen to the beam animation, and the
/// beam's `onDecay` puts it back with `setActiveMode(returnMode)` (`:276-278`). So the shrine screen
/// closing is the *middle* of the interaction, and a driver that reads it as the end walks away
/// between the two halves.
///
/// Three seconds of animation, from `hellportal.lua:122-138`: the beam's alpha ramps to 1 at
/// `+delta` per second, then `life` (passed as `1` at `shrine.lua:260`) burns down at the same rate,
/// then alpha falls back to 0, and only then does `onDecay` fire. Twelve seconds is four times that,
/// because the cost of being wrong is asymmetric — waiting a few seconds too long costs a few
/// seconds, and giving up too early abandons the reward the whole trip was for.
const BEAM_RETURN: std::time::Duration = std::time::Duration::from_secs(12);

/// How long to look for a `Pray` that should already be on screen.
///
/// Short, because this only ever asks about a button the game draws immediately if it draws it at
/// all — either the word was solved on an earlier visit, or it was not. The cost of asking is paid
/// once per shrine entry; the cost of not asking is a blessing left behind on every revisit.
const PRAY_ALREADY_THERE: std::time::Duration = std::time::Duration::from_millis(2500);

// A `leave()` helper used to live here, pressing the back plaque and confirming it gone. It moved
// out rather than being deleted: [`crate::act::SHRINE_GOBACK`] and `Screen::Shrine` are how the
// observer loop leaves this screen now, so the exit is handled once, by the code that handles every
// other screen's exit, instead of twice.

/// What a visit to a shrine did.
#[derive(Debug, Clone, Default)]
pub struct Played {
    /// Every guess and the colouring it drew, in order.
    pub guesses: Vec<(String, Pattern)>,
    /// The answer, once a row came back all green.
    pub solved: Option<String>,
    /// Whether `Pray` was found and pressed.
    pub prayed: bool,
    /// Whether the solve was cashed in as a **consecration**, which is what the slot holds while the
    /// portal is live. Distinct from [`Played::prayed`] because they are different rewards claimed
    /// under different conditions, and collapsing them would hide which one a run actually got.
    pub consecrated: bool,
    /// Human-readable trail, for the run report.
    pub log: String,
}

/// Presses `Pray` if it is in the slot, and records what happened.
///
/// Every caller here is asking the same question — *is the blessing claimable now?* — of a slot that
/// holds one of several buttons, so the button is **identified** rather than the rect counted as
/// occupied. `Pray`'s artwork against an active `Consecrate` scores ~0.856 against a 0.92 threshold
/// (see [`crate::act::SHRINE_PRAY_PRESENT`]), which is the margin this depends on.
///
/// A miss is not an error. `showPrayButton` (`shrine.lua:98-102`) has three conjuncts, and the
/// honest report for "one of them is false" is a logged score, not a failure — the run has better
/// things to do than stop over an unclaimed bonus.
fn claim_blessing(
    win: &crate::win::window::GameWindow,
    out: &mut Played,
    wait: std::time::Duration,
) -> Result<bool, crate::Error> {
    let found =
        crate::act::wait_for(win, &crate::act::SHRINE_PRAY, crate::act::SHRINE_PRAY_PRESENT, wait);
    if !found.found() {
        out.log.push_str(&format!(
            "  shrine: no `Pray` in the slot (best {:.4} < {:.2})\n",
            found.best,
            crate::act::SHRINE_PRAY_PRESENT
        ));
        return Ok(false);
    }
    let score = found.score.unwrap_or(found.best);
    crate::act::click_exact(win, &crate::act::SHRINE_PRAY, crate::act::SHRINE_PRAY_PRESENT)?;
    // The button going away is the confirmation available on screen; the flag behind it is
    // `<key>_used`, which `doPray` writes and saves immediately (`shrine.lua:132-135`), so the
    // caller's `apply_save` sees it without waiting for a screen exit.
    out.prayed = crate::act::wait_until_gone(
        win,
        &crate::act::SHRINE_PRAY,
        crate::act::SHRINE_PRAY_PRESENT,
        std::time::Duration::from_secs(6),
    );
    out.log.push_str(&format!("  shrine: prayed={} (Pray scored {score:.4})\n", out.prayed));
    Ok(out.prayed)
}

/// Presses `Consecrate`, waits out the beam, and takes the blessing if one is still owed.
///
/// The full post-solve sequence with the portal open, in one place because two callers need it: the
/// one that has just solved the word, and the one that walked in on a shrine solved in an earlier
/// run. Both are looking at the same screen by the time they get here.
///
/// **The beam is the part worth reading.** Consecrating is a round trip, not an exit.
/// `shrine.lua:250-253` takes `returnMode = getActiveMode()` when `areaUnused(key)`, then
/// `setActiveMode(overworld)` (`:283`) hands the screen to the animation, and the beam's `onDecay`
/// puts it back (`:276-278`). So the shrine screen going away is the *middle* of the interaction.
/// Waiting is also the safe move rather than merely the patient one: the animation runs with
/// `setInteractionEnabled(false)` (`:258`), so a driver that returns to its loop here spends three
/// seconds clicking into a dead screen. Live 2026-08-15 it did exactly that, missed four locate-me
/// probes, and opened the stats history page by accident before the screen came back.
///
/// The blessing may legitimately not be there. A shrine solved before the anomaly opened was prayed
/// at then — `Pray` is the *only* reward available with the portal shut — so `areaUnused` is already
/// false and `showPrayButton`'s last conjunct fails. `claim_blessing` reports that as a score and
/// moves on, which is why this asks unconditionally instead of trying to predict it.
fn spend_the_solve(
    win: &crate::win::window::GameWindow,
    input: &dyn crate::win::input::Input,
    out: &mut Played,
    wait: std::time::Duration,
) -> Result<(), crate::Error> {
    use std::time::Duration;

    // Identified, not sampled once: the shrine screen fades in, and a press aimed at a plaque that
    // has not finished rendering is this project's most-repeated bug.
    let found = crate::act::wait_for(
        win,
        &crate::act::SHRINE_CONSECRATE,
        crate::act::SHRINE_CONSECRATE_PRESENT,
        wait,
    );
    if !found.found() {
        out.log.push_str(&format!(
            "  shrine: no active `Consecrate` (best {:.4} < {:.2}) — not clicking the slot blind\n",
            found.best,
            crate::act::SHRINE_CONSECRATE_PRESENT
        ));
        return Ok(());
    }
    let slot = found.score.unwrap_or(found.best);
    crate::act::click_exact(win, &crate::act::SHRINE_CONSECRATE, crate::act::SHRINE_CONSECRATE_PRESENT)?;
    // The whole shrine screen going away is the game's own acknowledgement of the press — a far
    // stronger signal than watching a slot swap in place, which this project has been fooled by.
    out.consecrated = crate::act::wait_until_gone(
        win,
        &crate::act::SHRINE_GOBACK,
        crate::act::SHRINE_GOBACK_PRESENT,
        Duration::from_secs(8),
    );
    out.log.push_str(&format!("  shrine: consecrated={} (slot scored {slot:.4})\n", out.consecrated));
    if !out.consecrated {
        return Ok(());
    }

    let back =
        crate::act::wait_for(win, &crate::act::SHRINE_GOBACK, crate::act::SHRINE_GOBACK_PRESENT, BEAM_RETURN);
    if back.found() {
        out.log.push_str("  shrine: the screen came back after the beam\n");
    } else {
        // Either the beam is slower than `hellportal.lua:122-138` suggests, or `returnMode` was
        // never taken — which is itself the signal that the blessing was already claimed, since
        // that is the condition it is captured under. Re-entering costs one click, and
        // `claim_blessing` refuses to press anything it cannot identify, so guessing wrong here
        // costs a second rather than a wrong button.
        out.log.push_str(&format!(
            "  shrine: no return to the shrine screen within {BEAM_RETURN:?} (best {:.4}) — \
             re-entering to look for `Pray`\n",
            back.best
        ));
        // Not fatal if it never opens: `claim_blessing` below refuses to press anything it cannot
        // identify, so the cost of arriving here on the map is a logged miss rather than a wrong
        // button. Retried anyway, because this is the same overworld press that is known to be lost.
        open_the_shrine(win, input, &mut out.log)?;
    }
    claim_blessing(win, out, PRAY_ALREADY_THERE)?;
    Ok(())
}

/// Plays the word at the shrine we are standing on, then prays.
///
/// Assumes the overworld is showing and the shrine's area is **complete**, so the slot holds `Visit`
/// rather than `Combat` — see [`VISIT_CLICK`].
///
/// ## Why the band is `Wild`
///
/// `shrine.lua:497` picks `hard` when the player carries the `shrineWordHard` gear flag and `easy`
/// otherwise, and we do not read gear flags yet. Guessing wrong is not symmetric: too narrow a band
/// **eliminates the true answer** and the shrine becomes unwinnable with no error anywhere, while too
/// wide only costs guess quality. So the union is used until the flag is read.
///
/// ## The one failure worth naming
///
/// If the game rejects a word this proposes, the baked guess list disagrees with
/// `utils.dictionary` and is stale — the exact silent-rot failure [`crate::shrine`] warns about.
/// That is logged as such rather than retried into a loop, because no amount of retrying fixes it.
/// `anomaly_open` decides **which button the solve is expected to produce**, and it is the game's own
/// rule rather than a guess. `shrine.lua:98-103` shows `Pray` only when
/// `areaUnused and (hell == 0 or consecrated or desecrated)`, while `Consecrate` needs
/// `majorShrine and hell ~= 0` (`:92-95`) — and every shrine is major
/// (`overworld/generators/world.lua:86-89`). So at an unconsecrated shrine with the portal live,
/// `Pray` **cannot** be drawn and the slot holds `Consecrate`.
///
/// ## `Consecrate` **then** `Pray`, not one or the other
///
/// That rule says which button comes *first*, and this function used to read it as saying which
/// button comes *instead*. `showPrayButton`'s middle conjunct is
/// `(not majorShrine or hell == 0 or isConsecrated or isDesecrated)` — consecrating satisfies it, so
/// the press that spends the solve is also the press that unlocks the blessing. Both rewards are
/// available on the same visit, in that order, and the game even walks the screen back for us: the
/// consecration takes `returnMode` when the area is unused (`shrine.lua:250-253`) and restores it
/// when the beam decays (`:276-278`).
///
/// ## What is owed depends on *when* the shrine was solved
///
/// The two rewards are gated on different things — consecration on `hell ~= 0 and not isConsecrated`
/// (`:244`), the blessing on `areaUnused(key)` (`:101`) — so they are not two halves of one
/// transaction and a shrine can arrive owing either, both, or neither:
///
/// - **Before the anomaly opens, praying is the only reward on offer.** `showConsecrateButton`
///   needs `hell ~= 0`, so with the portal shut a solved shrine shows `Pray` and nothing else.
/// - **After it opens, every shrine can be consecrated once**, and the blessing is a separate
///   question. A shrine solved *now* owes both, in order. A shrine solved *earlier* was prayed at
///   earlier — there was nothing else to do there — so it owes the consecration alone and no `Pray`
///   will appear after the beam.
///
/// This is the dev's model, and it corrects an assumption in the first version of this code: that a
/// solved shrine always has a blessing waiting. It usually does not, and the cost of assuming it was
/// not a wasted look but a missed consecration — see the table at the entry check.
///
/// Live 2026-08-15, with the portal open: five shrines consecrated, none prayed at. The run logged
/// success at each one and left the blessing behind every time. The symptom the dev saw was not the
/// missing blessing but the wandering — `doPray` is what calls `setAreaUsed` (`:133`), so an
/// unprayed shrine keeps `_used` false, `worth_a_trip` stays true, and the planner walks back to a
/// shrine it has already finished instead of going to the anomaly.
///
/// Live 2026-08-10, before this argument was passed: `shrine6` was solved in four guesses, the slot
/// scored **0.8560** — the measured signature of an active `Consecrate`, see
/// [`crate::act::SHRINE_PRAY_PRESENT`] — and the run logged `solved but no Pray button` and walked
/// away from a blessing it had already earned.
/// ## `already_open`: the game sometimes opens the shrine for us, and that is a gift
///
/// Winning a fight *at* a shrine does not put us back on the map. `overworld.lua:1070-1079`:
///
/// ```lua
/// if runSaveData.rpg.scenario.shrine then
///     local shrine = require'shrine'
///     shrine:load({ back = self, location = ..., directFromCombat = true })
///     postgame.setBackMode(shrine)
/// ```
///
/// So the postgame screen's way back **is** the shrine, and dismissing it lands on a fully live
/// shrine screen — `directFromCombat` only skips rebuilding the backdrop the fight already drew
/// (`shrine.lua:514`). That is the game handing us the consecration on the way out of the fight.
///
/// Pass `true` there and step 1 is skipped. It has to be skipped rather than merely wasted: the
/// `Visit` coordinate lands on the shrine screen's own `Go back` plaque once that screen is up, so a
/// press aimed at opening a shrine that is *already* open closes it instead. See
/// [`open_the_shrine`], which guards its retries against the same collision.
///
/// (This note used to name `Consecrate` and `Pray` as the buttons underneath. That was wrong — both
/// sit at (1733, 972), the far side of the screen. The hazard is `Go back`.)
pub fn play(
    win: &crate::win::window::GameWindow,
    input: &dyn crate::win::input::Input,
    anomaly_open: bool,
    already_open: bool,
) -> Result<Played, crate::Error> {
    use crate::shrine::{max_guesses, show, solved, Baked, Band, Solver};
    use crate::win::capture::capture_window;
    use std::time::{Duration, Instant};

    let mut out = Played::default();
    let (cw, ch) = win.client_size()?;

    // 1. Open the word screen, and **confirm it opened** — see [`open_the_shrine`].
    //
    //    This used to click `Visit` and sleep, on the reasoning that the grid answering the length
    //    probe below was confirmation enough. It is not, and the difference is not academic. When
    //    the press is lost the probe types its `a` and its backspace into the *overworld*, which
    //    binds user functions to keys (`overworld.lua:1359-1368` — zoom, inventory, screenshot). So
    //    a swallowed click did not merely fail; it fired stray keystrokes at the map and then
    //    reported the shrine as unplayable. Confirming first is what makes the probe safe to run.
    let before_visit = capture_window(win)?;
    match already_open {
        true => out.log.push_str("  shrine: already open — the fight handed it to us\n"),
        false => {
            if !open_the_shrine(win, input, &mut out.log)? {
                out.log.push_str(
                    "  shrine: `Visit` never opened the screen — leaving without playing, and \
                     without typing the probe into the map\n",
                );
                return Ok(out);
            }
        }
    }

    // 1b. **Is the word already solved?** A win is not a property of this visit. `hasWon()` is
    //     `word == submitions[#submitions-1]` (`shrineview.lua:57-59`), and the submissions list is
    //     saved to `<dataKey>subs` on every accepted guess (`:267`) and handed back to the view on
    //     the next entry (`shrine.lua:495`). So a shrine solved in an earlier run — or in an earlier
    //     visit this run — opens already won, with its reward waiting in the slot.
    //
    //     Asked of the screen rather than of the save, because the slot answers the whole question
    //     at once. The save would need one query per route into this state, and there are four:
    //
    //     | slot on entry        | what it means                        | what is owed  |
    //     |----------------------|--------------------------------------|---------------|
    //     | nothing              | not solved                           | the word      |
    //     | active `Consecrate`  | solved, portal open, unconsecrated    | consecration  |
    //     | `Pray`               | solved, and the blessing is unclaimed | the blessing  |
    //     | greyed `Consecrate`  | solved, consecrated, already prayed   | nothing       |
    //
    //     **`Consecrate` is asked first, and the order is the correction.** This began by asking for
    //     `Pray`, on the assumption that a solved shrine always has a blessing waiting. It does not:
    //     with the portal shut, praying is the *only* reward a shrine offers, so a shrine solved
    //     before the anomaly opened was prayed at then and comes back showing `Consecrate` with
    //     `areaUnused` already false. Asking the wrong one first is not a wasted second — `Pray`
    //     would be absent, the code would fall through to the word, and it would type a probe letter
    //     into a finished board and never consecrate the shrine it walked there for.
    let ready = crate::act::wait_for(
        win,
        &crate::act::SHRINE_CONSECRATE,
        crate::act::SHRINE_CONSECRATE_PRESENT,
        PRAY_ALREADY_THERE,
    );
    if ready.found() {
        out.log.push_str(&format!(
            "  shrine: already solved, and `Consecrate` is live at {:.4} — spending it\n",
            ready.score.unwrap_or(ready.best)
        ));
        // The solve is not this visit's, but it is a solve, and everything downstream reads this
        // field to mean "the word is done". Recording it as such is what stops the driver reporting
        // a shrine it consecrated as one it failed at.
        out.solved = Some(String::new());
        spend_the_solve(win, input, &mut out, Duration::from_secs(6))?;
        return Ok(out);
    }
    if claim_blessing(win, &mut out, PRAY_ALREADY_THERE)? {
        out.log.push_str("  shrine: the word was already solved from an earlier visit\n");
        out.solved = Some(String::new());
        return Ok(out);
    }

    // 2. Find out how long the word is, by typing one letter and seeing which grid it landed in.
    //    Retried a couple of times because the screen transition may still be fading.
    let mut length = None;
    for attempt in 1..=3 {
        let before = capture_window(win)?;
        crate::win::input::type_text_injected("a", Duration::from_millis(30))?;
        // 100 ms, not the 400 this started at. By this point `wait_for_scene` has already settled
        // the transition, so the only thing being waited on is one letter appearing in one tile —
        // a local redraw on an already-static screen, which lands in the next frame. At the 120 FPS
        // this game runs at that is ~8 ms, so 100 leaves an order of magnitude of headroom.
        //
        // Cheap to be wrong about in this direction: a letter that has not rendered yet moves all
        // four candidate boxes equally, `infer_length` declines rather than guesses, and the retry
        // loop takes another sample. Being slow here would have been paid on every shrine.
        std::thread::sleep(Duration::from_millis(100));
        let after = capture_window(win)?;
        length = infer_length(&before, &after, cw, ch);
        // Always undo the probe letter, whether or not it was recognised.
        //
        // 34 ms, matching every other keystroke gap here — `remove` trims one character with no
        // animation, so a frame or two is the whole requirement.
        //
        // This is the gap with the nastiest failure mode, which is why it is not left generous
        // "just in case": if the backspace has not landed before the next attempt types its `a`,
        // the row holds `aa`, the tile that changes is column 2 rather than column 1, and
        // `infer_length` returns a **wrong** length rather than declining. Every other timing here
        // fails safe; this one fails silent. The retry loop cannot catch it either, since a
        // confident wrong answer ends the loop.
        input.press_key(crate::win::input::VK_BACK, crate::win::input::SC_BACK)?;
        std::thread::sleep(Duration::from_millis(34));
        if let Some(n) = length {
            out.log.push_str(&format!("  shrine: {n}-letter word (probe attempt {attempt})\n"));
            break;
        }
    }
    let Some(length) = length else {
        // Not on the shrine screen, or the grid is somewhere this does not model. Guessing a grid is
        // not an option, so give up and hand the screen back — the same exit as the success path.
        //
        // The back plaque has already answered the question this used to ask. Reaching here means
        // the shrine screen *is* up (or the fight handed it to us), so this is now the narrow case
        // it always claimed to be: a shrine screen whose grid is somewhere this does not model.
        //
        // The diff is kept anyway, because it is the one number that would catch the screen having
        // moved on underneath us between the plaque check and the probe — a possibility no template
        // check covers.
        let moved = capture_window(win)
            .map(|now| before_visit.diff_fraction(&now, crate::observe::settle::FULL))
            .unwrap_or(0.0);
        out.log.push_str(&format!(
            "  shrine: could not identify the grid (screen moved {moved:.4} since Visit) — leaving \
             without playing\n"
        ));
        return Ok(out);
    };

    let layout = Layout::new(length, cw, ch);
    let mut solver = Solver::new(&Baked, length, Band::Wild)?;
    let win_pattern = solved(length);

    // 3. Guess until it is green or the budget runs out.
    for turn in 1..=max_guesses(length) {
        let Some(guess) = solver.propose() else {
            out.log.push_str("  shrine: candidate set emptied — a colouring was misread\n");
            break;
        };
        crate::win::input::type_text_injected(&guess, Duration::from_millis(30))?;
        // 34 ms — about four frames at the 120 FPS this game runs at, and two at 60. This is not
        // waiting for anything to be *drawn*: `submit` reads `submitions[#submitions]`
        // (`shrineview.lua:232-234`), which `insert` has already filled from the text events. The
        // gap only has to outlast the game consuming the last keystroke, and a frame is the unit
        // that happens in.
        //
        // If it turns out to be too short the failure is loud rather than subtle: `submit` refuses
        // outright unless the row holds exactly `wordLength` characters, so a dropped letter leaves
        // the guess uncommitted and the row never colours — which `read_row` already reports as a
        // timeout rather than misreading.
        std::thread::sleep(Duration::from_millis(34));
        input.press_key(crate::win::input::VK_RETURN, crate::win::input::SC_RETURN)?;

        // Read → act → re-read: the row is not readable the instant Enter goes out, and `read_row`
        // returning None is the readiness check as well as the rejection signal.
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut pattern = None;
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(250));
            let frame = capture_window(win)?;
            if let Some(p) = read_row(&frame, &layout, turn) {
                pattern = Some(p);
                break;
            }
        }
        let Some(pattern) = pattern else {
            // The row never coloured. The likeliest cause by far is that `utils.dictionary` refused
            // the word, which leaves it sitting in the row uncommitted — so clear it before leaving,
            // or the next screen inherits a half-typed guess.
            out.log.push_str(&format!(
                "  shrine: {guess:?} drew no colouring in 6 s — the baked guess list may be stale \
                 against utils.dictionary\n"
            ));
            for _ in 0..length {
                input.press_key(crate::win::input::VK_BACK, crate::win::input::SC_BACK)?;
                // Same 34 ms, same reason: `remove` (`shrineview.lua:227-231`) trims one character
                // per press with no animation to wait out, so the only requirement is that the game
                // sees each press as its own event rather than coalescing them.
                std::thread::sleep(Duration::from_millis(34));
            }
            break;
        };

        solver.observe(&guess, pattern);
        out.log.push_str(&format!(
            "  shrine {turn}. {guess} {} ({} left)\n",
            show(pattern, length),
            solver.remaining()
        ));
        out.guesses.push((guess.clone(), pattern));
        if pattern == win_pattern {
            out.solved = Some(guess);
            break;
        }
    }

    // 4. Claim the reward. The slot is shared and swaps the moment we use it, so this is read before
    //    it is pressed and confirmed by its disappearance — never clicked blind.
    //
    //    **Which button is there is decided by the portal, not by hope.** With it open the solve
    //    yields `Consecrate`, and `Pray` is unreachable until that is done; see this function's doc.
    //    Matching `Pray`'s artwork against an active `Consecrate` scores ~0.856 against a 0.92
    //    threshold, so asking the wrong question here reads as "no button" and abandons the blessing.
    if out.solved.is_some() && anomaly_open {
        out.log.push_str(
            "  shrine: portal is open, so the solve yields `Consecrate` first\n",
        );
        spend_the_solve(win, input, &mut out, Duration::from_secs(6))?;
    } else if out.solved.is_some() {
        claim_blessing(win, &mut out, Duration::from_secs(6))?;
    }

    // 5. Stop here, whatever happened, and hand the screen back to the observer loop.
    //
    // Pressing `Pray` is the last thing this function knows how to do. What follows it — a blessing
    // cutscene with a progress plaque, a text screen, the shrine screen again with a greyed
    // `Consecrate` where `Pray` was — are all ordinary screens the loop already recognises and
    // clears, and it does so with the loop's own checks rather than a second copy of them here.
    //
    // The earlier version walked that sequence itself: sleep, click the progress plaque, sleep,
    // click the back plaque. It worked once and then did not, because it was open-loop about screens
    // it did not open. The run finished a shrine, cleared a text screen it had not expected, and
    // ended up on the stats history page with no idea it had moved.
    //
    // So the contract is narrow and honest: this returns with the game somewhere in the shrine's
    // aftermath, and `identify` says where.
    Ok(out)
}

/// What a consecration attempt did.
#[derive(Debug, Clone, Default)]
pub struct Consecrated {
    /// The shrine screen closed after the click, which is the game's own confirmation.
    pub done: bool,
    /// What [`crate::act::SHRINE_PRAY`]'s artwork scored against whatever was in the slot.
    ///
    /// Recorded on every attempt because this is the **first measurement of an active
    /// `Consecrate`** this project has ever taken — see [`crate::act::SHRINE_PRAY_PRESENT`], which
    /// predicts ~0.82 and asks to be re-measured at the first open anomaly. This is that run.
    pub slot_score: f64,
    pub log: String,
}

/// Consecrates the shrine we are standing on.
///
/// Assumes the overworld is showing, the shrine's area is complete (so the slot holds `Visit`), and
/// that the **caller has checked the save**: [`crate::overworld::WorldMap::worth_consecrating_here`]
/// is the gate, and it is not optional. See below.
///
/// ## Why this is a separate function from [`play`]
///
/// They are different errands at different times. `play` solves the word and prays, and it only
/// applies to a shrine that is `completed && !used`. Consecrating applies to one that is *already*
/// used — the word solved and the blessing taken, on an earlier visit or in an earlier run — and
/// needs only that the anomaly be open. A shrine can want one, the other, or neither.
///
/// ## The slot, and why the save has to vote
///
/// `Consecrate` sits at `ss(1.0, 0.9)`, `xOffset -0.75` — the same 250x100 rect as `Pray`, `Read`
/// and `Desecrate` (`shrine.lua:241,296,302,320`). We have no template for it, and one cannot be
/// fabricated: an *active* `Consecrate` is green-tinted `up` artwork
/// (`backgroundColor = 0.75, 1, 0.75`) that no run has ever seen. So the identification is done
/// where it can be done honestly — from the save:
///
/// ```lua
/// -- shrine.lua:98-102
/// showPrayButton = ... and overworldview.areaUnused(shrineLocation.key)
/// -- shrine.lua:93-95
/// showConsecrateButton = ShowAGoodButton() and majorShrine
///                        and (not (hell == 0 or isConsecrated(loc)) or areaHasBeenUsed(loc.key))
/// ```
///
/// At a shrine with `<key>_used` set and no `<key>_consecrated`, with `hell ~= 0`: `Pray` is
/// excluded by `areaUnused`, `Read` and `Desecrate` need the `spellSwears` gear flag, and
/// `Consecrate` both shows and is active. **One button can be there.** The screen then confirms that
/// a button is there at all, at [`crate::act::SHRINE_SLOT_OCCUPIED`].
///
/// `ShowAGoodButton` needs `shrineView.hasWon()`, which is not directly readable — but `_used` is
/// only ever set by `doPray` (`shrine.lua:271`), and `doPray` is only reachable behind
/// `showPrayButton`, which itself requires `hasWon()`. So **used implies solved**, and the gate
/// stands on that rather than on a guess.
///
/// ## Confirmation is the screen closing, not the button changing
///
/// `Consecrate`'s handler ends in `setActiveMode(overworld)` (`shrine.lua:288`), so a successful
/// press takes the whole shrine screen away. That is a far stronger signal than watching the slot,
/// which is exactly the kind of in-place swap this project has been fooled by before. The
/// authoritative check is the `<key>_consecrated` flag reaching the save, and the caller does that
/// by re-reading afterwards — `overworld:save()` runs in the beam's `onDecay` callback (`:281`), so
/// it is not instant, which is the standing save-flush caveat rather than a failure.
pub fn consecrate(
    win: &crate::win::window::GameWindow,
    input: &dyn crate::win::input::Input,
) -> Result<Consecrated, crate::Error> {
    use std::time::Duration;

    let mut out = Consecrated::default();

    // 1. Open the shrine screen. Same plain click `play` uses, and confirmed the same way it
    //    confirms everything on this screen -- by the back plaque, which is what `identify` reads.
    if !open_the_shrine(win, input, &mut out.log)? {
        out.log.push_str("  consecrate: the shrine screen did not open — not consecrating blind\n");
        return Ok(out);
    }

    // 2. **Which** button is in the slot, and is it live? This used to ask only whether the rect was
    //    painted, on the reasoning that the save had already said what must be there. That is half a
    //    check by construction, and the half it skipped is the one that failed: at `shrine3` on
    //    2026-08-10 the click went somewhere that opened the stats history page.
    //
    //    Waited for rather than sampled, because the shrine screen fades in and a plaque that has
    //    not finished rendering scores like an empty slot.
    let found = crate::act::wait_for(
        win,
        &crate::act::SHRINE_CONSECRATE,
        crate::act::SHRINE_CONSECRATE_PRESENT,
        Duration::from_secs(6),
    );
    out.slot_score = found.score.unwrap_or(found.best);
    if !found.found() {
        out.log.push_str(&format!(
            "  consecrate: no active `Consecrate` (best {:.4} < {:.2}) — not clicking it blind\n",
            found.best,
            crate::act::SHRINE_CONSECRATE_PRESENT
        ));
        return Ok(out);
    }
    out.log.push_str(&format!(
        "  consecrate: active `Consecrate` identified at {:.4}\n",
        out.slot_score
    ));

    // The artwork itself. The template now exists, so this is no longer *for* cutting one — it is
    // for the gap [`crate::act::SHRINE_CONSECRATE_PRESENT`] names: the template was cut from a single
    // hand capture, so an active `Consecrate` has never been scored against different scenery. Every
    // consecration from here leaves that second sample on disk.
    //
    // It has to be taken **here** — on the shrine screen, at the slot's rect, after the button has
    // been identified and before it is clicked. The first attempt at this called the driver's
    // `snap_area_slot` from `navigate`, which photographs the *overworld* area slot: the capture
    // came back a picture of `Visit`, labelled `consecrate-live`, and the log line said "captured
    // the area slot" exactly as it should have. The name was mine and the code was honest.
    let (sx, sy, sw, sh) = crate::act::SHRINE_PRAY.search;
    match crate::win::capture::capture_client_rect(win, sx, sy, sw - sx, sh - sy) {
        Ok(f) => {
            let path = std::path::Path::new("spike-frames-live").join("shrine-consecrate-live.png");
            match f.write_png(&path) {
                Ok(()) => out.log.push_str("  consecrate: captured the live slot artwork\n"),
                Err(e) => out.log.push_str(&format!("  consecrate: could not write it: {e}\n")),
            }
        }
        Err(e) => out.log.push_str(&format!("  consecrate: could not capture it: {e}\n")),
    }

    // 3. Press it. The screen going away is reported, but it is **not** the success signal — the
    //    stats history page closes it too. `Run::confirm_consecrated` reads the save flag.
    let (cx, cy) = crate::act::SHRINE_CONSECRATE.click;
    input.click(cx, cy)?;
    out.done = crate::act::wait_until_gone(
        win,
        &crate::act::SHRINE_GOBACK,
        crate::act::SHRINE_GOBACK_PRESENT,
        Duration::from_secs(10),
    );
    out.log.push_str(&format!("  consecrate: shrine screen closed={}\n", out.done));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shrine::show;

    #[test]
    fn the_four_letter_layout_matches_what_was_measured_live() {
        // The one layout a live run has actually seen: a four-letter shrine at 1920x1080, whose grid
        // was sampled at these exact centres and hit every tile.
        let l = Layout::new(4, 1920, 1080);
        assert_eq!(l.max_guesses, 8, "ceil(30/4) = 8, inside the 6..10 clamp");
        assert_eq!(l.tile, 89, "floor(118*0.75+0.5)");
        assert_eq!(l.origin, (782, 22));
        assert_eq!((l.width(), l.height()), (356, 712));
        assert_eq!(l.tile_center(1, 1), (826, 66));
        assert_eq!(l.tile_center(1, 2), (915, 66));
        assert_eq!(l.tile_center(1, 4), (1093, 66));
        assert_eq!(l.tile_center(8, 1), (826, 689));
    }

    #[test]
    fn only_the_four_letter_grid_is_scaled_down() {
        // maxGuesses is clamped at 6, so lengths 5, 6 and 7 all keep full-size tiles and differ only
        // in width. Length 4 is the sole case where ceil(30/n) exceeds 6.
        for (len, guesses) in [(5, 6), (6, 6), (7, 6)] {
            let l = Layout::new(len, 1920, 1080);
            assert_eq!((l.max_guesses, l.tile), (guesses, 118), "length {len}");
            assert_eq!(l.height(), 708);
        }
        assert_eq!(Layout::new(4, 1920, 1080).tile, 89);
    }

    #[test]
    fn the_block_stays_centred_on_the_anchor() {
        for len in MIN_LENGTH..=MAX_LENGTH {
            let l = Layout::new(len, 1920, 1080);
            assert_eq!(l.origin.0 + l.width() / 2, 960, "length {len} horizontally centred");
            assert_eq!(l.origin.1 + l.height() / 2, 378, "length {len} at 0.35 of the height");
        }
    }

    #[test]
    fn a_single_tile_identifies_the_word_length() {
        // The inverse of Layout: given the first tile's box, recover the length. Every layout must
        // round-trip, or the detector would pick the wrong grid and read the wrong pixels.
        for len in MIN_LENGTH..=MAX_LENGTH {
            let l = Layout::new(len, 1920, 1080);
            assert_eq!(length_from_tile(l.origin.0, l.tile, 1920), Some(len), "length {len}");
        }
    }

    #[test]
    fn a_tile_box_that_fits_no_layout_is_rejected() {
        // A misread bounding box must not be rounded into a plausible answer -- reading a 5-letter
        // grid as 6 would sample the gaps between tiles and classify nothing.
        assert_eq!(length_from_tile(960, 89, 1920), None, "zero span");
        assert_eq!(length_from_tile(200, 89, 1920), None, "span implies 17 letters");
        assert_eq!(length_from_tile(782, 0, 1920), None, "no tile at all");
    }

    // Every sample below was measured off a live shrine at 1920x1080 -- the four-letter Swanland
    // shrine, whose answer was TAXI -- rather than reasoned about. The letter is named because glyph
    // coverage is what moves these numbers around.
    #[test]
    fn the_live_colour_samples_classify_correctly() {
        assert_eq!(classify((34, 124, 34)), Some(Colour::Green), "green under 'a'");
        assert_eq!(classify((25, 141, 25)), Some(Colour::Green), "green under 't', a thin glyph");
        assert_eq!(classify((146, 124, 34)), Some(Colour::Yellow), "yellow under 'a'");
        assert_eq!(classify((175, 143, 16)), Some(Colour::Yellow), "yellow under 'i', thinner still");
        for grey in [(82, 82, 82), (77, 77, 77), (65, 65, 65), (83, 82, 83), (70, 70, 70)] {
            assert_eq!(classify(grey), Some(Colour::Grey), "grey {grey:?}");
        }
    }

    #[test]
    fn an_empty_tile_is_not_a_colour_whatever_is_behind_it() {
        // THE case this classifier exists for. Grass is green-ish and would beat every other
        // reference on a nearest-neighbour test; pale sky is neutral and would read as grey.
        assert_eq!(classify((130, 155, 82)), None, "empty over grass -- looks green, is not");
        assert_eq!(classify((126, 152, 84)), None, "empty over grass, another column");
        assert_eq!(classify((152, 172, 163)), None, "empty over distant hills");
        assert_eq!(classify((206, 215, 197)), None, "empty over bright sky -- neutral but not dark");
        assert_eq!(classify((174, 190, 176)), None, "empty over pale sky");
        assert_eq!(classify((224, 229, 206)), None, "the brightest empty tile measured");
    }

    #[test]
    fn the_thin_glyph_yellow_would_have_broken_a_distance_cut() {
        // Recorded as a test because it is the measurement that changed the design: 'i' on yellow sat
        // 39 units from the 'a'-derived reference, and the distance cut under consideration was 40.
        let a_yellow = (146i32, 124i32, 34i32);
        let i_yellow = (175i32, 143i32, 16i32);
        let d = (((a_yellow.0 - i_yellow.0).pow(2)
            + (a_yellow.1 - i_yellow.1).pow(2)
            + (a_yellow.2 - i_yellow.2).pow(2)) as f64)
            .sqrt();
        assert!(d > 35.0, "two real yellows are {d:.1} apart, so absolute distance is the wrong tool");
        assert_eq!(classify((146, 124, 34)), classify((175, 143, 16)));
    }

    fn frame_of(pixels: &[(i32, i32, (u8, u8, u8))], w: i32, h: i32) -> Frame {
        // Background is a bright neutral, i.e. an unsubmitted tile, so anything not painted reads as
        // "not coloured yet" rather than accidentally as grey.
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        for i in (0..bgra.len()).step_by(4) {
            bgra[i] = 200;
            bgra[i + 1] = 210;
            bgra[i + 2] = 200;
        }
        for &(x, y, (r, g, b)) in pixels {
            let i = ((y as usize) * (w as usize) + x as usize) * 4;
            bgra[i] = b;
            bgra[i + 1] = g;
            bgra[i + 2] = r;
        }
        Frame { width: w, height: h, bgra }
    }

    /// Paints every pixel of the tile at `(row, col)` a flat colour.
    fn paint(px: &mut Vec<(i32, i32, (u8, u8, u8))>, l: &Layout, row: usize, col: usize, c: (u8, u8, u8)) {
        let (cx, cy) = l.tile_center(row, col);
        let r = l.tile / 2;
        for dy in -r..=r {
            for dx in -r..=r {
                px.push((cx + dx, cy + dy, c));
            }
        }
    }

    #[test]
    fn a_painted_row_reads_back_as_the_pattern_that_was_painted() {
        let l = Layout::new(4, 1920, 1080);
        let (green, grey) = ((34, 124, 34), (75, 75, 75));
        let mut px = Vec::new();
        // The real first guess from the live run: lare -> .G..
        paint(&mut px, &l, 1, 1, grey);
        paint(&mut px, &l, 1, 2, green);
        paint(&mut px, &l, 1, 3, grey);
        paint(&mut px, &l, 1, 4, grey);
        let frame = frame_of(&px, 1920, 1080);
        let pattern = read_row(&frame, &l, 1).expect("a fully painted row reads");
        assert_eq!(show(pattern, 4), ".G..");
    }

    #[test]
    fn the_pattern_is_read_left_to_right() {
        // Base-3 with position 0 least significant is easy to get backwards, and a mirrored pattern
        // is a plausible-looking colouring that eliminates the wrong candidates.
        let l = Layout::new(4, 1920, 1080);
        let (green, yellow, grey) = ((34, 124, 34), (146, 124, 34), (75, 75, 75));
        let mut px = Vec::new();
        paint(&mut px, &l, 3, 1, green);
        paint(&mut px, &l, 3, 2, yellow);
        paint(&mut px, &l, 3, 3, grey);
        paint(&mut px, &l, 3, 4, grey);
        let frame = frame_of(&px, 1920, 1080);
        // tins -> GY.., the live third guess.
        assert_eq!(show(read_row(&frame, &l, 3).unwrap(), 4), "GY..");
    }

    #[test]
    fn a_row_that_has_not_coloured_yet_reads_as_nothing() {
        // The readiness check. An uncoloured row must not read as a pattern of greys, or the solver
        // would be handed a colouring the game never showed and would eliminate the true answer.
        let l = Layout::new(4, 1920, 1080);
        let frame = frame_of(&[], 1920, 1080);
        assert_eq!(read_row(&frame, &l, 1), None);
    }

    #[test]
    fn a_half_coloured_row_reads_as_nothing() {
        // Mid-animation, some tiles have turned and some have not. Reading that would be worse than
        // reading nothing, so partial rows are refused rather than filled in.
        let l = Layout::new(4, 1920, 1080);
        let mut px = Vec::new();
        paint(&mut px, &l, 1, 1, (75, 75, 75));
        paint(&mut px, &l, 1, 2, (34, 124, 34));
        let frame = frame_of(&px, 1920, 1080);
        assert_eq!(read_row(&frame, &l, 1), None);
    }

    #[test]
    fn the_four_candidate_first_tiles_do_not_overlap() {
        // The premise of `infer_length`: one keystroke can only distinguish the grids if each
        // candidate's sampling box sees pixels no other candidate sees.
        let mut spans: Vec<(usize, i32, i32)> = Vec::new();
        for len in MIN_LENGTH..=MAX_LENGTH {
            let l = Layout::new(len, 1920, 1080);
            let (cx, _) = l.tile_center(1, 1);
            let r = (l.tile / 4).max(1);
            spans.push((len, cx - r, cx + r));
        }
        for (i, &(la, a0, a1)) in spans.iter().enumerate() {
            for &(lb, b0, b1) in &spans[i + 1..] {
                assert!(a1 < b0 || b1 < a0, "length {la} box {a0}..{a1} overlaps {lb} box {b0}..{b1}");
            }
        }
    }

    #[test]
    fn one_typed_letter_identifies_the_grid() {
        for len in MIN_LENGTH..=MAX_LENGTH {
            let l = Layout::new(len, 1920, 1080);
            let before = frame_of(&[], 1920, 1080);
            // A letter appearing darkens the middle of exactly one tile.
            let mut px = Vec::new();
            paint(&mut px, &l, 1, 1, (20, 20, 20));
            let after = frame_of(&px, 1920, 1080);
            assert_eq!(infer_length(&before, &after, 1920, 1080), Some(len), "length {len}");
        }
    }

    #[test]
    fn a_screen_that_merely_animated_identifies_nothing() {
        // The failure this must not have: cloud drift and an idling character move every candidate a
        // little, and picking the argmax of noise would commit the whole puzzle to the wrong grid.
        let before = frame_of(&[], 1920, 1080);
        let mut px = Vec::new();
        for x in (0..1920).step_by(3) {
            for y in (0..200).step_by(3) {
                px.push((x, y, (203, 213, 203)));
            }
        }
        let after = frame_of(&px, 1920, 1080);
        assert_eq!(infer_length(&before, &after, 1920, 1080), None, "a uniform 3/255 shift is not a letter");
    }

    #[test]
    fn a_row_off_the_bottom_of_the_frame_is_refused_rather_than_wrapped() {
        // Guards the indexing arithmetic: a row index past the grid must not sample some other part
        // of the screen and return a confident answer.
        let l = Layout::new(4, 1920, 1080);
        let frame = frame_of(&[], 1920, 1080);
        assert_eq!(read_row(&frame, &l, 40), None);
    }
}
