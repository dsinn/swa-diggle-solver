//! Threshold regression tests, run against saved frames rather than a live game.
//!
//! These pin the numbers that the doc comments in [`buttons`](super::buttons) quote. Both
//! thresholds were once set from negative controls that happened to be dark screens, and both
//! turned out to admit plain brown map ground — a failure invisible until something scored the
//! templates against *every* frame on disk instead of the handful that motivated them.
//!
//! The frames are gitignored (`/spike-frames*/`), so these skip where they are absent, as the
//! game-source tests do. The templates themselves are tracked, so a template edit still gets checked
//! wherever a corpus exists.
//!
//! Split out of `act.rs` on 2026-08-21 (#76), because a corpus regression suite outgrew the
//! machinery it checks. `super` is still `act`, so every name these reach for resolves
//! exactly as it did inline.

use super::*;
use crate::win::capture::Frame;

/// Loads a corpus frame.
///
/// `tests/frames`, **not** `spike-frames-live`. These tests used to read the live spike output
/// directory, which no one had decided on — the captures happened to be there and the tests
/// reached for them. Runs write that directory under fixed names, so a run can overwrite the
/// evidence a threshold test rests on, and one did: a stall inside the anomaly replaced
/// `combat-stalled.png` with a `PlayerTurn` frame containing no `Finish` at all, and the test
/// failed on a change that had nothing to do with it.
///
/// The corpus is a fixture, so it lives with the tests and is tracked. Frames are captured once
/// and copied in deliberately; nothing the driver does at runtime can reach them.
fn frame(name: &str) -> Option<Frame> {
    let path = PathBuf::from("tests").join("frames").join(name);
    let dec = png::Decoder::new(std::fs::File::open(path).ok()?);
    let mut rdr = dec.read_info().ok()?;
    let mut buf = vec![0; rdr.output_buffer_size()];
    let info = rdr.next_frame(&mut buf).ok()?;
    let n = info.color_type.samples();
    // The same BGRA layout `capture_window` produces, so this exercises the live comparison.
    let mut bgra = Vec::with_capacity((info.width * info.height * 4) as usize);
    for px in buf.chunks_exact(n) {
        bgra.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    Some(Frame { width: info.width as i32, height: info.height as i32, bgra })
}

/// Crops a frame to a client rectangle, standing in for `capture_client_rect`.
fn crop(f: &Frame, (x0, y0, x1, y1): (i32, i32, i32, i32)) -> Frame {
    let (w, h) = (x1 - x0, y1 - y0);
    let mut bgra = Vec::with_capacity((w * h * 4) as usize);
    for y in y0..y1 {
        let row = (y * f.width + x0) as usize * 4;
        bgra.extend_from_slice(&f.bgra[row..row + (w as usize) * 4]);
    }
    Frame { width: w, height: h, bgra }
}

/// Scores a button's template against a saved frame **the way [`locate`] does**: capture only
/// the search box, then search the template inside that crop with no further bounds.
///
/// Deliberately not `find_at_scale_in(whole_frame, .., Some(button.search))`. That is what
/// `diggle findpng` does, and it treats `search` as an anchor box rather than a capture
/// rectangle — the exact mismatch that let a 16x16 search box on a 300x100 template measure
/// perfectly offline while never once matching in the live run. A regression test that measures
/// through the wrong path is worse than none, because it certifies the bug.
fn score(button: &Button, name: &str) -> Option<f64> {
    let f = frame(name)?;
    let tpl = Template::load(&PathBuf::from("templates").join(button.template)).ok()?;
    find_at_scale_in(&crop(&f, button.search), &tpl, 1.0, 1, None).map(|m| m.inliers)
}

/// Scores a button the way [`identify`] does: the template-sized rect at `origin`, no sweep.
///
/// The two paths are not interchangeable, and reporting one while the run uses the other is how
/// a regression test certifies a bug — see [`score`]'s note. `identify` calls [`score_exact`] for
/// every screen it names, so this is the measurement that speaks to what a run saw.
fn score_at_origin(button: &Button, name: &str) -> Option<f64> {
    let f = frame(name)?;
    let tpl = Template::load(&PathBuf::from("templates").join(button.template)).ok()?;
    let (ox, oy) = button.origin;
    let rect = (ox, oy, ox + tpl.width as i32, oy + tpl.height as i32);
    find_at_scale_in(&crop(&f, rect), &tpl, 1.0, 1, None).map(|m| m.inliers)
}

/// Measures the frame a run stopped on with `Start` plainly on screen.
///
/// 2026-08-14 at `l16sub14`: the run pressed Combat, the screen moved 0.975, and the next look
/// did not report `Screen::Pregame`. It re-derived the same step, pressed the same coordinate a
/// second time into a screen where it means nothing (0.011 of movement), and stopped with
/// `Combat did not open`. `spike-frames-live/gave-up.png` is the pregame, copied here.
///
/// This is here to say which half is at fault, because an absent fingerprint and a fingerprint
/// nobody asked about produce the same log line and want opposite fixes.
#[test]
fn the_pregame_a_run_stopped_on_is_recognisable() {
    let Some(exact) = score_at_origin(&PREGAME_START, "pregame-graveyard.png") else {
        eprintln!("SKIP: frame corpus not present");
        return;
    };
    // Both paths, because they answer different questions and only one is `identify`'s.
    let searched = score(&PREGAME_START, "pregame-graveyard.png").unwrap();
    assert!(
        exact >= PREGAME_START_PRESENT,
        "a resting Start scored {exact:.4} at its origin (searched: {searched:.4}), under the \
         {PREGAME_START_PRESENT:.2} bar"
    );
    eprintln!("PREGAME_START on the give-up frame: exact {exact:.4}, searched {searched:.4}");
    if let Some(e2) = score_at_origin(&PREGAME_START, "pregame-graveyard-2.png") {
        let s2 = score(&PREGAME_START, "pregame-graveyard-2.png").unwrap();
        eprintln!("PREGAME_START on the second give-up frame: exact {e2:.4}, searched {s2:.4}");
    }
    // The resting template must NOT claim a hot button — if it did, the pair would be
    // indistinguishable and the second template pointless.
    for f in ["pregame-hot.png", "pregame-hot-2.png"] {
        let cold = score_at_origin(&PREGAME_START, f).unwrap();
        assert!(
            cold < PREGAME_START_PRESENT,
            "{f}: resting template scored {cold:.4} on a hot button"
        );
    }
}

/// The highlighted `Start`, on the two frames a run photographed itself failing to read.
///
/// Both are pregames the run could not name — `l14` and `l48_plaza`, 2026-08-14 — and on both
/// the resting template scored 0.4972. That number is the whole argument for a second template
/// rather than a lower bar: it sits *under* impostors the same frames carry, so no threshold
/// separates a hot pregame from a screen that is not a pregame.
#[test]
fn a_highlighted_start_is_still_the_pregame() {
    let Some(hot) = score_at_origin(&PREGAME_START_HOT, "pregame-hot.png") else {
        eprintln!("SKIP: frame corpus not present");
        return;
    };
    assert!(hot >= PREGAME_START_PRESENT, "the frame it was cut from scored {hot:.4}");

    // The one that matters: a *different* pregame, at a different location, with a different
    // backdrop. Scoring 1.0000 against its own source frame would prove only that cropping works.
    let other = score_at_origin(&PREGAME_START_HOT, "pregame-hot-2.png").unwrap();
    assert!(
        other >= PREGAME_START_PRESENT,
        "a second hot pregame scored {other:.4}, under the {PREGAME_START_PRESENT:.2} bar"
    );

    // And the hot template must not claim a resting button, or the two would be one.
    for f in ["pregame-graveyard.png", "pregame-graveyard-2.png"] {
        let cold = score_at_origin(&PREGAME_START_HOT, f).unwrap();
        assert!(
            cold < PREGAME_START_PRESENT,
            "{f}: hot template scored {cold:.4} on a resting button"
        );
    }
    eprintln!("PREGAME_START_HOT: source {hot:.4}, independent {other:.4}");
}

#[test]
fn finish_is_told_apart_from_another_plank_and_from_bare_ground() {
    let Some(real) = score(&COMBAT_FINISH, "now.png") else {
        eprintln!("SKIP: frame corpus not present");
        return;
    };
    // Two independent captures two days apart, both exact. The threshold sits under these.
    assert!(real >= COMBAT_FINISH_PRESENT, "a real Finish scored {real:.4}");
    // `combat-stalled.png` was the second of those two captures and is **gone**: it lived in the
    // live spike directory, and a stall inside the anomaly overwrote it with a `PlayerTurn`
    // frame that contains no `Finish` at all. The directory was git-excluded, so there is
    // nothing to restore. Skipped rather than deleted, because the gap is worth seeing — a
    // second independent capture is what made the 1.0000 from the first one meaningful, and this
    // assertion comes back the moment one is captured into `tests/frames`.
    match score(&COMBAT_FINISH, "combat-stalled.png") {
        Some(older) => {
            assert!(older >= COMBAT_FINISH_PRESENT, "an older real Finish scored {older:.4}")
        }
        None => eprintln!("SKIP: combat-stalled.png awaits recapture"),
    }

    // A *differently composed* WaitPhase: `Fight on!` beside it, a lit brazier and an open chest
    // in the scene. Worth its own assertion because the other two scoring exactly 1.0000 was
    // weak evidence — a template scores 1.0000 against the rendering it was cropped from, which
    // says nothing about how the button looks when the screen around it differs. This is the
    // frame the run was actually sitting on when the check failed.
    let composed = score(&COMBAT_FINISH, "waitphase-with-fighton.png").unwrap();
    assert!(
        composed >= COMBAT_FINISH_PRESENT,
        "a live WaitPhase with Fight on! beside it scored {composed:.4}"
    );

    // The nearest confusable available: a wooden plank button reading `Adventure!`. `Give up` is
    // the *same button object* as Finish (`rpg.lua:592-597`) so it would score at least this
    // well, and pressing it eulogises the run.
    let other_plank = score(&COMBAT_FINISH, "16-selected.png").unwrap();
    assert!(
        other_plank < COMBAT_FINISH_PRESENT,
        "a different word on a plank scored {other_plank:.4}, at or above the threshold"
    );

    // Blank brown map, no button in the slot at all.
    let ground = score(&COMBAT_FINISH, "overworld-campfire.png").unwrap();
    assert!(ground < COMBAT_FINISH_PRESENT, "bare map ground scored {ground:.4}");
}

/// The confusable that mattered all along, finally measured rather than estimated.
///
/// `Eulogise` occupies the identical slot and plank as `Finish`; only the word differs. A single
/// template cannot separate them — 0.8527 for the word swap against 0.8639 for `Finish` merely
/// going greyed — so the test is that **argmax over the two templates** does.
#[test]
fn finish_and_eulogise_are_told_apart_by_comparing_both_templates() {
    let Some(f_on_finish) = score(&COMBAT_FINISH, "now.png") else {
        eprintln!("SKIP: frame corpus not present");
        return;
    };
    let e_on_finish = score(&COMBAT_EULOGISE, "now.png").unwrap();
    let f_on_death = score(&COMBAT_FINISH, "eulogise-at-death.png").unwrap();
    let e_on_death = score(&COMBAT_EULOGISE, "eulogise-at-death.png").unwrap();

    assert!(f_on_finish > e_on_finish, "{f_on_finish:.4} vs {e_on_finish:.4} on a Finish frame");
    assert!(e_on_death > f_on_death, "{e_on_death:.4} vs {f_on_death:.4} on the death screen");

    // Both margins comfortably wider than anything a single cut point could offer.
    let margin = (f_on_finish - e_on_finish).min(e_on_death - f_on_death);
    assert!(margin > 0.10, "argmax margin only {margin:.4}");

    // And the death screen must not clear the Finish bars at all.
    assert!(f_on_death < COMBAT_FINISH_PRESENT, "Eulogise scored {f_on_death:.4} as Finish");
    assert!(f_on_death < COMBAT_FINISH_ACTIVE);
}

#[test]
fn a_reward_screen_is_told_apart_from_bare_ground() {
    let Some(real) = score(&REWARD_CONFIRM, "post-crypt.png") else {
        eprintln!("SKIP: frame corpus not present");
        return;
    };
    assert!(real >= REWARD_SCREEN_PRESENT, "a real greyed Confirm scored {real:.4}");
    let ground = score(&REWARD_CONFIRM, "overworld-campfire.png").unwrap();
    assert!(ground < REWARD_SCREEN_PRESENT, "bare map ground scored {ground:.4}");
}

/// The combat HUD separates "in a fight" from every non-combat screen in the corpus, and does it
/// on the frame where every *other* fingerprint had failed.
///
/// `combat-turn1-hurt.png` is that frame: the run that entered combat from an overworld event and
/// never noticed, at `health = 1` of 20, with the hurt vignette at 1.4 of 1.5. Six buttons
/// sharing the affirmative slot were unreadable on it. This corner was not.
#[test]
fn the_combat_hud_is_told_apart_from_every_screen_that_is_not_a_fight() {
    let Some(hurt) = score(&COMBAT_HUD, "combat-turn1-hurt.png") else {
        eprintln!("SKIP: frame corpus not present");
        return;
    };
    assert!(
        hurt >= COMBAT_HUD_PRESENT,
        "turn 1 under a full-strength vignette scored {hurt:.4}, below the threshold — the one \
         state this fingerprint exists for"
    );

    // The gap is the finding, so it is asserted rather than trusted. Every one of these is a
    // screen the navigator can legitimately be sitting on, and none may read as a fight.
    for name in
        ["16-selected.png", "post-crypt.png", "reward-selected.png", "overworld-campfire.png"]
    {
        let q = score(&COMBAT_HUD, name).unwrap();
        assert!(q < COMBAT_HUD_PRESENT, "{name} scored {q:.4}, at or above the threshold");
        // Not merely under the bar — nowhere near it. A non-combat screen creeping up towards
        // 0.99 would mean the crop had started matching wood grain rather than the HUD.
        assert!(q < 0.75, "{name} scored {q:.4}, close enough to the bar to be worth re-measuring");
    }
}

/// The HUD crop is **biome-independent**, which is what makes it a screen fingerprint rather
/// than a picture of one fight.
///
/// `combat-turn1-hurt.png` is a crypt at 1/20 under a full vignette — the frame the template was
/// cut from. `combat-forest-turn1.png` is a spider forest, a different parallax, different
/// lighting, red vines across the backdrop, a different enemy. Both score 1.0000, so the crop
/// carries only the `?` button and the turn plaque and none of the scene behind them.
///
/// Worth pinning because 0.99 is the tightest bar in this module, and a crop that had picked up
/// any backdrop would pass on its own biome and fail everywhere else — which would look exactly
/// like the bug this frame came from, and is not it.
#[test]
fn the_combat_hud_reads_the_same_in_a_different_biome() {
    let Some(forest) = score(&COMBAT_HUD, "combat-forest-turn1.png") else {
        eprintln!("SKIP: frame corpus not present");
        return;
    };
    let crypt = score(&COMBAT_HUD, "combat-turn1-hurt.png").unwrap_or(0.0);
    assert!(forest >= COMBAT_HUD_PRESENT, "a forest fight scored {forest:.4}");
    assert!(crypt >= COMBAT_HUD_PRESENT, "the crypt frame scored {crypt:.4}");

    // And nothing else claims that frame, so `identify` reaching it returns `CombatEntered`.
    assert!(
        score(&COMBAT_FINISH, "combat-forest-turn1.png").unwrap_or(0.0) < COMBAT_FINISH_PRESENT
    );
    assert!(
        score(&CHARACTER_STATS, "combat-forest-turn1.png").unwrap_or(0.0) < CHARACTER_STATS_PRESENT
    );
}

/// The class-unlock screen must not read as hero select. **Both layers are asserted.**
///
/// Met live: a road event unlocked the Woodsman mid-run, and every run that day reported
/// `screen: HeroSelect` and then failed hunting for a map that was not there.
#[test]
fn the_unlock_screen_is_not_hero_select() {
    let Some(unlock) = score(&UNLOCK_CONTINUE, "unlock-woodsman.png") else {
        eprintln!("SKIP: frame corpus not present");
        return;
    };
    let hero = score(&HEROSELECT_HEADER, "unlock-woodsman.png").unwrap_or(0.0);
    eprintln!("  unlock {unlock:.4} / hero-select heading {hero:.4}");

    // Layer one: the unlock's own button is exact.
    assert!(unlock >= UNLOCK_CONTINUE_PRESENT, "the real Continue scored {unlock:.4}");

    // Layer two: the heading no longer clears its bar on this frame. It DID at the old 0.80 --
    // 0.8532 -- which is why the bar moved. Asserted from both sides so neither the raise nor the
    // measurement can be quietly undone.
    assert!(hero > 0.80, "the impostor that forced the raise scored {hero:.4}, once above 0.80");
    assert!(hero < HEROSELECT_HEADER_PRESENT, "still reads as hero select at {hero:.4}");

    // Layer three, and the one that survives an impostor we have not met yet: ordering. Asserted
    // on explicit booleans rather than on these scores, because the point is what `identify` does
    // when BOTH fire -- which is the situation a louder future impostor puts us in.
    assert_eq!(
        super::identify_from_scores(true, true),
        Screen::Unlock,
        "with both firing, the more specific fingerprint has to win"
    );
    assert_eq!(super::identify_from_scores(false, true), Screen::HeroSelect);
}

/// The event plaque separates "an event is still up" from every screen that has no event on it.
#[test]
fn the_event_plaque_is_told_apart_from_every_screen_without_one() {
    // Scored through the computed band, the way live code does it -- two options on that frame.
    let scan = |name: &str, options: usize| -> Option<f64> {
        let f = frame(name)?;
        let tpl = Template::load(&PathBuf::from("templates").join(EVENT_CHOICE.template)).ok()?;
        find_at_scale_in(&crop(&f, event_choice_search(options)), &tpl, 1.0, 1, None)
            .map(|m| m.inliers)
    };
    let Some(real) = scan("event-woodsman.png", 2) else {
        eprintln!("SKIP: frame corpus not present");
        return;
    };
    assert!(real >= EVENT_CHOICE_PRESENT, "the Woodsman's own event scored {real:.4}");
    eprintln!("  event-woodsman.png (n=2): {real:.4}");
    // The count has to be right for the band to land on the plaque. A wrong `n` is a miss, not a
    // near-miss, and that is worth pinning: it is what makes the console's choice list load-
    // bearing rather than incidental.
    for n in [1, 3, 4] {
        let q = scan("event-woodsman.png", n).unwrap_or(0.0);
        assert!(q < EVENT_CHOICE_PRESENT, "the right plaque at the wrong n={n} scored {q:.4}");
    }

    // `overworld-campfire.png` is the one that matters. The map's area button is wood too, so a
    // bare-wood template is exactly the crop that could match it -- and would then report an
    // event on every overworld screen, freezing the run for ever. 400 px is wider than the 250 px
    // `default` button, which is what makes that impossible rather than merely unlikely.
    for name in
        ["overworld-campfire.png", "post-crypt.png", "combat-turn1-hurt.png", "16-selected.png"]
    {
        // Every option count the band can take, since a run asks this question with whatever
        // count the console last reported -- including on a screen that has no event at all.
        for n in 1..=4 {
            let q = scan(name, n).unwrap_or(0.0);
            assert!(
                q < EVENT_CHOICE_PRESENT,
                "{name} scored {q:.4} at n={n}, at or above the threshold"
            );
            eprintln!("  {name} (n={n}): {q:.4}");
        }
    }
}

/// **This crop is no longer turn-specific, and that is now the point.**
///
/// It used to assert the opposite — that a later turn scores *below* [`COMBAT_HUD_PRESENT`],
/// because the template carries the numeral `1` and the check was meant as an entry signal. That
/// held at a 0.99 bar, and the old assertion's own message said what to do if the numeral
/// stopped mattering: say so here rather than leave the doc claiming it does. This is that.
///
/// What changed: a live fight at turn 1 scored **0.9820** and missed the 0.99 bar, so `identify`
/// returned `Unknown` and the run stood on the map path in front of a full board until it gave
/// up. The bar is [`MAX_PRESENT`] now, and a two-character numeral is too small a part of a
/// 225x100 region to hold turn 11 below it.
///
/// So the contract is "a fight is on screen", not "a fight just started" — which is the more
/// useful question anyway. The **gap to the nearest non-combat frame** is what the check really
/// lives on, and if that ever closes the answer is a crop without the numeral, never a lower
/// threshold.
#[test]
fn any_turn_reads_as_combat_and_stands_clear_of_what_is_not() {
    let Some(turn_11) = score(&COMBAT_HUD, "combat-chest.png") else {
        eprintln!("SKIP: frame corpus not present");
        return;
    };
    let turn_1 = score(&COMBAT_HUD, "combat-turn1-hurt.png").unwrap();
    for (name, q) in [("turn 1", turn_1), ("turn 11", turn_11)] {
        assert!(
            q >= COMBAT_HUD_PRESENT,
            "{name} scored {q:.4}, below the bar — a fight this misses is a fight the run walks \
             past, which is exactly what 0.99 did on 2026-08-09"
        );
    }
    let nearest_non_combat = score(&COMBAT_HUD, "16-selected.png").unwrap();
    assert!(
        turn_11 - nearest_non_combat > 0.3,
        "a later turn ({turn_11:.4}) should stand well clear of the nearest non-combat screen \
         ({nearest_non_combat:.4}); that margin is the whole of this check"
    );
}

/// **No threshold may be tighter than the frame variation nobody has measured.**
///
/// The rule behind [`MAX_PRESENT`], enforced rather than trusted. Two bars in this file were set
/// near 1.0 with a note that exact bounds made it free, and the tighter of them missed a live
/// fight by 0.008 — a run stood in front of a full board reporting a navigation error.
///
/// A cap is not a substitute for measurement, and this test does not pretend otherwise: the
/// per-button docs carry the true positive and the nearest confusable, and those gaps are what
/// make a threshold correct. This only guarantees that whatever is chosen leaves at least 0.05
/// of room for a background, an animation or a biome we have not photographed.
///
/// The list is written out because there is no way to enumerate consts, which is the same
/// weakness [`ALL`] has — a threshold added and not listed here is checked by nothing. Add it.
#[test]
fn every_threshold_leaves_room_for_a_frame_we_have_not_seen() {
    let bars = [
        ("COMBAT_FINISH", COMBAT_FINISH_PRESENT),
        ("COMBAT_FINISH_ACTIVE", COMBAT_FINISH_ACTIVE),
        ("COMBAT_HUD", COMBAT_HUD_PRESENT),
        ("COMBAT_EULOGISE", COMBAT_EULOGISE_PRESENT),
        ("REWARD_SCREEN", REWARD_SCREEN_PRESENT),
        ("MENU_START", MENU_START_PRESENT),
        ("HEROSELECT_CONFIRM", HEROSELECT_CONFIRM_PRESENT),
        ("POSTGAME_CONTINUE", POSTGAME_CONTINUE_PRESENT),
        ("CHARACTER_STATS", CHARACTER_STATS_PRESENT),
        ("CHARACTER_BACK", CHARACTER_BACK_PRESENT),
        ("PREGAME_START", PREGAME_START_PRESENT),
        ("EVENT_CHOICE", EVENT_CHOICE_PRESENT),
        ("UNLOCK_CONTINUE", UNLOCK_CONTINUE_PRESENT),
        ("HEROSELECT_HEADER", HEROSELECT_HEADER_PRESENT),
        ("SHRINE_GOBACK", SHRINE_GOBACK_PRESENT),
        ("STATS_BACK", STATS_BACK_PRESENT),
        ("SHRINE_PRAY", SHRINE_PRAY_PRESENT),
        ("CONTINUE", CONTINUE_PRESENT),
    ];
    for (name, bar) in bars {
        assert!(
            bar <= MAX_PRESENT,
            "{name}_PRESENT is {bar:.2}, above the {MAX_PRESENT:.2} cap. A bar that tight \
             asserts a region is pixel-identical across frames nobody has sampled; if the \
             separation genuinely needs it, the crop is wrong, not the cap"
        );
    }
}

#[test]
fn praying_cannot_be_fooled_by_the_button_it_replaces() {
    // The cap above is the ceiling; this is the floor, and it is the one that has teeth.
    //
    // `shrineplay::claim_blessing` presses `Pray` on this threshold alone, with no second
    // opinion, and the rect it reads is shared with `Consecrate`. The dangerous neighbour is not
    // an *active* Consecrate — that state means the blessing is genuinely not claimable yet, and
    // pressing it there would spend the solve early — it is the greyed one, measured live at
    // `shrine1` on 2026-08-15 while the word was still unsolved.
    //
    // Lowering the bar past that figure does not merely make a match noisier: it makes "the
    // blessing is ready" indistinguishable from "the word has not been solved", which is the
    // pair this whole slot exists to separate.
    const GREYED_CONSECRATE: f64 = 0.8564;
    assert!(
        SHRINE_PRAY_PRESENT > GREYED_CONSECRATE,
        "SHRINE_PRAY_PRESENT is {SHRINE_PRAY_PRESENT:.4}, at or below the {GREYED_CONSECRATE:.4} \
         a greyed `Consecrate` scores against Pray's artwork — an unsolved shrine would read as \
         a claimable blessing"
    );
}

/// **A button under the pointer is not the button we have a template for.**
///
/// `ui/elements/button.lua:122,159` draws `<img>-up-hover.jpg` over `<img>-up.jpg` at
/// `hover_alpha`, and `hover` is set purely from `mousemoved` (`:223-269`) — so a plaque keeps
/// its hover artwork for as long as the pointer sits inside it, with nothing to time out. Every
/// template in this file was cut with the pointer somewhere else.
///
/// `inn-rest-hovered.png` is the live frame from 2026-08-15 at The Quacking Duck, captured after
/// the run had clicked `Rest` and left the pointer on it. The plaque is plainly drawn, at the
/// coordinate the arithmetic predicts, and it scores **0.5452** — so far under [`MIN_INLIERS`]
/// that no threshold could split the two. The run read that as "the inn is not there yet", spent
/// [`crate::innplay::REST_TRIES`] × 1.5s hunting it, and then `leave_inn`'s presence check —
/// the same `locate` — concluded it was already out of the inn while standing in it.
///
/// The fix is not a looser bar or a second template: it is to move the pointer off the artwork
/// before reading it, which [`crate::navigate::Run::park`] already exists to do. This test is
/// what says the hazard is real, and it is the reason that call is not optional.
#[test]
fn a_hovered_plaque_does_not_match_its_own_template() {
    let Some(hovered) = score(&INN_REST, "inn-rest-hovered.png") else {
        eprintln!("SKIP: frame corpus not present");
        return;
    };
    assert!(
        hovered < MIN_INLIERS,
        "`inn Rest` scores {hovered:.4} on the hovered frame, at or above MIN_INLIERS \
         {MIN_INLIERS:.4}. If this now passes, the hover artwork or the metric changed — and \
         the parking in `rest_at_inn` was justified by this measurement."
    );
}

/// The back plaque is one button wearing several screens, and that is what makes it the exit.
///
/// [`SHRINE_GOBACK`]'s template was cut from a **shrine**. This scores it against an **inn**,
/// through [`score_at_origin`] because that is the path `identify` and `click_when_ready` take.
/// `ui/inn.lua:68-71` and `ui/rest.lua:517-520` declare the same `small` button at `ss(0, 0.9)`,
/// `xOffset 1.13`, with the same `back.png` icon, and the plaque is opaque — so the two rooms
/// behind them never reach the pixels.
///
/// This is the measurement [`crate::navigate::Run::back_one_screen`] rests on: leaving the inn
/// is keyed on this plaque rather than on `Rest`, because the plaque is not the button we just
/// clicked and so is never the one we are hovering.
#[test]
fn the_back_plaque_is_the_same_button_at_a_shrine_and_at_an_inn() {
    let Some(q) = score_at_origin(&SHRINE_GOBACK, "inn-rest-hovered.png") else {
        eprintln!("SKIP: frame corpus not present");
        return;
    };
    // Held to [`SHRINE_GOBACK_PRESENT`] rather than to the [`MIN_INLIERS`] that `locate` — and
    // so `back_one_screen` — actually gates on. The stricter bar is deliberate: this is a claim
    // that the two screens share one piece of artwork, not merely that they are close enough to
    // pass. Measured 1.0000, so there is nothing being squeezed through here.
    assert!(
        q >= SHRINE_GOBACK_PRESENT,
        "the shrine's back plaque scores {q:.4} on an inn screen, under the \
         {SHRINE_GOBACK_PRESENT:.4} this test holds it to. If this fails the two screens no \
         longer share the artwork, and leaving the inn needs a template of its own."
    );
}

/// Pins the inversion itself, because it is the reason [`REWARD_SCREEN_PRESENT`] cannot simply be
/// tuned down again: bare ground scores **higher** than a real reward screen whose item has been
/// selected. Anyone lowering the threshold to catch the active state will re-admit the map.
#[test]
fn bare_ground_outranks_a_selected_reward_screen() {
    let Some(ground) = score(&REWARD_CONFIRM, "overworld-campfire.png") else {
        eprintln!("SKIP: frame corpus not present");
        return;
    };
    let selected = score(&REWARD_CONFIRM, "reward-selected.png").unwrap();
    assert!(
        ground > selected,
        "expected the inversion to still hold: ground {ground:.4} vs selected {selected:.4}. \
         If this now fails, the template or the metric changed and REWARD_SCREEN_PRESENT can \
         be reconsidered."
    );
}

/// [`AREA_BUTTON_SHOWING`] sits in a gap that was measured, against the states it can be
/// mistaken for.
///
/// Every area button is the same plank at the same coordinate, so background agreement is total
/// and only the lettering separates them — a threshold guessed here would be a threshold picked
/// out of the air. The three confusables are the ones a crossing actually meets: `Explore` (the
/// corrupted forest we are trying to enter), `Visit` (a settlement subnode), and `Combat` greyed
/// out (offered but not pressable).
///
/// The slot captures are 250x100, exactly template-sized, so [`score`] and `score_at_origin`
/// would both be measuring the same single comparison. `crop` is given the whole frame.
#[test]
fn the_combat_plank_is_separable_from_the_planks_it_shares_a_slot_with() {
    let tpl = Template::load(&PathBuf::from("templates").join(AREA_COMBAT.template)).unwrap();
    let against = |name: &str| -> Option<f64> {
        let f = frame(name)?;
        assert_eq!(
            (f.width, f.height),
            (tpl.width as i32, tpl.height as i32),
            "{name} must be a slot-sized capture for this comparison to mean anything"
        );
        find_at_scale_in(&f, &tpl, 1.0, 1, None).map(|m| m.inliers)
    };
    let Some(explore) = against("area-explore.png") else {
        eprintln!("SKIP: frame corpus not present");
        return;
    };
    let visit = against("area-visit.png").unwrap();
    let greyed = against("area-combat-greyed.png").unwrap();
    let worst = explore.max(visit).max(greyed);
    assert!(
        worst < AREA_BUTTON_SHOWING,
        "the nearest confusable must fall under the gate: Explore {explore:.4}, \
         Visit {visit:.4}, greyed Combat {greyed:.4}, gate {AREA_BUTTON_SHOWING}"
    );
    // A margin rather than a bare inequality: a gate resting a thousandth above the loudest
    // wrong answer is a gate that the next capture moves. This one has 0.07 under it.
    assert!(
        AREA_BUTTON_SHOWING - worst > 0.05,
        "the gap is too thin to trust: worst confusable {worst:.4} vs gate {AREA_BUTTON_SHOWING}"
    );
}

/// [`AREA_BUTTON_LIVE`] separates a **pressable** plank from a greyed one, whatever it says.
///
/// The other half of the same measurement, and the one that would have saved the two dead
/// presses of 2026-08-20. `Explore` and `Visit` are live buttons wearing different words and
/// score 0.87 and 0.86; a greyed `Combat` — the same word as the template — scores 0.74. So
/// greying costs more agreement than the lettering does, and one bar can tell live from dead
/// across all three.
///
/// The corpus is three live planks and one greyed one. The live side is much better sampled by
/// the run logs — see [`AREA_BUTTON_LIVE`] for all 298 readings, the worst live one of which is
/// **0.8355**, and for why the caller still may not veto a press on this bar.
///
/// Both populations are asserted, corpus and live, because the gate has to clear both and the
/// live figures are what moved it from 0.80 to 0.79.
#[test]
fn a_greyed_plank_reads_lower_than_any_live_one() {
    let tpl = Template::load(&PathBuf::from("templates").join(AREA_COMBAT.template)).unwrap();
    let against = |name: &str| -> Option<f64> {
        find_at_scale_in(&frame(name)?, &tpl, 1.0, 1, None).map(|m| m.inliers)
    };
    let Some(explore) = against("area-explore.png") else {
        eprintln!("SKIP: frame corpus not present");
        return;
    };
    let visit = against("area-visit.png").unwrap();
    let greyed = against("area-combat-greyed.png").unwrap();

    // The ordering is the claim: every live plank above the bar, the greyed one below it.
    // The template scores 1.0000 against itself and cannot be the minimum, so it is left out.
    let worst_live = explore.min(visit);
    assert!(
        worst_live > AREA_BUTTON_LIVE,
        "a live plank must read as live: Explore {explore:.4}, Visit {visit:.4}, \
         gate {AREA_BUTTON_LIVE}"
    );
    assert!(
        greyed < AREA_BUTTON_LIVE,
        "a greyed plank must not: {greyed:.4} vs gate {AREA_BUTTON_LIVE}"
    );
    // A margin on both sides, as above. A bar resting a thousandth off either population is a
    // bar the next capture moves.
    assert!(
        worst_live - AREA_BUTTON_LIVE > 0.05 && AREA_BUTTON_LIVE - greyed > 0.05,
        "the gap is too thin to trust: worst live {worst_live:.4}, greyed {greyed:.4}, \
         gate {AREA_BUTTON_LIVE}"
    );
    // **The live populations**, from the `area slot:` lines of `spike-run-20260821-*.md`. Not
    // re-measurable here — they are readings of screens nobody captured — so they are written
    // down as the numbers the runs printed, which is what a threshold moved by them owes the
    // next person to touch it.
    const LIVE_TRAVEL: f64 = 0.8566; // x244, every crossing press in five runs
    const LIVE_OPEN: f64 = 0.8458; // x6, a chest with nothing guarding it
    const LIVE_WORST: f64 = 0.8355; // x5, a village subnode
    const GREYED_AT_L38: f64 = 0.7367; // x1, the reading that ended run 0251Z
    for live in [LIVE_TRAVEL, LIVE_OPEN, LIVE_WORST] {
        assert!(live > AREA_BUTTON_LIVE, "{live:.4} is a live plank and must read as one");
    }
    assert!(GREYED_AT_L38 < AREA_BUTTON_LIVE, "and the one that ended a run must not");
    // The same 0.05 either side, which is what took the gate from 0.80 down to 0.79: at 0.80
    // the worst live reading had only 0.0355 under it.
    assert!(LIVE_WORST - AREA_BUTTON_LIVE > 0.04, "live margin");
    assert!(AREA_BUTTON_LIVE - GREYED_AT_L38 > 0.05, "greyed margin");
    // The greyed reading agrees with the corpus **exactly**, which is what makes the live logs
    // usable as calibration at all rather than as anecdote.
    assert!((GREYED_AT_L38 - greyed).abs() < 1e-4, "corpus {greyed:.4} vs live {GREYED_AT_L38}");

    // And it is strictly the looser of the two bars, which is what makes `Combat` imply `live`.
    assert!(AREA_BUTTON_LIVE < AREA_BUTTON_SHOWING);
}
