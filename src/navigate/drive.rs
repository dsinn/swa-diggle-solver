//! The step loop: look at the screen, decide, act, and say what happened.
//!
//! Lifted out of `navigate` on 2026-08-21 (#76) unchanged, because it is half the driver by weight
//! and the half that keeps growing. Everything it needs — [`Run`] and its methods, the module's
//! constants, the small pure helpers — stays in the parent, which a child module can see; that is
//! why this moved rather than [`Run`] itself, and why the move cost no visibility widening at all.
//!
//! `drive` is the only caller of most of what `navigate` holds, so reading it top to bottom is
//! still the way to learn what the driver does. What the split buys is that the loop can now be
//! read without scrolling past two thousand lines of the methods it calls.
//!
//! ## Where the arrival dispatch goes
//!
//! The dispatch on *what we arrived at* — shrine, inn, shop, tower, crossing — is still inside the
//! loop rather than a function of its own, which is what #76 records as the next cut and what #14
//! (the tower `Reveal` press) and #16 (open a chest on arrival) will both want. Lifting it is a
//! real change and not a move, so it is deliberately not done here.

// The parent is this module's whole vocabulary: `Run`, `Stop`, every timing constant, and the
// pure helpers the loop reads. Listing them would be a hundred lines that say nothing.
use super::*;

pub fn drive(
    r: &mut Run, fight: &Fight, health: &mut Option<crate::rest::Health>, deadline: Instant,
) -> Stop {
    // Are we already in a fight? The save says so outright.
    //
    // `combatSaveData` exists only while a fight is live — the game deletes it when combat ends,
    // which is why `fight.rs:306` reads an unreadable file as "combat is over". So its presence is
    // the question answered, not a hint to be corroborated. `checkpoint.rs:131` already trusts it for
    // exactly this, and `Fight` was built to join a fight in progress (`fight.rs:10`); the only thing
    // missing was anyone asking at startup.
    //
    // Asked here, before a single frame is captured, because the alternative is inferring it from
    // pixels *through* the main menu still fading out after the launch click — the transient that
    // defeated two resumes, scoring 0.3223 one run and 0.5985 the next. A file on disk has no
    // animation.
    //
    // Startup is also the safe end of the standing rule about this file: never read it while the
    // game is deleting it, which is the moment a reward is confirmed or postgame is dismissed.
    // Nothing is in flight here.
    if fight.combat_path.is_file() {
        r.log.push_str("0. resuming a fight already in progress
");
        // **Sit still for eight seconds before touching anything.**
        //
        // Resuming drops us into a fight the game is still opening. On 2026-08-12 that meant a boss
        // introduction — the combat HUD up, the enemy named across the screen, and no board drawn
        // yet — and the first clicks went into it and were discarded. Nothing downstream could tell:
        // the readiness check samples tile centres for brightness, and on that card they sample sky
        // and sea, which are brighter than any tile. Sixteen of sixteen slots read as occupied on a
        // screen with no board on it, so the wait it was supposed to perform never happened.
        //
        // A fixed delay, not a cleverer measurement. Measuring this properly means telling a tile
        // from scenery, and two attempts at that on 2026-08-12 both shipped regressions that ended
        // runs at the *start* of ordinary fights — the dev's call was to stop patching and take the
        // blunt instrument. Eight seconds covers the introduction, costs eight seconds once per
        // resumed fight, and cannot be fooled by what is on screen because it does not look.
        //
        // Scoped to the resume path deliberately. A fight entered by walking into it is announced on
        // the console and already handled; this is the one entry the run does not watch happen.
        std::thread::sleep(RESUME_SETTLE);
        r.log.push_str(&format!("  waited {RESUME_SETTLE:?} for the fight to finish opening\n"));
        let mut fl = String::new();
        let outcome =
            fight.run(&mut r.feed, &r.keys, &mut fl, deadline.min(Instant::now() + Duration::from_secs(300)));
        r.log.push_str(&fl.lines().map(|l| format!("    {l}
")).collect::<String>());
        match outcome {
            Ok(o) if o.cleared() => {
                r.log.push_str(&format!("  resumed fight finished: {o:?}
"));
                let now = r.apply_save();
                if let (Some(b), Some(a)) = (health.clone(), now) {
                    r.map.note_health(b, a);
                    r.map.rested(a);
                }
                *health = now;
            }
            // Not a stop dressed as success: an unresumable fight is worth reporting as itself, so a
            // run that cannot rejoin says so rather than wandering onto the map path and failing to
            // find a map that was never there.
            Ok(o) if o.fatal() => return Stop::Died(format!("resumed: {o:?}")),
            // Before the catch-all, or a deliberate abort reports as a fight that went wrong.
            Ok(o) if o.stop_requested() => return Stop::Requested,
            Ok(other) => return Stop::Fought(format!("resumed: {other:?}")),
            Err(e) => return Stop::Failed(format!("could not resume the fight: {e}")),
        }
    }

    for step in 1.. {
        // **Every step, at the top.** Placed before the exits below rather than after the step's
        // work, so a run that ends on this iteration has already shown everything the previous one
        // wrote — the `Stop` arms all return without coming back here.
        //
        // The affirmative streak is closed first so its count belongs to the step that produced it
        // and cannot be attributed to the next one.
        r.close_affirmative_run();
        r.flush_log();
        if Instant::now() >= deadline {
            return Stop::Exhausted;
        }
        // Checked here, at the top, and not partway down where it used to sit: with no step cap
        // above it, this and the deadline are the only two ways a run ends that are not the run's
        // own decision, and a `continue` from a screen handler must not be able to skip either.
        if crate::config::stop_requested() {
            let _ = std::fs::remove_file(STOP_FILE);
            r.log.push_str(&format!("{step}. stop requested — ending cleanly\n"));
            return Stop::Requested;
        }
        if r.map.anomaly_beaten() {
            return Stop::AnomalyBeaten;
        }
        // Ask what is on screen before acting on the assumption that it is the map.
        //
        // The navigator assumes "map" and finds out otherwise several steps later, by failing —
        // four `Absent` locate-me readings after a cleared crypt, and a run that clicked into the
        // character inventory and had no way back. Both were one look away from being known.
        //
        // Only screens that need naming are logged. The map is the ordinary case and reporting it
        // every iteration would bury everything else.
        let mut screen = crate::act::identify(r.win);
        // **One look is not enough right after we asked for a fight.**
        //
        // `identify` is a set of `score_exact` comparisons, and mid-transition every template scores
        // low — the same "a number below the threshold is not the same as the screen having moved
        // on" that [`crate::act::score_exact`] warns about and that [`crate::act::wait_for`] exists
        // for. Asking once and calling it Unknown commits this iteration to the map path.
        //
        // Live 2026-08-14 at `l16sub14`: the press landed (the screen moved 0.975), the single look
        // came back Unknown, the map path re-derived the same step and pressed the same coordinate
        // into the pregame, where it means nothing — 0.011 of movement — and the run stopped with
        // `Combat did not open`. `spike-frames-live/gave-up.png` is that pregame, and its Start
        // button scores **1.0000** against a 0.90 bar, on both the exact and searched paths. The
        // fingerprint was never the problem; nothing asked it a second time.
        //
        // Scoped to `combat_expected`, which until now was carried only so the log could say whether
        // a fight was walked into or asked for. Everywhere else Unknown is the ordinary answer —
        // the map is Unknown — so polling for a name would cost every iteration the full timeout.
        // Here we have just pressed a button whose whole purpose is to leave the map, so Unknown is
        // a transition rather than a verdict, and it is worth waiting out.
        if screen == crate::act::Screen::Unknown && r.combat_expected {
            let by = Instant::now() + COMBAT_OPENS_BY;
            while Instant::now() < by {
                // **Take the cursor back before looking.** Opening a screen moves the real mouse:
                // `input.setHotspotHighlight` calls `love.mouse.setPosition(unpack(hotspot))` and
                // hides the pointer (`utils/input.lua:94-96`), so the game parks it on that screen's
                // hotspot — which on the pregame is `Start`. The button then draws in its hover art,
                // and every template in the registry is cropped from the resting art.
                //
                // That is what four seconds of `Unknown` was, live 2026-08-14 at `l16sub5`: the
                // screen was up and unmistakable to a human, and unreadable to us because we were
                // holding a picture of it in a state the game had moved it out of. The frame that
                // seemed to disprove this scored 1.0000 only because the *next* press parked the
                // cursor before the capture — a photograph taken after the evidence had gone.
                //
                // Parking is already how this run keeps a hover out of a reading — see [`NEUTRAL`],
                // chosen to sit clear of the view's own hotspot rectangle. Doing it inside the loop
                // rather than once, because the warp follows the screen: whatever arrives during
                // these four seconds may grab the pointer again on the way in.
                r.park();
                std::thread::sleep(Duration::from_millis(150));
                r.pump();
                screen = crate::act::identify(r.win);
                if screen != crate::act::Screen::Unknown {
                    break;
                }
            }
            // Cleared either way: a press that opened nothing within the window is a press that
            // failed, and leaving the flag up would make the *next* iteration wait all over again.
            if screen == crate::act::Screen::Unknown {
                r.combat_expected = false;
                r.log.push_str(&format!(
                    "  pressed Combat and no screen arrived within {COMBAT_OPENS_BY:?} — treating \
                     it as still the map\n"
                ));
                // **Photograph and score before moving on**, because this branch has already been
                // wrong once about what it was looking at.
                //
                // Live 2026-08-14 at `l16sub5`: the console printed `Pregame screen:` — its last
                // line — so the screen was constructed, this poll ran its full four seconds naming
                // nothing, and `gave-up.png` taken moments later scores **1.0000** for
                // `PREGAME_START`. Three facts that cannot all be about the same instant, and no
                // capture existed from inside the window to say which one moved.
                //
                // A number here settles it next time: a Start at 0.89 is a threshold or a hover
                // state, a Start at 0.02 is a screen that was not drawn yet, and those want
                // opposite fixes.
                //
                // Kept now that the cursor is taken back above, because the park is a counter to one
                // mechanism rather than a proof there is only one. If this line ever prints a Start
                // in the eighties again, the answer is the game's own hover art — `ButtonArt` in
                // [`crate::observe::affirm`] already loads all five states from the game's files,
                // and no hand-cropped template would be needed.
                r.snap_screen("combat-no-screen");
                r.log_button_scores();
            }
        }
        if screen != crate::act::Screen::Unknown {
            r.log.push_str(&format!("{step}. screen: {screen:?}\n"));
        }
        // A screen we can name and cannot act on. Stopping here is the whole point: the alternative
        // is what this loop used to do with one, which is fall through to the map path and spend the
        // remaining budget probing for a map that is not there, then report the probing as the
        // failure. `Screen::Dead` is the one that qualifies today.
        if let Some(stop) = precheck(screen) {
            r.log.push_str(&format!(
                "  **`{screen:?}` is recognised and nothing answers it** — stopping here rather \
                 than treating it as the map\n"
            ));
            return stop;
        }
        // A fight we did not know we were in. Tested first, because every branch below assumes there
        // is a map underneath the screen, and in combat there is not.
        //
        // This is the gap that stranded a run at `l41`. `handle_event` answered a `Stump in the road`
        // by taking `[Combat] - Cut it down.`, combat started, and nothing on the overworld path
        // watches for that — so the navigator went on probing for a map, four blind locate-me clicks
        // deep, on a combat screen. The console had said `Player turn 0 start` with the whole board
        // in it; nobody was listening, and no button-shaped fingerprint could have helped, because
        // the player was at 1/20 health and the hurt vignette had taken the affirmative slot with it.
        //
        // Recognised by the HUD instead, which the vignette does not reach. See [`act::COMBAT_HUD`].
        //
        // ## Now the only way into a fight, planned or not
        //
        // The planned path used to play its own fight out immediately after pressing the area
        // button, on the strength of having seen `Pregame screen:`. That made two fight handlers
        // whose only difference was how they had found out, and the one with the announcement could
        // not recognise a door that does not make one. Pressing the button now just `continue`s, so
        // both arrive here — and `combat_expected` is kept only so the log can still say which.
        if screen == crate::act::Screen::CombatEntered {
            r.log.push_str(match std::mem::take(&mut r.combat_expected) {
                true => "  the fight is up — playing it out\n",
                false => "  in combat and had not noticed — playing it out\n",
            });
            let mut fl = String::new();
            let outcome = fight.run(
                &mut r.feed,
                &r.keys,
                &mut fl,
                deadline.min(Instant::now() + Duration::from_secs(400)),
            );
            r.log.push_str(&fl.lines().map(|l| format!("    {l}\n")).collect::<String>());
            match outcome {
                Ok(o) if o.cleared() => {
                    r.log.push_str(&format!("  fight finished: {o:?}\n"));
                    // Every screen position we hold predates the map the fight just handed back.
                    // In a lost woods that is literal: `loadLight` re-orients it. See
                    // [`Run::settled_dump`].
                    r.positions_stale_at = r.dumps;
                    // And the camera is still returning to the player, so even a dump printed after
                    // this one describes a view in motion. See [`Run::needs_recentre`].
                    r.needs_recentre = true;
                    // Bookkeeping every fight needs. Skipping it is how a run walks out of a fight at
                    // 1/20 and never considers resting, because nothing recorded the loss.
                    let now = r.apply_save();
                    if let (Some(b), Some(a)) = (health.clone(), now) {
                        r.map.note_health(b, a);
                        r.map.rested(a);
                    }
                    *health = now;
                    continue;
                }
                // Reported as a fight that went wrong, not as a map failure. The whole point of this
                // branch is that "no pan dump after locate-me" was never the truth about this state.
                Ok(o) if o.fatal() => return Stop::Died(format!("{o:?}")),
                Ok(o) if o.stop_requested() => return Stop::Requested,
                Ok(other) => return Stop::Fought(format!("{other:?}")),
                Err(e) => return Stop::Failed(format!("could not play the fight out: {e}")),
            }
        }
        // **A shrine screen after a shrine fight is not a dead end, it is the handoff.**
        //
        // The dev, 2026-08-15: "Upon completing the combat at shrine1, we should immediately
        // Consecrate instead of having to rely on the fallback."
        //
        // Right, and the game agrees — `overworld.lua:1070-1079` wires the postgame screen's way
        // back to a freshly loaded shrine when the scenario was a shrine, so dismissing the rewards
        // lands us *on* the shrine with `directFromCombat = true`. Escaping that and walking back in
        // through `Visit` is refusing a door the game opened.
        //
        // The save decides, not the screen. [`crate::act::Screen::Shrine`] means no more than
        // "there is a way back from here" — it has fired on an inn — so what makes this safe is
        // `worth_consecrating_here`, which reads `_consecrated` and the corruption flags and is
        // documented as the only thing allowed to answer "is a shrine underfoot".
        //
        // Re-read first: `completed` reaches the save when the *screen* is exited, so the flag from
        // the fight we have just won can be one read behind. Same reason, same fix, as the arrival
        // branch's own re-read.
        if screen == crate::act::Screen::Shrine {
            r.apply_save();
            let here = r.map.here().map(str::to_string);
            if let Some(key) = here.filter(|k| {
                r.map.worth_consecrating_here(k) && !r.shrines_tried.contains(k)
            }) {
                r.log.push_str(&format!(
                    "  the fight left us on `{key}`'s shrine screen — consecrating here rather \
                     than walking back in\n"
                ));
                r.shrines_tried.insert(key.clone());
                r.map.abandon(&key);
                let anomaly_open = r.map.anomaly_is_open().unwrap_or(false);
                match crate::shrineplay::play(r.win, &r.keys, anomaly_open, true) {
                    Ok(played) => {
                        r.log.push_str(&played.log);
                        if played.consecrated && !r.confirm_consecrated(&key, CONSECRATE_CONFIRM) {
                            r.log.push_str(
                                "  shrine: the screen closed but `_consecrated` never landed\n",
                            );
                        }
                    }
                    Err(e) => r.log.push_str(&format!("  shrine failed: {e}\n")),
                }
                r.apply_save();
                continue;
            }
        }
        // Every dead end that is left by pressing one button. See [`ESCAPES`] for which, and why
        // each is in the list.
        if let Some(esc) = ESCAPES.iter().find(|e| e.screen == screen) {
            match crate::act::click_exact(r.win, esc.button, esc.threshold) {
                Ok(q) => {
                    r.park();
                    std::thread::sleep(Duration::from_millis(900));
                    r.pump();
                    r.log.push_str(&format!("  left {} ({q:.4})\n", esc.what));
                }
                Err(e) => return Stop::Failed(format!("stuck on {}: {e}", esc.what)),
            }
            continue;
        }
        // The main menu, which we reach on purpose (the cinematic skip reloads the world through it)
        // and by accident (anything that strands us outside the game). Either way the way out is the
        // same, and it belongs here rather than only inside `skip_cinematic`: that function owned the
        // click, so when its one attempt was refused the run had no second look, fell through to the
        // map path, and blind-probed a menu — which is how it kept opening the almanac.
        //
        // A second look is worth having because the refusal is often transient. Arriving from the
        // options menu leaves `Continue` highlighted, and a highlighted button is not the button the
        // template was cut from: it scored 0.5726 against a 0.90 bar on first arrival, and cleanly on
        // the way back. `click_exact` refusing was right — `Restart` is the neighbour, and it
        // eulogises the run — but a refusal is a reason to look again, not to stop.
        if screen == crate::act::Screen::MainMenu {
            r.park();
            std::thread::sleep(Duration::from_millis(600));
            r.pump();
            // Both renderings, same origin and same click: whichever matches, the action is
            // identical. The plain template is asked first because it is the ordinary case; the
            // highlighted one exists because arriving through the options menu — the skip's own
            // route — leaves the button lit, and that state had no template until it stalled a run.
            let mut hit = crate::act::click_exact(
                r.win,
                &crate::act::CONTINUE,
                crate::act::CONTINUE_PRESENT,
            );
            if hit.is_err() {
                hit = crate::act::click_exact(
                    r.win,
                    &crate::act::CONTINUE_HOT,
                    crate::act::CONTINUE_PRESENT,
                );
            }
            match hit {
                Ok(q) => {
                    r.log.push_str(&format!("{step}. resumed from the main menu ({q:.4})
"));
                    std::thread::sleep(Duration::from_millis(1500));
                    r.pump();
                }
                // Deliberately not a stop. The next iteration re-identifies and tries again, which is
                // exactly what turns a highlighted button into an ordinary one.
                Err(e) => {
                    r.log.push_str(&format!("{step}. main menu, Continue refused: {e}
"));
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            continue;
        }
        // A class unlock, which arrives unannounced after a fight — a live run was handed "The
        // Cultist class is now available." on the way back from a level 3 crypt. Dismissing it is the
        // whole interaction; the unlock itself is a profile-wide reward that needs nothing from us.
        if screen == crate::act::Screen::Unlock {
            match crate::act::click_exact(
                r.win,
                &crate::act::UNLOCK_CONTINUE,
                crate::act::UNLOCK_CONTINUE_PRESENT,
            ) {
                Ok(q) => {
                    r.park();
                    std::thread::sleep(Duration::from_millis(900));
                    r.pump();
                    r.log.push_str(&format!("  dismissed a class unlock ({q:.4})\n"));
                }
                Err(e) => return Stop::Failed(format!("stuck on the unlock screen: {e}")),
            }
            continue;
        }
        // The combat pregame: recognised by its `Start`, and the click validated by that button
        // going away.
        //
        // This first watched the console for `Pregame screen:` (`ui/pregame.lua:91`) and pressed
        // Space. That worked, but it could only ever prove the screen had been *constructed* — the
        // announcement-is-not-readiness trap again — and gave no way to tell whether the press took.
        // The fingerprint answers both halves, which is why it is worth the crop.
        if screen == crate::act::Screen::Pregame {
            match crate::act::click_exact(
                r.win,
                &crate::act::PREGAME_START,
                crate::act::PREGAME_START_PRESENT,
            ) {
                Ok(q) => r.log.push_str(&format!("{step}. pregame — started the encounter ({q:.4})
")),
                Err(e) => return Stop::Failed(format!("pregame Start refused: {e}")),
            }
            r.park();
            if !crate::act::wait_until_gone(
                r.win,
                &crate::act::PREGAME_START,
                crate::act::PREGAME_START_PRESENT,
                Duration::from_secs(8),
            ) {
                return Stop::Failed("clicked pregame Start but it is still on screen".into());
            }
            std::thread::sleep(Duration::from_millis(800));
            r.pump();
            continue;
        }
        // Skipped here rather than at the call site that answered it, so any caller can arm it.
        // Checked before the screen is identified because the cinematic is precisely a state where
        // identification fails: `utils/events.lua:45-48` pans the camera to (0,0) and disables
        // interaction, which leaves locate-me inert and the map unreadable. The run this came from
        // spent its last four steps asking a panning, uninteractable map where it was.
        if r.pending_cinematic {
            r.pending_cinematic = false;
            match skip_cinematic(r) {
                Ok(()) => r.log.push_str(&format!("{step}. skipped the anomaly cinematic
")),
                Err(e) => r.log.push_str(&format!("{step}. could not skip the cinematic: {e}
")),
            }
            continue;
        }
        // An event can be up at the top of any iteration, and while one is it owns the screen —
        // clicking the map does nothing and `recentre` times out with "no pan dump". Entering a
        // subworld raises one immediately, which is how a live run got stuck at a village gate it
        // had just walked through. Answer it first, then look at the map.
        r.pump();
        // Text, then options, then whatever the choice raises next.
        //
        // Looped here rather than by falling back to the outer loop, because these chain: a choice
        // commonly leads to another lore screen, so a village gate can be text, text, choice, text
        // before the map is reachable. Re-entering the outer loop for each would spend a quarter of
        // the run's twenty steps standing still — and the step budget exists to stop *wandering*,
        // not to ration reading.
        //
        // Order matters: text gates options. While a lore screen is up the choices do not exist yet,
        // so `parse_events` would find nothing and the map would not be on screen either.
        // A reward screen gates everything behind it exactly as a lore screen does, and it is not
        // always the fight loop that has to deal with it: a fight can end, hand back, and leave the
        // screen standing. Until it is dismissed there is no map, no area buttons and no
        // affirmative, so every later step fails looking for a screen nobody checked for. That is
        // what four `Absent` locate-me readings at 0.23 turned out to be after a cleared crypt.
        //
        // Asked here, in the loop, rather than only where a fight happens to end — same reason
        // dialogue and lore detection live here rather than at a village gate.
        if crate::itemchoice::on_screen(r.win) {
            r.log.push_str(&format!("{step}. a `Choose one:` screen is up
"));
            let mut il = String::new();
            let picked = crate::itemchoice::choose(
                r.win,
                &mut r.feed,
                &r.keys,
                &fight.game_dir,
                &mut il,
                deadline.min(Instant::now() + Duration::from_secs(45)),
                // The map's own reading, which this path does have. See `itemchoice::Boon`: a Heal
                // boon is worth nothing at full health.
                r.map.is_hurt(),
            );
            r.log.push_str(&il.lines().map(|l| format!("    {l}
")).collect::<String>());
            match picked {
                Ok(crate::itemchoice::Chosen::Took(key)) => {
                    r.log.push_str(&format!("  took **{key}**
"));
                    // The screen closes into whatever built it -- a postgame after a fight, the map
                    // after a shop -- so let the next pass work out where we are rather than
                    // assuming. It is dismissed; that was the blocking part.
                    std::thread::sleep(Duration::from_millis(800));
                    r.pump();
                }
                Ok(other) => {
                    return Stop::Failed(format!(
                        "a `Choose one:` screen is up and cannot be cleared: {other:?}"
                    ))
                }
                Err(e) => return Stop::Failed(format!("item screen: {e}")),
            }
        }

        // A fight waiting to be finished gates the map exactly as an item screen does, and a save
        // resumed mid-combat drops us straight into one — the run then demanded an overworld dump,
        // found none, and stopped. Twice, on two different saves.
        //
        // Asked visually rather than from `combatSaveData`, whose existence proves nothing: it is
        // the whole RUN's save, removed only at death or postgame (`overworld.lua:968`, `:1154`), so
        // it is present all the time. `Finish` is drawn only in `WaitPhase`
        // (see [`crate::act::COMBAT_FINISH`]), which is precisely the state that strands us.
        //
        // `Fight::run` is built to join a fight already in progress, so it needs no special entry:
        // it reads the save, sees `WaitPhase`, and clicks Finish itself.
        // Polled, not asked once. Resuming a save lands here while the crypt is still fading in,
        // and a template scores near zero against a half-drawn screen -- so a single low reading
        // means "not yet", not "not there". Asking once is what sent three runs down the map path
        // with `Finish` about to appear behind them.
        let waited = crate::act::wait_for(
            r.win,
            &crate::act::COMBAT_FINISH,
            crate::act::COMBAT_FINISH_PRESENT,
            Duration::from_millis(2500),
        );
        if !waited.found() && (waited.best > 0.3 || waited.faults > 0) {
            // Only worth a line when it was close or we were blind; a flat miss on the overworld is
            // the ordinary case and would drown the log.
            r.log.push_str(&format!(
                "{step}. no Finish (best {:.2} over {} looks, {} capture faults)\n",
                waited.best, waited.looks, waited.faults
            ));
        }
        if waited.found() {
            r.log.push_str(&format!("{step}. a fight is waiting to be finished
"));
            let mut fl = String::new();
            let outcome = fight.run(
                &mut r.feed,
                &r.keys,
                &mut fl,
                deadline.min(Instant::now() + Duration::from_secs(300)),
            );
            r.log.push_str(&fl.lines().map(|l| format!("    {l}
")).collect::<String>());
            match outcome {
                Ok(o) if o.cleared() => r.log.push_str(&format!("  {o:?}
")),
                Ok(o) if o.fatal() => return Stop::Died(format!("{o:?}")),
                Ok(o) if o.stop_requested() => return Stop::Requested,
                Ok(other) => return Stop::Fought(format!("{other:?}")),
                Err(e) => return Stop::Failed(e.to_string()),
            }
            let now = r.apply_save();
            if let (Some(b), Some(a)) = (*health, now) {
                r.map.note_health(b, a);
                r.map.rested(a);
            }
            *health = now;
            continue;
        }

        // Dismissing something changes the screen, so go back to the top and look again rather than
        // carrying on as if the map were underneath.
        //
        // This used to `continue` the *inner* loop, which only ever meant "clear another text
        // screen" — the screen check at the top of the iteration was never re-run. A live run walked
        // straight through that gap: it finished a shrine, cleared one text screen, and proceeded
        // into the map path on the stats history page, failing four locate-me probes and stopping
        // with `no pan dump after locate-me`. Both new screen handlers existed by then and neither
        // was ever asked, because asking happens at the top and the run never got back there.
        let mut dismissed = false;
        for _ in 0..10 {
            if r.clear_text_screen() {
                r.log.push_str(&format!("{step}. cleared a text screen\n"));
                dismissed = true;
                break;
            }
            match r.handle_event() {
                Some(title) => {
                    r.log.push_str(&format!("{step}. answered `{title}`\n"));
                    // Answering the rumble is what *starts* the cinematic, so this is the moment to
                    // skip it — see `skip_cinematic`.
                    if title.to_ascii_lowercase().contains("rumble") {
                        match skip_cinematic(r) {
                            Ok(()) => r.log.push_str("  skipped the anomaly cinematic\n"),
                            Err(e) => r.log.push_str(&format!("  cinematic skip failed: {e}\n")),
                        }
                    }
                    dismissed = true;
                    break;
                }
                None => break,
            }
        }
        if dismissed {
            continue;
        }

        // Locate-me is an overworld control and does not work in a subworld — confirmed live. It is
        // also unnecessary there: the dump fires on every *arrival* (`overworldview.lua:1442`), and
        // inside a subworld we arrive every hop, so the coordinates are already as fresh as they get.
        //
        // The catch is that staleness is silent. An animated pan announces itself when
        // `offsetTransition` completes (`:1253-1255`), but a mouse **drag** writes `xoffset` directly
        // with no transition and no dump (`:1256-1259`) — so a dragged map invalidates every
        // coordinate we hold without saying so. Hence: act on a dump immediately, never hold one
        // across an action, and verify what the click did.
        // Inside a subworld the dump normally arrives free, because we arrive every hop. It does NOT
        // arrive on the hop that never happened: a run resumed inside a subworld has only the
        // `World loaded` dump, whose coordinates are unusable.
        //
        // `verboseAdjacencyData` prints `xoffset+location.posX*zoomMult` (`:1033`), and that dump is
        // emitted at `:1607` — *before* the camera is placed. Measured live: it reported the well at
        // (960, 520) while the well was drawn at (755, 600). Every entry is off by the same
        // translation, which happens to be the player's own world position, and that is the one
        // coordinate the dump never prints. So it cannot be corrected from the dump alone.
        //
        // Locate-me is the way out, and the earlier note here — that it "does not work in a
        // subworld" — was wrong about the cause. The button is simply not on screen: it shares its
        // slot with the location's area buttons. Clicking empty map restores it
        // (`core:mousereleased`, `:1479-1485`), and that branch tests only whether a location was
        // under the release, not which world we are in.
        let inside_now = r.map.inside().is_some();
        // **Poll, do not wait.** Ask for a dump with no timeout at all, and if there is not one, go
        // back to the top of the loop — where [`crate::act::identify`] gets another look — after a
        // second. Three times, then give up.
        //
        // The eight-second block this replaces is what made the observer useless at exactly the
        // wrong moment. `identify` runs at the top of each iteration and is perfectly stateless;
        // `act::COMBAT_HUD` scores **1.0000** on the frame the last run died holding. But the fight
        // had not rendered when the screen was checked, and by the time it had, the iteration was
        // committed to waiting for a dump that could never come and then exiting. One look, taken
        // too early, and no second chance for eight seconds.
        //
        // A dump also cannot arrive while an event dialogue is up, or a shop, or a class unlock —
        // every stall `inside a subworld with no settled dump` has ever reported was one of those,
        // and the message named `settled_dump`, which was working correctly every time.
        //
        // Polling makes the observer the thing that runs often and the dump the thing that is merely
        // checked for. A screen that appears mid-wait is now seen within a second instead of being
        // missed entirely.
        let fresh = if inside_now {
            // `Duration::ZERO` is one pump and one look — see `settled_dump`, which tests its
            // deadline after checking, so a zero budget still gets a full attempt.
            // A fight just ended: settle the camera deliberately rather than reading around it.
            // Nothing here is a fallback — if locate-me cannot be made to work we would rather take
            // the miss and retry than aim at a moving view. See [`Run::needs_recentre`].
            if r.needs_recentre {
                let (cw, ch) = r.win.client_size().unwrap_or((1920, 1080));
                // `recentre` leaves its answer in `latest`, which is where `settled_dump` reads
                // from, so there is nothing to carry across by hand. The flag clears only on
                // success: a locate-me that did not take has settled nothing.
                if r.recentre().is_some_and(|a| !camera_is_lost(&a.nodes, cw, ch)) {
                    r.needs_recentre = false;
                    r.dump_misses = 0;
                }
            }
            let polled = r.settled_dump(Duration::ZERO).or_else(|| {
                // `recentre` clicks the map to force a pan dump, so it is worth one try rather than
                // three: a run that is not on the map at all should not be clicking at it repeatedly.
                //
                // It is also the cure for a camera that has not caught up, because locate-me centres
                // on the player by construction. Its answer is held to the same test anyway — an
                // invariant that is only checked on one of two paths into the same variable is a
                // rule with a hole in it.
                let (cw, ch) = r.win.client_size().unwrap_or((1920, 1080));
                (r.dump_misses + 1 >= MAX_DUMP_MISSES)
                    .then(|| r.recentre())
                    .flatten()
                    .filter(|a| !camera_is_lost(&a.nodes, cw, ch))
            });
            match polled {
                Some(a) => {
                    r.dump_misses = 0;
                    a
                }
                None => {
                    r.dump_misses += 1;
                    if r.dump_misses >= MAX_DUMP_MISSES {
                        return Stop::Failed(format!(
                            "inside `{}`: no dump, and no screen we recognise, {} looks over",
                            r.map.inside().unwrap_or("?"),
                            r.dump_misses
                        ));
                    }
                    r.log.push_str(&format!(
                        "{step}. no dump inside `{}` — asking the screen again (look {} of {})\n",
                        r.map.inside().unwrap_or("?"),
                        r.dump_misses + 1,
                        MAX_DUMP_MISSES
                    ));
                    std::thread::sleep(DUMP_RETRY_PAUSE);
                    continue;
                }
            }
        } else if r.map.standing_on_what_we_came_for(r.committed_to.as_deref())
            && r.latest.is_some()
            && r.select_here()
        {
            // **Select, do not wait.** We are standing on the node we walked to and the next action
            // is one press at a fixed coordinate, so the pan this would otherwise sit through buys
            // nothing: the area slot is HUD, drawn wherever the camera is, and input during the
            // glide lands.
            //
            // The arrow press itself still happens — see the note at [`Run::recentre`]. Skipping it
            // outright is what ended the run of 2026-08-20, because it is the press that sets
            // `selectedLocation` to us, and without it the slot keeps whichever node was clicked
            // last. The frame showed a greyed `Combat` belonging to a crypt three nodes away.
            //
            // `latest.is_some()` guards the binding rather than the decision: nothing on the surface
            // path reads `fresh` — it is consumed only by `cross_toward` and the crossing arms,
            // both inside a subworld — but the binding still needs a value, and on the first step of
            // a run there is not one yet.
            r.log.push_str(&format!(
                "{step}. standing on `{}`, which is what we came for — selected it without waiting \
                 for the pan\n",
                r.map.here().unwrap_or("?")
            ));
            r.skipped_the_pan = true;
            r.latest.clone().expect("guarded")
        } else {
            // The overworld gets the settling for free — it re-centres every step, and `recentre`
            // already refuses a dump older than its own click. What it did not get is the sanity
            // check on the answer, and there is no reason the overworld camera is more trustworthy
            // than a subworld's; a locate-me caught mid-glide reads the same either side.
            //
            // Treated as a miss rather than a stop, which is what this branch already does with a
            // locate-me that did not answer: go round, re-identify, try again.
            let (cw, ch) = r.win.client_size().unwrap_or((1920, 1080));
            match r.recentre().filter(|a| !camera_is_lost(&a.nodes, cw, ch)) {
                Some(a) => {
                    r.recentre_misses = 0;
                    a
                }
                // **Not a stop on the first miss.** A failed locate-me means "no map answered", and
                // the commonest reason by far is that a screen is still arriving — the run that
                // produced this branch stopped on a stats history page caught mid-fade, with the
                // previous screen's furniture still drawn over the top of it. Giving up on the first
                // look diagnoses a transition as a dead end.
                //
                // So: wait a second and go round again. Going round is what matters more than the
                // second — it re-runs `identify` at the top of the loop, which is where a screen
                // that is not the map gets recognised and handled. Retrying the probe in place would
                // only ask the same question of the same wrong screen.
                None => {
                    r.recentre_misses += 1;
                    if r.recentre_misses <= RECENTRE_RETRIES {
                        r.log.push_str(&format!(
                            "{step}. no pan dump — waiting for a transition (miss {} of {})\n",
                            r.recentre_misses, RECENTRE_RETRIES
                        ));
                        std::thread::sleep(Duration::from_secs(1));
                        continue;
                    }
                    return Stop::Failed(format!(
                        "no pan dump after locate-me, {} times over",
                        r.recentre_misses
                    ));
                }
            }
        };
        let here = r.map.here().unwrap_or("?").to_string();
        let place = r.map.get(&here).cloned();

        // **#26, the reading half.** What a destroyed village leaves behind, said out loud and not
        // acted on.
        //
        // The press is the part nobody has watched: `lootShop` (`utils/world.lua:1260-1315`) has two
        // outcomes and only one of them changes the screen — `#rewards > 0` opens an item selection,
        // and an empty shelf adds gold, plays a particle and changes no mode at all, so a driver that
        // presses and waits for a screen hangs on the ordinary case. The dev's call on that was to
        // *press it and look* rather than predict it, which needs a live run to be looking.
        //
        // So this run says what it can see, and a run with the dev watching turns it into a press
        // with the detection already proved. Free either way: `loot_here` is two map lookups on a
        // step that has already done far more.
        match r.map.loot_here(&here) {
            0 => {}
            left => r.log.push_str(&format!(
                "  `{here}` ({}) can still be looted {left}× — **not pressed**, see #26\n",
                place.as_ref().map(|p| p.heading.as_str()).unwrap_or("?")
            )),
        }

        // **Are we going round in circles?** See [`Run::sterile_here`] for why this is here at all
        // and why it stops rather than steers.
        //
        // Before any of the branches below, because every one of them is a reason to be at a node
        // and none of them is in a position to notice that we have been at it four times over.
        // Computed once per step and used twice: by the loop guard below, and by the crossing
        // branch's door line — which needs exactly this to say whether the door still matches the
        // errand. Two calls would be two plan computations that could disagree.
        let doing = r
            .map
            .next_target()
            .map(|p| format!("{:?} -> {}", p.reason, p.target))
            .unwrap_or_else(|| "no errand".into());
        {
            let laps = r.sterile_here(&here);
            r.recent.push_back(format!("{step}. `{here}` — {doing}"));
            while r.recent.len() > 10 {
                r.recent.pop_front();
            }
            if laps >= LOOP_GIVE_UP {
                let trail = r.recent.iter().cloned().collect::<Vec<_>>().join("\n  ");
                r.log.push_str(&format!(
                    "{step}. **going in circles** — `{here}` for the {laps}th time with nothing \
                     gained in between\n  {trail}\n  progress stuck at {:?}\n",
                    r.map.progress()
                ));
                r.log_button_scores();
                return Stop::Looping(format!(
                    "{here} visited {laps} times with no progress; last errand {doing}"
                ));
            }
            // A frontier that has twice taught us nothing stops being one. See [`LOOP_WRITE_OFF`]
            // for the loop this breaks and why the two guards already in place could not.
            //
            // Falls through rather than restarting the step: writing off is a fact about the map, and
            // every chooser below reads the map fresh, so this step is decided with it already true.
            if laps >= LOOP_WRITE_OFF
                && place.as_ref().map(|p| p.is_frontier()).unwrap_or(false)
                && !r.map.is_written_off(&here)
            {
                r.map.abandon(&here);
                r.log.push_str(&format!(
                    "{step}. `{here}` is written off — stood on {laps} times with nothing learned, \
                     and a frontier that teaches nothing is not a frontier\n"
                ));
            }
        }

        // A shrine we are standing on whose fight is already won and whose blessing is unclaimed.
        //
        // Taken **on arrival, whatever the errand was**. An uncorrupted shrine is strictly
        // beneficial: the walk is already paid for, and `Pray` hands over wildcard tiles
        // (`doPray` -> `blessings.wildcardRewards`, `shrine.lua:126-131`) which are exactly what a
        // hurt run wants before its next fight. The objective is not disturbed — this returns to the
        // map and the next `next_target` picks up where it left off.
        //
        // `completed` is the shrine's *area*, i.e. its combat, and it is what decides whether the
        // overworld slot holds `Visit` or `Combat` (`overworld/locations/shrine.lua:64,76`). It is
        // NOT the word: `shrineView.hasWon()` is the guess list restored from
        // `<key>subs` (`shrine.lua:495`), which is why a shrine can be `[done]` with the puzzle
        // untouched — which is exactly the state the live run found shrine1 in.
        //
        // `used` is `<key>_used` (`overworldview.lua:215-218`), set by praying, and is the only
        // thing that gates `Pray` once the word is solved (`showPrayButton`, `shrine.lua:98-102`).
        // So "complete and unused" is precisely "there is something here worth doing".
        // The greyed forms, which are the states a fingerprint is most likely to be fooled by: the
        // verb is still drawn, just at half alpha over a different base image
        // (`ui/elements/button.lua:128,131`). A threshold measured only against a live button would
        // read "finished" as "ready" — the failure that a shared slot punishes hardest, because the
        // coordinate that means `Visit` on one node means `Combat` on the next.
        //
        // "Spent" is a different flag for each verb, and conflating them produced two byte-identical
        // captures labelled as opposites. A crypt's `Combat` greys out when its area is `completed`.
        // A shrine's `Visit` does not: `completed` is the shrine's *combat*, and the shrine stays
        // visitable until `used` records that something was prayed for.
        if let Some((p, tag)) = place.as_ref().and_then(|p| {
            if p.is_shrine() {
                p.used.then_some((p, "visit-spent"))
            } else {
                p.completed.then_some((p, "combat-spent"))
            }
        }) {
            let _ = p;
            if !r.slots_captured.contains(tag) {
                r.slots_captured.insert(tag.to_string());
                r.snap_area_slot(tag);
            }
        }
        // **One shrine branch, and it always brings the typist.**
        //
        // The dev's rule: solve the word as soon as we enter an unsolved shrine. There used to be a
        // second branch below for a shrine that was `used` but unconsecrated, and it called
        // `shrineplay::consecrate` — which opens the screen, looks for a live `Consecrate` and has
        // no typist at all. Whenever the word was not already won that branch could only fail, and
        // on 2026-08-15 it did: two subworlds and three fights to reach `shrine5`, greyed button,
        // `left unconsecrated`, nothing typed.
        //
        // `play` handles every state the slot can be in — active `Consecrate` spends the solve,
        // `Pray` claims the blessing, an empty slot means the word and so the solver runs — so there
        // is nothing for a second entry point to add. See its doc for the four-state table.
        //
        // The condition is the union of what the two branches covered: an unused shrine is worth
        // entering for its blessing, and a used one is worth entering while the anomaly is open and
        // it is still unconsecrated.
        // **A shrine's `completed` is one save read behind us when we arrive.**
        //
        // The guard below wants `completed` because the area slot holds `Visit` only when the area
        // is complete — press it early and the click starts a fight instead of opening the word. For
        // a shrine with no fight on it that flag lands on *arrival*, so the map we planned the hop
        // with predates it and the branch silently declines.
        //
        // Live 2026-08-15, twice, and it read as two different bugs. At `shrine5` on the first run
        // the play branch declined and the consecrate branch took it instead — with no typist, so
        // nothing was solved. On the second run the branches had been merged, so declining meant
        // doing *nothing at all*: the run crossed three fights to reach `shrine5`, arrived, and left
        // for `shrine7` in the same step. The dev watched both.
        //
        // One extra save read, only when standing on a shrine we think is unfinished. Everything
        // else here already re-reads after acting; this is the one place that had to read *before*.
        let place = match place.as_ref().filter(|p| p.is_shrine() && !p.completed) {
            Some(_) => {
                r.apply_save();
                r.map.get(&here).cloned()
            }
            None => place,
        };
        if let Some(p) = place
            .as_ref()
            .filter(|p| p.is_shrine() && p.completed)
            .filter(|p| !p.used || r.map.worth_consecrating_here(&p.key))
            .filter(|p| !r.shrines_tried.contains(&p.key))
        {
            let key = p.key.clone();
            r.log.push_str(&format!("{step}. at **{key}** — playing the shrine\n"));
            // Standing on an unused shrine, so the slot is showing a live `Visit`.
            r.snap_area_slot("visit-live");
            // Marked before the attempt, not after: a play that panics, times out, or leaves us on
            // an unexpected screen must still count as having had its go, or the guard protects
            // nothing in exactly the cases it exists for.
            r.shrines_tried.insert(key.clone());
            // **Crash-safety, and no longer the loop guard it was.** Since a failed play stops the
            // run outright (below), the only visit that gets past this line is one that worked — and
            // a worked shrine sets `_used`, which is what `worth_a_trip` reads. What this still buys
            // is the case where `play` never returns at all.
            //
            // The planner has to be told as well, and this is the whole reason `abandon` exists.
            // `shrines_tried` only stops us *entering* the shrine again; it says nothing about
            // whether the shrine is worth walking to, and a shrine left unprayed is still unused as
            // far as the save is concerned. With only half the story, the planner kept choosing the
            // one shrine the arrival branch would always decline, and the run ping-ponged between it
            // and the cleared crypt on the way to the next.
            r.map.abandon(&key);
            // The portal decides which button a solve produces, so the answer travels into `play`
            // rather than being discovered by a failed match afterwards. Unknown reads as closed —
            // the conservative direction, since it only costs the older `Pray` attempt.
            let anomaly_open = r.map.anomaly_is_open().unwrap_or(false);
            // **The navigator opens the screen; the driver only plays it.** — the dev, 2026-08-17:
            // *the navigator should make sure we leave the overworld before dispatching the shrine
            // driver.*
            //
            // The driver can open a shrine perfectly well and still be the wrong place to enforce
            // this, because what it owns is the *word*, not the map. When the `Visit` press was its
            // problem, a swallowed press became "could not identify the grid" — a shrine-shaped
            // report for a navigation fault, three layers from the thing that actually went wrong.
            // Worse, its probe then typed into the overworld, which binds user functions to keys.
            //
            // So the handover has a precondition now, and it is checked by the side that owns the
            // map. `play` is told the screen is already open, and every state it can find there is
            // one of its own.
            // `Shrine screen:` is the game's own word for it (`shrine.lua:431-432`); the two screen
            // fingerprints are the backstop, and the weaker one — `Screen::Shrine` is a back plaque
            // four screens share, which is precisely the ambiguity the console line does not have.
            if !r.left_the_overworld(
                "Visit",
                Some(crate::act::SHRINE_SCREEN),
                &[crate::act::Screen::Shrine, crate::act::Screen::ShrineConsecrate],
            ) {
                return Stop::Failed(format!(
                    "shrine {key}: `Visit` never took us off the map after {LEAVE_TRIES} presses — \
                     a navigation fault, not a shrine one"
                ));
            }
            match crate::shrineplay::play(r.win, &r.keys, anomaly_open, true) {
                Ok(played) => {
                    let log = played.log.clone();
                    r.log.push_str(&log);
                    if played.consecrated && !r.confirm_consecrated(&key, CONSECRATE_CONFIRM) {
                        r.log.push_str(
                            "  shrine: **the screen closed but `_consecrated` never landed** —                              not consecrated after all
",
                        );
                    }
                    if !played.prayed && !played.consecrated {
                        // **Pre-MVP, this stops the run on purpose.** — the dev, 2026-08-17:
                        // *if the driver has a critical error with the interaction itself, since
                        // we're pre-MVP, we should stall on purpose.*
                        //
                        // It used to log loudly and walk on, on the reasoning that the blessing is a
                        // bonus. What settled it is that **no shrine can be lost by playing it
                        // correctly**: the exhaustive tree walk in `shrine::tests::
                        // no_answer_exhausts_the_guess_budget` plays every answer at every length and
                        // band, and the worst case never exceeds the budget. So a shrine we failed to
                        // use is never the shrine's fault — it is our grid reading, our click, or our
                        // colour classification, every time, and those are exactly the faults that
                        // stop being findable once the run walks away from them.
                        //
                        // The margin is what makes this urgent rather than tidy: the worst case is
                        // **exactly** the budget in five of the twelve configurations — 8 of 8 at
                        // length 4 Wild, 6 of 6 at every length from 5 up in Hard and Wild — so a
                        // single misread colouring is not a setback, it is the shrine. There is no
                        // slack to absorb one.
                        return Stop::Failed(format!(
                            "shrine {key} left un{} — no word can exhaust the budget, so this is an \
                             interaction fault worth stopping on",
                            if anomaly_open { "consecrated" } else { "prayed" }
                        ));
                    }
                }
                // Same rule, and this one was never even a judgement call: an error out of `play` is
                // the interaction failing outright.
                Err(e) => return Stop::Failed(format!("shrine {key} failed: {e}")),
            }
            // Re-read before anything plans on it. `_used` reaches the save when the shrine screen
            // is *exited*, which the driver has just done, so this is the first moment the flag is
            // readable — see the standing note that a stale read here is timing, not failure.
            r.apply_save();
            // **Was a blessing actually left behind?** Only the save can answer, and the answer is
            // not "did we press Pray this visit". A shrine solved before the anomaly opened was
            // prayed at then — with the portal shut that is the only reward a shrine offers — so it
            // owes the consecration alone and no `Pray` appears after the beam. Judging that by the
            // press would report a correct, complete visit as a failure.
            //
            // Worth a loud line when it is real, because `_used` is what `worth_a_trip` reads: a
            // shrine that stays unused stays a destination, and the run will walk back to it instead
            // of going to the anomaly.
            if let Some(p) = r.map.get(&key).filter(|p| !p.used) {
                let _ = p;
                r.log.push_str(
                    "  shrine: still **unused** after the visit — a blessing is owed here, and \
                     the planner will keep choosing this shrine until it is claimed\n",
                );
            }
            continue;
        }

        // A shrine we are standing on that is already used but not consecrated.
        //
        // This branch is why `worth_a_trip`'s `!consecrated` clause is honest. It used to promise a
        // trip that nothing could fulfil: `WorldMap::worth_consecrating_here` existed and was called
        // from nowhere, so the planner routed to a used shrine, `drive` declined it — the play
        // branch above needs `!used` — and the planner sent it straight back. A live run bounced
        // `l10 -> shrine2 -> l10` twelve times and died of it.
        //
        // An uncorrupted shrine is strictly worth it: consecrating costs no fight and is what the
        // `shrineKarma` economy pays out on. The gate is the map's, not this function's, because it
        // is the map that knows whether a *corrupted* one is merely on the way.
        // **Standing on a shrine and doing nothing is the end of it as a destination.**
        //
        // The structural backstop, and the more important half of the fix above. `abandon` existed
        // already but was only ever called from inside the branches that *act*, so any shrine the
        // driver declined stayed a perfectly good target forever and the planner kept re-choosing
        // it. That is a loop whatever the reason for declining, so the guard belongs here — where
        // "we arrived and nothing happened" is known — rather than being added case by case as each
        // new reason to decline appears.
        if place.as_ref().map(|p| p.is_shrine()).unwrap_or(false) {
            r.map.abandon(&here);
        }

        // A wizard's tower we are standing on that still has fog to sell. **Placeholder** —
        // `tower::press_reveal` is a stub, so this logs the opportunity and walks on.
        //
        // Wired up now, ahead of the press, because the decision is the part that can be got wrong
        // silently. `Reveal` and `Teleport` share a coordinate (`wizard_tower.lua:52,71`), so the
        // gate has to be computed rather than clicked at — see `tower::Offer`.
        //
        // `used` is a bool here and the flag is a number there, which loses the one case
        // `tower::Offer` can otherwise recover: a tower used before we picked up `towerRange+` is
        // offered again at the wider range (`wizard_tower.lua:56-59`). Mapping true to `Some(0)`
        // with `range = 0` collapses that to `Spent`, which is the safe direction and the same one
        // `Offer::of` documents — we skip a free map rather than press the mode change.
        if let Some(p) = place.as_ref().filter(|p| p.type_is(crate::tower::TYPE_NAME)) {
            let offer = crate::tower::Offer::of(true, p.used.then_some(0), 0);
            if offer == crate::tower::Offer::Available {
                r.log.push_str(&format!(
                    "{step}. at **{}** — `Reveal` is on offer here (NOT IMPLEMENTED, walking on)\n",
                    p.key
                ));
                match crate::tower::press_reveal(r.win) {
                    Ok(true) => {
                        r.apply_save();
                    }
                    Ok(false) => {}
                    Err(e) => r.log.push_str(&format!("  reveal failed: {e}\n")),
                }
            }
        }

        // Inside a subworld: walk to the exit rather than reaching for it.
        //
        // A `Fight` verdict deliberately falls through to the combat handling below, because it is
        // not a detour — `canTravelToDirect` refuses to move off an incomplete node, so clearing the
        // one underfoot is the only legal move available.
        let mut crossing = None;
        // The crossing said this node has to be dealt with. Carried to the fight branch rather than
        // re-derived there from `can_step`, which answers a *different* question — "is any move off
        // this node legal" — and says yes whenever a neighbour is complete, which after a retreat is
        // always true. That gap is why a `Fight` verdict could fall through the fight branch and out
        // the other side.
        let mut must_clear_here = false;
        if let Some(container) = r.map.inside().map(|s| s.to_string()) {
            // Bound rather than matched in place: `cross_toward` records the door it chose, so the
            // borrow it takes would still be live in the arms that call into the map.
            let verdict = r.map.cross_toward(&fresh.exits);
            match verdict {
                Some(crate::overworld::Crossing::Fight { at }) => {
                    r.log.push_str(&format!(
                        "{step}. inside `{container}` — `{at}` must be cleared before we can leave it\n"
                    ));
                    must_clear_here = true;
                }
                Some(mv) => crossing = Some((container, mv)),
                None => return Stop::Failed(format!("inside {container} with no crossing plan")),
            }
        }
        if let Some((container, mv)) = crossing {
            use crate::overworld::Crossing;
            // Standing on the inn we crossed the village for. Nothing to click on the map — the
            // errand is the whole reason we are in here.
            //
            // `abandon` first, and unconditionally, for the same reason the shrine branches do it:
            // this is the driver's record of having had its go. Without it a rest that fails to open
            // a screen leaves the inn a perfectly good destination, `cross_toward` routes straight
            // back to it, and the run spends its budget walking between the gate and the bar. The
            // planner and the driver must not disagree about what is still worth walking to.
            if let Crossing::Arrive { at } = &mv {
                // **Which errand are we standing on?** The two share this branch because reaching
                // them is identical; what happens next is not.
                if r.map.get(at).map(|p| p.is_general_store()).unwrap_or(false) {
                    r.log.push_str(&format!("{step}. at **{at}** in `{container}` - the general store
"));
                    // The same press as every other area button, and now confirmed by *which screen
                    // arrived* rather than by the screen having changed. `Shop` sits in the slot
                    // `Visit` and `Combat` share (`village.lua:312-320`), so it is the same press
                    // the shrine makes and it is lost the same way — and a shop that failed to open
                    // used to be indistinguishable from one that opened and stocked nothing, since
                    // the very next thing this does is read an inventory off the console.
                    if !r.left_the_overworld(
                        "Shop",
                        Some(crate::act::SHOP_OPENED),
                        &[crate::act::Screen::Shop],
                    ) {
                        r.map.abandon(at);
                        r.log.push_str("  the shop did not open - writing the store off
");
                        continue;
                    }
                    // The console is the instrument here, not the screen: the pump is what carries
                    // `Opened shop UI` and the inventory behind it.
                    std::thread::sleep(Duration::from_millis(1200));
                    r.pump();

                    // **The game told us the stock; the layout tells us where it is drawn.**
                    // Neither is a guess and neither is a template — see `crate::shopplay`.
                    let inv = crate::shopplay::inventory(r.feed.lines());
                    r.log.push_str(&format!("  the store lists {} items
", inv.len()));
                    let bought = match crate::shopplay::index_of(&inv, crate::shopplay::HEART) {
                        None => {
                            r.log.push_str("  no `healthBuff` in stock — nothing to buy here
");
                            false
                        }
                        // **Paged to first, then placed.** `page_the_shop_to` returns the offset it
                        // actually reached rather than the one it wanted, so a press that did not
                        // take leaves `slot_at` unable to place the item — and the `None` arm below
                        // refuses, exactly as it did when paging did not exist at all.
                        Some(i) => match crate::shopplay::slot_at(r.win, i, page_the_shop_to(r, i)) {
                            // There is no confirmation dialogue on this screen, so a slot we cannot
                            // place is a purchase of whatever else is sitting in it. That was the
                            // whole reason paging was left out, and it is still the answer when
                            // paging fails.
                            None => {
                                r.log.push_str(&format!(
                                    "  the heart is item {i}, and the shelf could not be paged to \
                                     it — nothing is pressed\n"
                                ));
                                false
                            }
                            Some((x, y)) => {
                                // **Empty the shelf, do not take one off it.** The dev's
                                // correction, 2026-08-15: settlements stock more than one heart and
                                // the single purchase was leaving them behind.
                                //
                                // Bounded by two numbers and the smaller wins. The shop printed the
                                // stock when it opened, and `hearts_affordable` is the purse with
                                // `INN_COST` held back — a reserve over the whole visit, not just
                                // the first item, so a shelf of four cannot spend us out of a bed.
                                //
                                // Same slot every time. `reduceStock` decrements in place
                                // (`shop.lua:101-106`) and the grid is rebuilt from the same list,
                                // so the heart does not move while we are buying it; and a click on
                                // a sold-out item does nothing, because `mousereleased` checks
                                // stock before it takes the gold
                                // (`ui/elements/shopitem.lua:147-165`). Overshooting is therefore
                                // harmless, which is what makes clicking to a count safe without a
                                // fresh dump between presses — there isn't one, the inventory is
                                // printed on open only.
                                let stock = crate::shopplay::stock_of(&inv, crate::shopplay::HEART);
                                let want = stock.min(r.map.hearts_affordable()).max(0);
                                r.log.push_str(&format!(
                                    "  the heart is item {i}, at ({x}, {y}) — {stock} in stock, \
                                     {} gold, buying {want}\n",
                                    r.map.gold()
                                ));
                                match r.win.client_to_screen(x, y) {
                                    Ok((sx, sy)) => {
                                        for _ in 0..want {
                                            let _ = r.tap("shop: buying a heart", sx, sy);
                                            std::thread::sleep(Duration::from_millis(700));
                                        }
                                        r.pump();
                                        want > 0
                                    }
                                    Err(e) => {
                                        r.log.push_str(&format!("  could not aim at it: {e}
"));
                                        false
                                    }
                                }
                            }
                        },
                    };

                    // Leave whatever happened. The back arrow is the mirror of `Sell` at the other
                    // end of the same bar, measured off the frame this screen was first captured on.
                    if let Ok((bx, by)) = r.win.client_to_screen(1755, 916) {
                        let _ = r.tap("shop: paging the shelf", bx, by);
                        std::thread::sleep(Duration::from_millis(900));
                        r.pump();
                    }

                    // **Confirmed from the save, not from the press.** `core.save` writes the whole
                    // `shopData` back to `areaFlags[<key>_shops]` when the shop closes
                    // (`shop.lua:303-309`), and `reduceStock` records what was bought under
                    // `purchased` (`:101-106`). So leaving is what makes the purchase readable, and
                    // this is the first moment it can be checked.
                    r.apply_save();
                    let spent = r.heart_is_recorded(&container);
                    r.log.push_str(&format!(
                        "  heart bought={bought}, and the save {}
",
                        match spent {
                            true => "agrees",
                            false => "does not show it yet",
                        }
                    ));
                    // Written off either way: a store that did not sell us one this time will not
                    // sell us one next time either, and standing here again is the bounce every
                    // other errand in this file has already had to learn not to do.
                    r.map.abandon(at);
                    if spent || bought {
                        r.map.bought_the_heart(&container);
                        // **A heart raises the ceiling, so buying one makes us hurt.** The dev,
                        // 2026-08-16: *after we buy the healthBuff, we should top up our health.*
                        //
                        // `healthBuff` adds four **maximum** health and no current health
                        // (`items/ephemeral.lua:4-9`), so a run that walked in full walks out at
                        // 20/24 — and at 20/28 after emptying a shelf of two, which is what
                        // happened at Stillingfleet before the run died in the next crypt. Nothing
                        // noticed, because the arrival branch that asks `top_up_at` runs when we
                        // *land* on a settlement and we are already standing in this one.
                        //
                        // The save is re-read first rather than trusted from before the purchase:
                        // `top_up_at` decides on `Health::is_full`, and the reading it would
                        // otherwise use was taken when the old maximum was still the maximum — a
                        // full bar that is no longer full. The purchase has already been confirmed
                        // from this same file, so the flush has happened.
                        if let Some(h) = r.apply_save() {
                            r.log.push_str(&format!("  health is now {}/{}\n", h.current, h.max));
                        }
                        if r.map.top_up_at(&container) {
                            r.log.push_str(
                                "  the heart raised the ceiling — going back for the bed\n",
                            );
                        }
                    }
                    continue;
                }
                r.log.push_str(&format!("{step}. at **{at}** in `{container}` — resting\n"));
                let rested = r.rest_at_inn();
                // **Written off only if it gave us nothing.** This used to abandon before trying,
                // which is what turned one missed press into "walk to the next village at 7/20":
                // the inn stopped being a destination, `seeking_a_rest` went false, and the crossing
                // routed out of the village.
                //
                // Abandoning still has to happen on failure, or `cross_toward` returns `Arrive`
                // here for ever. But a *partial* rest is progress, not failure — health went up, so
                // coming back round to the same inn is a loop with a monotone measure under it,
                // which is the one kind that terminates. It ends when the bar is full (`wants_rest`
                // clears) or the purse is empty (`inn_inside`'s gold gate stops nominating it), and
                // **A failure is counted, not taken as a verdict** — see [`Run::rest_failures`] —
                // and "nothing to buy" is not a failure at all. Collapsing those two is what made
                // arriving at full health cost three visits.
                match rested {
                    Rested::Healed => {
                        r.rest_failures.remove(at);
                    }
                    Rested::NothingToDo => {
                        r.rest_failures.remove(at);
                        // **The errand is over, and the inn just said so.** `wants_rest` otherwise
                        // clears only on *reading* full health, and the read that would do it is not
                        // on disk until we have left — `overworld:save()` runs in the inn's `goBack`
                        // (`ui/inn.lua:9`) — so the decision to come back is taken before the
                        // evidence lands. Live 2026-08-10: healed to full, walked out, walked
                        // straight back in, and opened the rest screen again to be told
                        // `healthNeed = 0`. This is that same `healthNeed`, read one screen earlier.
                        r.map.rest_errand_over();
                    }
                    Rested::Failed => {
                        let n = r.rest_failures.entry(at.clone()).or_insert(0);
                        *n += 1;
                        if *n >= REST_GIVE_UP {
                            r.log.push_str(&format!(
                                "  rest: {at} has given us nothing {n} times — writing it off\n"
                            ));
                            r.map.abandon(at);
                        } else {
                            r.log.push_str(&format!(
                                "  rest: nothing landed at {at} ({n} of {REST_GIVE_UP}) — trying again\n"
                            ));
                        }
                    }
                }
                // Re-read before anything plans on it: `overworld:save()` runs in the inn's
                // `goBack` (`ui/inn.lua:9`), so leaving is the moment the new health is readable.
                // This is what clears `wants_rest` and lets the run get on with the anomaly.
                if let Some(h) = r.apply_save() {
                    r.log.push_str(&format!(
                        "  health is now {}/{}{}\n",
                        h.current,
                        h.max,
                        match rested {
                            Rested::Healed => "",
                            Rested::NothingToDo => " (nothing left to buy)",
                            Rested::Failed => " (nothing was spent)",
                        }
                    ));
                }
                continue;
            }
            // **One press for as far as the town will carry us**, the crossing's counterpart to the
            // surface far hop. Computed here rather than in the arm because a match guard cannot
            // bind, and calling it twice to get the value out would be the same walk done twice.
            //
            // Only `Step` can use it: the other three arms exist precisely because no route is
            // known, and there is no chain to walk without one.
            //
            // The far node has to be **named in this dump**. On the surface a node the dump omits is
            // placed from the world frame; that is exactly what must not happen here, because
            // `lostOrientation` re-rolls every interior coordinate on re-entry
            // (`forest.lua:483-490`), so a remembered position inside a subworld is worth nothing.
            // A node the dump does not name falls back to the ordinary single step.
            // **Where the door came from, on every arm and not only `Step`.**
            //
            // Task #51. The `l4` loop of 2026-08-16 ran ten crossings toward `l4_path_to_l1` while
            // the errand was `Heart -> l4_path_to_l10` — a different door, and one the log itself
            // called "not on any route we know", which is *why* the crossing fell through to the
            // frontier walk. Two explanations fit: a `crossing_to` commitment held from an older
            // target (`committed_exit` compares the **goal** and not the target, and this run
            // emptied a shelf mid-crossing, which moves the target without touching the goal), or
            // `choose_exit` genuinely preferring `l1`.
            //
            // **The log could not tell them apart**, because `door_note` was printed only in the
            // `Step` arm — and `Probe` and `Seek` are the two arms a loop actually runs
            // in. One line, before the branch, so the next run answers it instead of the run after
            // that.
            //
            // `door_note` already marks a held commitment with `HELD`; printing the current plan
            // beside it is what makes a stale one visible, since the two disagreeing is the whole
            // symptom.
            r.log.push_str(&format!(
                "  door: {} | reason {} | plan now {doing}\n",
                r.map.door_note().unwrap_or_else(|| "none recorded".into()),
                r.map.door_reason().map(|d| d.why()).unwrap_or("held from earlier"),
            ));
            // **This looked in the wrong list, and so it never once fired.** — the dev, three times
            //
            // `far_hop_inside` ends in `far_chain`, whose last act is
            // `.filter(|k| !self.can_step_is_adjacent(from, k))` — one hop is not a multi-hop, so the
            // key it returns is **never adjacent to `here`**. This then looked for that key in
            // `fresh.nodes`, which is the dump's *adjacent connections* (`overworldview.lua:1030-1035`)
            // and is where `fold` builds `neighbours` from. So the two conditions are mutually
            // exclusive by construction: every node in `fresh.nodes` is adjacent, and the far hop is
            // never adjacent. `9922815` shipped a feature that could not run, and three live runs
            // crossing uncorrupted villages hop by hop is what that looks like from outside.
            //
            // **The right list was already in the dump.** `Subworld exit positions` prints every one
            // of the container's roads out, with coordinates, **at any distance**
            // (`overworldview.lua:1040-1047`) — that is the whole reason a crossing can steer toward
            // a door it cannot yet reach. And the door is exactly what a "leave the town in one
            // press" hop is aiming at: `Crossing::Step`'s `toward` is `<container>_path_to_<x>`.
            //
            // This satisfies the requirement the comment below states and the old code only appeared
            // to: the position is **printed in this dump**, not remembered. `lostOrientation`
            // re-rolls every interior coordinate on re-entry (`forest.lua:483-490`), so a remembered
            // interior position is worth nothing — and nothing here remembers one.
            //
            // Still guarded by [`crate::observe::hud::is_map_point`], because an exit is routinely
            // off-screen: one live dump put two of them at (2153, -3) and (1958, 1654). Off the map
            // we can click, the far hop is declined and the ordinary adjacent step stands.
            //
            // ## A chain that stops short of the door, which used to be given up on
            //
            // The paragraph here said that an ordinary interior node "is declined in silence,
            // because that is a node no dump gives us a position for and there is nothing to aim
            // at". True of a dump, and not true of a **visit**: the exits print at any distance and
            // therefore register every interior dump against the last one, so an interior frame
            // assembles across a crossing exactly as the surface frame assembles across a run. That
            // is [`crate::overworld::InsideFrame`], and the door case below is now its first branch
            // rather than its only one.
            //
            // What it cost to leave undone, measured: Enthorpe on 2026-08-21, four presses to reach
            // an inn three nodes inside a village whose interior was complete and uncorrupted. The
            // hop was computed correctly every time and thrown away for want of a coordinate.
            //
            // **The door is still tried first**, and not merely for tidiness. Its position is
            // printed by the dump in front of us; the frame's is inferred from two anchors and a
            // fitted scale. When both can answer, the one that cannot be wrong wins.
            //
            // ## #80: a probe has a route too, and it does not lead to the door
            //
            // This was `Crossing::Step` alone, on the grounds that *the other three arms exist
            // precisely because no route is known*. True of the **door** and false of the walk: a
            // `Probe` is a step along a route to a frontier the crossing has already chosen and
            // holds, which is [`crate::overworld::WorldMap::frontier_target`]. What it has no route
            // to is `toward`, and hopping that way would be hopping toward the very thing the probe
            // is deliberately not going to yet.
            //
            // Measured over the run of 2026-08-22: 42 probes, not one of which asked for a hop.
            // Every move in Bainton Clump was a `Probe`, so a chain existed at each of them and was
            // walked a node at a time. `Seek` is included on the same reasoning — it differs from
            // `Probe` only in having no door to name, and it holds a frontier just the same.
            //
            // ## And the farthest node we can see, rather than nothing
            //
            // A hop off the clickable map used to be dropped whole. In `l4` toward
            // `l4_path_to_shrine1` that happened six times in a row with the door travellable in one
            // press throughout, until it became the immediate next node and the *ordinary* path
            // panned it into reach — `panned by (0, -196)`. One refusal was 86 px past the right
            // edge with its y already on screen.
            //
            // The dev, 2026-08-22: *if it makes it more reliable and simpler, fast-hop to the
            // farthest node on the path that's still visible without panning.* So the chain is
            // walked from the far end and the first clickable node wins. Its worst case is that no
            // node qualifies and the ordinary adjacent step stands — which is exactly the old
            // behaviour — so this can add hops and never lose one. Panning here was the other
            // option and is the worse one: the ordinary path already pans, and a far hop that panned
            // would be aiming at a coordinate its own pan had just invalidated.
            let far_toward: Option<String> = match &mv {
                Crossing::Step { toward, .. } => Some(toward.clone()),
                Crossing::Probe { .. } | Crossing::Seek { .. } => {
                    r.map.frontier_target().map(str::to_string)
                }
                _ => None,
            };
            let far_inside: Option<(String, (f64, f64))> = far_toward.and_then(|toward| {
                let (cw, ch) = r.win.client_size().ok()?;
                // Why each was left, so a chain that comes back empty can still be told apart from
                // one that was never computed.
                let mut passed_over: Vec<String> = Vec::new();
                for f in r.map.far_hop_chain_inside(&here, &toward) {
                    // **The door is still tried first**, and not merely for tidiness. Its position
                    // is printed by the dump in front of us; the frame's is inferred from two
                    // anchors and a fitted scale. When both can answer, the one that cannot be
                    // wrong wins.
                    let drawn = f
                        .strip_prefix(&format!("{container}_path_to_"))
                        .and_then(|want| fresh.exits.iter().find(|e| e.to_key == want))
                        .map(|e| ((e.x, e.y), "its road is drawn at"))
                        .or_else(|| {
                            r.map
                                .inside_screen_position(&fresh, &f)
                                .map(|p| (p, "this visit's frame puts it at"))
                        });
                    // Nothing on screen and nothing in the frame. An unregistered dump, or a room we
                    // have never been adjacent to.
                    let Some(((x, y), how)) = drawn else {
                        passed_over.push(format!("`{f}` (nothing places it)"));
                        continue;
                    };
                    if crate::observe::hud::is_map_point(x as i32, y as i32, cw, ch) {
                        if !passed_over.is_empty() {
                            r.log.push_str(&format!(
                                "  passed over {} for `{f}`, which {how} ({x:.0}, {y:.0})\n",
                                passed_over.join(", ")
                            ));
                        }
                        return Some((f, (x, y)));
                    }
                    passed_over
                        .push(format!("`{f}` ({how} ({x:.0}, {y:.0}), off the map we can click)"));
                }
                if !passed_over.is_empty() {
                    r.log.push_str(&format!(
                        "  no node on the chain is clickable — {} — stepping instead\n",
                        passed_over.join(", ")
                    ));
                }
                None
            });
            let (what, at) = match &mv {
                Crossing::Leave { to } => {
                    match fresh.exits.iter().find(|e| &e.to_key == to) {
                        Some(e) => (format!("leaving `{container}` for `{to}`"), (e.x, e.y)),
                        None => return Stop::Failed(format!("exit to {to} not on screen")),
                    }
                }
                // **One press for as far as the town will carry us**, the crossing's counterpart to
                // the surface far hop below. Only `Step` gets it: the other three arms exist
                // precisely because no route is known, and there is no chain to walk without one.
                //
                // The far node's position has to come **from this dump**, and it does — see the
                // exits lookup above. On the surface a node absent from the dump is placed from the
                // world frame, and that is exactly what must not happen here: `lostOrientation`
                // re-rolls every interior coordinate on re-entry (`forest.lua:483-490`), so a
                // remembered position inside a subworld is worth nothing. A door this dump does not
                // draw somewhere clickable falls back to the ordinary single step.
                Crossing::Step { to, toward } if far_inside.is_some() => {
                    let (far, n) = far_inside.as_ref().expect("guarded");
                    (
                        format!(
                            "crossing `{container}` toward `{toward}` — `{far}` is travellable in \
                             one press at ({:.0}, {:.0}), so not via `{to}` hop by hop",
                            n.0, n.1
                        ),
                        *n,
                    )
                }
                // The same press, for a walk heading to a frontier rather than to a door. Named
                // apart from the arm above because the two are different journeys, and conflating
                // them in the report has cost this project a diagnosis every time it has happened.
                Crossing::Probe { to, .. } | Crossing::Seek { to } if far_inside.is_some() => {
                    let (far, n) = far_inside.as_ref().expect("guarded");
                    (
                        format!(
                            "probing `{container}`{} — `{far}` is travellable in one press at \
                             ({:.0}, {:.0}), so not via `{to}` hop by hop",
                            aiming_at(r),
                            n.0,
                            n.1
                        ),
                        *n,
                    )
                }
                // The door's reason and what it was ranked against used to be spliced in here, and
                // that is now the unconditional line above — three runs in a row turned on which
                // branch chose the door, and it was the arms *without* the line that needed it.
                Crossing::Step { to, toward } => match fresh.nodes.iter().find(|n| &n.key == to) {
                    Some(n) => {
                        (format!("crossing `{container}` toward `{toward}` via `{to}`"), (n.x, n.y))
                    }
                    None => return Stop::Failed(format!("{to} is not adjacent on screen from {here}")),
                },
                // **Separated from `Step`, which it used to share a line with.** The two mean
                // opposite things — one is a hop along a route, the other is a walk into the dark
                // because no route exists — and printing them alike cost a whole run's diagnosis.
                //
                // Live in `l2` on 2026-08-09: twenty-two consecutive lines read `crossing l2 toward
                // l2_path_to_l1 ... via l2subN`, which reads as a considered route across a village.
                // Every one of them was this branch. The exits section of a dump gives a door's
                // POSITION but never its key (`overworldview.lua:1041-1047`), so
                // `l2_path_to_l1` was not a node in our graph until a dump finally named it as a
                // neighbour — at the second-to-last step. The run explored the whole village because
                // routing to the door was not something it could do, and the log said otherwise.
                //
                // **And the frontier it is aiming at, since #57.** With two arms, the alternation
                // was the diagnosis; with one, a walk reads as coherent whether or not it is going
                // anywhere sensible, and the step alone cannot say. `via` is one hop; `for` is the
                // node the walk holds and will keep walking to until it arrives or it stops being
                // worth arriving at.
                Crossing::Probe { to, toward } => match fresh.nodes.iter().find(|n| &n.key == to) {
                    Some(n) => (
                        format!(
                            "`{toward}` is not on any route we know — probing `{container}` via `{to}`{}",
                            aiming_at(r)
                        ),
                        (n.x, n.y),
                    ),
                    None => return Stop::Failed(format!("{to} is not adjacent on screen from {here}")),
                },
                // Its own line, and deliberately not either of the two above. A search with no
                // destination at all is a third thing.
                //
                // Two ways to have no destination, and the log used to name only one of them. An inn
                // we have not found is the errand case; an exit the fog has not shown us is the
                // crossing case, and a lost woods makes it the common one. Reporting the second as
                // `searching e1 for its inn` had the run looking for a bar in the woods.
                Crossing::Seek { to } => match fresh.nodes.iter().find(|n| &n.key == to) {
                    Some(n) => (
                        match r.map.seeking_an_inn(&container) {
                            true => format!(
                                "searching `{container}` for its inn via `{to}`{}", aiming_at(r)
                            ),
                            false => format!(
                                "no way out of `{container}` in sight — probing via `{to}`{}",
                                aiming_at(r)
                            ),
                        },
                        (n.x, n.y),
                    ),
                    None => return Stop::Failed(format!("{to} is not adjacent on screen from {here}")),
                },
                Crossing::Fight { .. } | Crossing::Arrive { .. } => {
                    unreachable!("handled above")
                }
            };
            r.log.push_str(&format!("{step}. {what}\n"));
            // A node can be adjacent and still be somewhere we must not click. The dump reports
            // positions in screen space regardless of visibility, so an exit can sit off-screen or
            // under the HUD — and clicking one at (213, 18) opened the character screen, after which
            // the area-button coordinate meant `Stats` and the run spent its whole budget there.
            // Not fatal any more: the map can be scrolled until the node is somewhere clickable.
            // Ulrome is bigger than the window — from `l10sub6` the road to l19 sits at (169, 24)
            // under the inventory button, the road to l7 at x=2109, and two more below y=1660.
            //
            // The pan is measured rather than assumed. Scrolling is silent and clamped to the map
            // bounds (`clampWithinBoundsX`, `overworldview.lua:293-297`), so the delta we asked for
            // is an upper bound on the delta we got, and nothing announces the difference.
            let mut at = at;
            if let Ok((cw, ch)) = r.win.client_size() {
                // **One drag was one attempt too few.** [`pan_again`] carries the stopping rule: it
                // gives up the moment a pull gains nothing, which is what the map's own bound looks
                // like from here, and caps the rest.
                let mut spent = 0usize;
                let mut last = None;
                // A drag that happened but could not be measured. Not a failure by itself — see the
                // shared recovery below — but it does end this pulling loop, because `at` no longer
                // describes anything: the view moved by an unknown amount.
                let mut unmeasured = false;
                while pan_again(at, cw, ch, last, spent) {
                    let want = pan::shift_to_reach(at, cw, ch);
                    let Some(got) = r.pan_map(want) else {
                        unmeasured = true;
                        break;
                    };
                    // **A measurement that does not match its drag is not a measurement.**
                    //
                    // `measure` reports `None` when it cannot correlate; it cannot report a
                    // correlation that landed on the wrong piece of map, and that is what ended the
                    // run of 0436Z — 380 px sideways from a pull that asked for none, folded into
                    // `at`, and every later click aimed from the false position. See
                    // [`pan::Shift::agrees_with`].
                    //
                    // Routed into `unmeasured` rather than given a recovery of its own, because it
                    // *is* the same condition: the view moved by an amount we do not know. The cure
                    // written there — locate-me, which ends the motion and asks the game where the
                    // player is instead of reading the view — is exactly what an unknown position
                    // wants.
                    if !got.agrees_with(want) {
                        r.log.push_str(&format!(
                            "  the pan measured ({:.0}, {:.0}) against ({:.0}, {:.0}) asked — that \
                             is not this drag, so where `{what}` sits is unknown\n",
                            got.dx, got.dy, want.dx, want.dy
                        ));
                        unmeasured = true;
                        break;
                    }
                    at = pan::moved(at, got);
                    r.log.push_str(&format!(
                        "  panned by ({:.0}, {:.0}) of ({:.0}, {:.0}) wanted; `{}` now at ({:.0}, {:.0})\n",
                        got.dx, got.dy, want.dx, want.dy, what, at.0, at.1
                    ));
                    (last, spent) = (Some(got), spent + 1);
                }
                // **A pan that could not deliver is not the end of a run.** It used to be, and the
                // run of 2026-08-09 ended exactly here: the road out of `l2` sat at y = -130, the
                // pan asked for `(0, 164)` and measured `(38, -26)`, and that was that.
                //
                // That measurement has a documented shape — see [`Run::needs_recentre`], where a pan
                // asked for `(27, 0)` and measured `(28, -128)` because the game's own motion landed
                // inside our measurement. The cure recorded there is not a better detector but
                // locate-me, which ends the motion instead of reading around it and returns a dump
                // settled by construction. So take that cure here too rather than a second guess at
                // the same coordinate: re-centre, and derive the step again from a view we know is
                // still.
                //
                // Bounded, because a node the map genuinely will not show must still end the run
                // rather than spin.
                //
                // **Two ways in, and they want the same cure.** A pan that measured cleanly but did
                // not fetch the node is the case this branch was written for. A pan that could not
                // be **measured** is the other, and it used to end the run on the spot — even though
                // `pan::measure` returns `None` rather than a guess *precisely* so the caller can
                // re-establish position, and says so in its own doc. Ending the run was the one
                // response that never did. Live 2026-08-14: the road out of `l24` sat at (1381,
                // 1230), the first drag came back unmeasured, and that was the run.
                //
                // Locate-me answers both, because it does not read the view — it *ends* the motion
                // and asks the game where the player is. An unknown position is exactly what it
                // repairs, and re-centring also moves the target, so a node that no drag could fetch
                // may simply be on screen afterwards.
                if unmeasured || pan_again(at, cw, ch, None, 0) {
                    // **Before spending a retry: make the map smaller.**
                    //
                    // Dragging has two failure modes here and zooming answers both. A node below the
                    // window comes back inside it because everything halves toward the centre; and a
                    // patch that will not correlate stops mattering, because the fresh dump after
                    // the zoom carries new coordinates for everything rather than a measured delta.
                    // Task #29, and the dev's call after three runs died on the same exit.
                    //
                    // One press does the whole job. `pagedown` is bound to `scrollDown5`
                    // (`utils/defaultbinds/keyboard.lua:30`), which is `love.wheelmoved(0, -5)`
                    // (`main.lua:467`), and the overworld's handler is
                    // `core.setZoom(y > 0)` (`overworldview.lua:1529-1531`) — one step whatever the
                    // magnitude. A step out halves `targetZoomMul` and it is clamped at `0.5`
                    // (`:996`), so from the default `1` a single press reaches the floor and further
                    // presses are silent no-ops. That is why this fires once per run and not once
                    // per retry.
                    if r.zoom_out() {
                        continue;
                    }
                    if r.pan_retries >= MAX_PAN_RETRIES {
                        return Stop::Failed(match unmeasured {
                            true => format!(
                                "{what}: last seen at ({:.0}, {:.0}); the pan could not be measured \
                                 after {MAX_PAN_RETRIES} re-centres — position is unknown",
                                at.0, at.1
                            ),
                            false => format!(
                                "{what}: still out of reach at ({:.0}, {:.0}) after panning and \
                                 {MAX_PAN_RETRIES} re-centres",
                                at.0, at.1
                            ),
                        });
                    }
                    r.pan_retries += 1;
                    r.needs_recentre = true;
                    r.log.push_str(&match unmeasured {
                        // Deliberately "last seen at": after an unmeasured drag the coordinate is
                        // where the node *was*, and reporting it as current is how a reader ends up
                        // debugging a number that stopped meaning anything.
                        true => format!(
                            "  `{what}` was last seen at ({:.0}, {:.0}) and the pan could not be \
                             measured — re-centring to re-establish position (try {} of \
                             {MAX_PAN_RETRIES})\n",
                            at.0, at.1, r.pan_retries
                        ),
                        false => format!(
                            "  `{what}` is at ({:.0}, {:.0}) and panning did not fetch it — \
                             re-centring and deriving the step again (try {} of \
                             {MAX_PAN_RETRIES})\n",
                            at.0, at.1, r.pan_retries
                        ),
                    });
                    continue;
                }
                r.pan_retries = 0;
            }
            let Ok((ax, ay)) = r.win.client_to_screen(at.0 as i32, at.1 as i32) else {
                return Stop::Failed("coordinate conversion failed".into());
            };
            let _ = r.tap(&format!("crossing: select for {what}"), ax, ay);
            std::thread::sleep(Duration::from_millis(900));
            r.pump();
            // **`Travel` is not always what is on offer, and pressing it when it is not stalls the
            // run in silence.**
            //
            // A forest subnode holding enemies offers `Combat` and nothing else
            // (`overworld/generators/forest.lua:86-87`). There is no `Travel` on it to press, no
            // console line saying so, and the slot coordinate is shared — so the press lands on
            // nothing, `here` never changes, and the arrival wait runs out. That is how the run of
            // 2026-08-16 ended, four steers deep into a forest that had been peaceful the first time
            // we crossed it and had enemies put back on its roads by corruption.
            //
            // Fighting is the right answer rather than a detour, and it is the dev's standing
            // ruling: **for the MVP, stick to the path and fight through whatever is standing on
            // it** — the full argument is under `WorldMap::cross_toward`, where backing out was
            // tried and called off. `canTravelToDirect` needs one endpoint complete
            // (`overworldview.lua:1316-1321`), so an uncleared node on the route is not a thing to
            // route around; it is the toll.
            //
            // The press is handed straight back to the top of the loop, which is where `Pregame` and
            // `CombatEntered` already have handlers. Nothing here tries to predict which arrives.
            if r.combat_is_on_offer() {
                r.log.push_str(
                    "  the slot offers `Combat`, so this node is a fight and not a step — taking it\n",
                );
                if matches!(r.click_area_button("Combat"), Ok(true)) {
                    r.combat_expected = true;
                    continue;
                }
                // A press the diff could not see is not a press that failed — the pregame animates
                // in and an early frame is identical to the map it replaced. The fight branch's own
                // recovery covers this case; here it is enough to fall through and let the arrival
                // wait below decide, since a fight that did open will change `here` when it ends.
                r.log.push_str("  the Combat press showed no movement — waiting to see what arrived\n");
            } else {
                let _ = r.click_area_button("Travel (subworld)");
            }
            // Arrival, not pixels.
            //
            // A frame diff over one second called a *successful* move a failure: travel begins with
            // a walk animation that barely changes the screen in that window, so the verdict was
            // 0.002 while the player was in fact walking to the well. The run then reported the
            // village as uncrossable while standing somewhere new.
            //
            // `here` changing is the game's own statement that we moved, and the overworld path has
            // always used it. Text is cleared inside the wait because arrival raises lore and events,
            // and those hold back the dump that would tell us we arrived.
            //
            // **And an exit other than arriving**, which is #82. `handle_event` is called in here,
            // and answering a `[Combat]` choice arms `r.combat_expected` — at which point there is
            // no arrival coming, because the fight has replaced the walk. Without this the loop runs
            // its full timeout and then reports a failure that never happened.
            let by = Instant::now() + Duration::from_secs(30);
            let mut arrived = false;
            while Instant::now() < by && !arrived && !r.combat_expected {
                std::thread::sleep(Duration::from_millis(300));
                r.pump();
                r.clear_text_screen();
                r.handle_event();
                arrived = r.map.here().map(|h| h != here).unwrap_or(false);
            }
            if r.combat_expected && !arrived {
                r.log.push_str("  an event started a fight instead of a walk — handing back\n");
                continue;
            }
            if !arrived {
                return Stop::Failed(format!("no arrival after: {what}"));
            }
            r.log.push_str(&format!("  arrived at `{}`\n", r.map.here().unwrap_or("?")));
            continue;
        }

        // Standing on an unfinished fight: it cannot be walked past.
        //
        // `subworld_container` is the guard that stopped this walking into a forest. A container's
        // heading carries a level and reads exactly like a fight, but its area button ENTERS the
        // subworld (`getLocationButtons` tests `typeData.subworld` first). Clearing one means
        // fighting through its interior, which is a capability this run does not have.
        // ...but only when the subworld is where we are trying to GET TO. Standing on one we are
        // merely walking past is not a reason to stop.
        //
        // This branch used to fire on any container underfoot, and it ended a run that was doing
        // nothing wrong: at 0 health the planner correctly chose the `l7` campfire, the route there
        // crosses Ulrome, and arriving at Ulrome killed the run on the spot. The machinery to cross
        // a subworld already existed and had worked one step earlier in that very log —
        // `l10sub7` out to `l18` — because `cross_toward` runs when we are *inside*. We simply
        // never got inside, having stopped on the doorstep.
        //
        // Falling through instead lets the code below click the area button, which enters the
        // subworld (`getLocationButtons` tests `typeData.subworld` before `basicCombatZone`), after
        // which the next iteration is "inside" and the crossing logic takes over.
        if let Some(p) = place.as_ref() {
            // **A village we are hurt in front of is never a dead end.** Entering it is the errand,
            // not a detour on the way to somewhere else — so the "nowhere left to go" test must not
            // be allowed to answer for it. Without this the run reaches the rest it planned and
            // stops on the doorstep in exactly the state the rest was for: `next_target` excludes
            // `here`, and at low health with every remaining node hostile there may be no second
            // choice for it to name.
            //
            // ## This is also the ONLY way we learn a village can be entered at all
            //
            // `subworld_container` is set from a dump taken *inside* (`overworld.rs`), so it can
            // never be true for a village we have not already been in. Standing on the very rest
            // stop we crossed a forest to reach, the driver therefore knew nothing about it, walked
            // away, and `next_target` — which excludes `here` — nominated the next village along.
            // Live 2026-08-08: `l19 -> l27 (for l27, Rest)`, `l27 -> l19 (for l19, Rest)`, four
            // round trips between two adjacent villages, having just crossed `l9` to get to one.
            // Cycle number eight, and the first with a cause outside the routing.
            //
            // A village heading settles it where a forest's cannot. The deliberate rule against
            // inferring a container from its heading — `a_container_is_learned_from_being_inside_
            // it_not_from_its_heading` — exists because `Eight Timberland — level 4 forest` reads
            // exactly like a fight. `Dane village` does not: `village.lua:5,72` is
            // `typeName = 'village'` with `subworld = 'village'`, and `getLocationButtons`
            // (`overworldview.lua:461-468`) returns the subworld button set for it. So the area
            // button below is `Enter`, not `Combat`.
            //
            // **Corrupted is excluded**, and that is the whole of the risk here: `getAreaButtons`
            // is consulted *before* `subworld`, and a village under attack replaces the set
            // (`village.lua:371-395`). We do not know what those buttons are, so we do not press
            // them on spec.
            //
            // ## This is the narrow version of a general capability
            //
            // "Am I hurt, and is this a village" is the wrong question in the long run. The right
            // one is **does this container hold anything I currently want** — which is the same
            // question `WorldMap::cross_toward` answers inside, and answers just as narrowly, with
            // `inn_inside` and `seeking_a_rest` written around the inn specifically.
            //
            // The generic form is an *errand*: a predicate over interior places plus what to do on
            // arriving at one. `Goal::Rest` names the inn (`village.lua:341`); a shop errand names
            // the shop subnode and `buyer::wanted`, which already exists as a deliberate
            // pass-through stub. `Crossing::Arrive` would carry the errand and the driver dispatch
            // on it, instead of `Arrive` meaning "the inn" by construction.
            //
            // Deliberately not built yet — the MVP has one errand, and inventing the abstraction
            // around a single case is how you get the wrong abstraction. Task #18, raised by the dev
            // for the post-MVP shop work.
            // **Corruption is a level, not a locked door — `completed` is what says whether a fight
            // is still owed.** A corrupted village that has been cleared has its inn back; it is a
            // harder fight than an ordinary village and that is the whole of the difference.
            //
            // The planner already knew this — its rest-site filter reads `(!p.corrupted ||
            // p.completed)` (`overworld::plan`) — and this line did not, so the two disagreed: the
            // planner would route to a cleared corrupted village *for the rest* and the driver would
            // arrive and decline to take it. A disagreement between planner and driver is the shape
            // that produced the `shrine1 -> l10 -> shrine1` bounce, and the same omission is what
            // left `shrine1` unconsecrated for four runs — a filter testing `corrupted` without
            // asking `completed`.
            // **Two reasons to walk into a village now.** A bed, and a heart -- see
            // `WorldMap::wants_a_heart`. The gate is otherwise identical, corruption clause and all:
            // corruption is a level rather than a locked door, and a cleared corrupted village has
            // its shops back.
            //
            // **`is_settlement`, not `type_is("village")`.** A town is a settlement too, and the
            // planner's heart filter has always known that while this line did not — which is the
            // whole of the `l28 <-> l27` bounce the dev stopped by hand. See [`Place::is_settlement`].
            //
            // **And a bed is now worth stopping for whenever we are passing one.** The dev's rule,
            // 2026-08-15: *if we're at a settlement, have less than full health, and have the gold
            // to rest at the inn, go rest.* That is a weaker condition than `wants_rest`, which
            // waits for half health or a four-point drop — deliberately, because those are the bars
            // for making a *detour*. Standing in the doorway is not a detour, and ten gold for six
            // health before the next fight is the cheapest trade on the board.
            let top_up = r.map.top_up_at(&p.key);
            // **The bed and the shelf ask different questions of the same place** — task #75.
            //
            // `is_settlement` is `village || town`, the two nouns that can carry a `healthBuff`, and
            // that is the right bar for a heart. It is the wrong bar for a bed: `store_inn` is in
            // every settlement's roster (`overworld/generators/village.lua:684-685`), hamlets
            // included, so asking it here shut a hamlet's inn to a hurt run standing in its doorway.
            //
            // Kept as one `rest_here` because the block below it is one decision — whether to enter
            // — but the two reasons to enter no longer share a premise they never both had.
            let open_for_business = (!p.corrupted || p.completed)
                // Under attack or lost, every building in the village answers `Enter` with an empty
                // room or a `Loot` button — see [`crate::overworld::Place::trades`]. Entering to
                // rest or shop is then a walk across a subworld for nothing.
                && p.trades();
            let for_a_bed = p.has_an_inn()
                && ((r.map.wants_rest() && r.map.gold() >= crate::rest::INN_COST) || top_up);
            let for_a_heart =
                p.is_settlement() && r.map.wants_a_heart() && !r.map.heart_is_spent(&p.key);
            let rest_here = open_for_business && (for_a_bed || for_a_heart);
            if p.subworld_container || rest_here {
                let heading_for = r.map.next_hop().map(|h| h.plan.target);
                let stuck_here =
                    !rest_here && heading_for.as_deref().map(|t| t == here).unwrap_or(true);
                if stuck_here {
                    r.log.push_str(&format!(
                        "{step}. `{here}` ({}) is the destination and is a subworld — clearing it \
                         from the inside is not implemented\n",
                        p.heading
                    ));
                    return Stop::AtSubworld(here);
                }
                // **Only the rest case announces anything here, and that is #54.**
                //
                // The crossing case used to print "`l4` is a subworld on the way to `l20` — entering
                // to cross it" at this point, which is *before the decision to enter has been made*.
                // Nothing in this block presses anything for a crossing; the press happens further
                // down in the fight branch, and only if that branch fires. At step 47 of the 1752Z
                // run it did not — the far hop found `l20` one press away and travelled there
                // instead — so the log announced an entry that never happened and then, four lines
                // later, a travel to somewhere else under the same step number. It cost the dev a
                // question about why the run had entered a forest and immediately left, and the
                // answer was that it never went in.
                //
                // So the entry is announced where it is made. See the fight branch below, which
                // knows it is a crossing rather than a fight and now says so.
                if rest_here {
                    r.log.push_str(&format!(
                        "{step}. `{here}` ({}) is the rest we came for — entering it\n",
                        p.heading
                    ));
                }
                // **Press it here, rather than falling through and hoping.**
                //
                // Falling through worked only by accident, and only for a forest: the area button is
                // pressed further down inside the *fight* branch, which fires on `has_combat() &&
                // !completed`. A forest container's heading carries a level, so it qualified and the
                // `Combat` press turned out to enter the subworld. `Dane village` carries no level
                // and is already complete, so that branch could never fire — the run logged
                // "entering to cross it" and then travelled onward, which is the `l19 <-> l27`
                // bounce with a sentence of narration on top.
                //
                // `Enter` and `Combat` are the same click either way: `click_area_button` presses a
                // fixed position (`AREA_BUTTON`) and the label only reaches the log.
                //
                // **Only the rest case takes this path.** A container we are crossing still falls
                // through to the fight branch, because that is the code that crossed `l9` live on
                // 2026-08-08 and it handles an outcome this does not: the press may open a *pregame*
                // rather than a subworld, and telling those apart is the loop down there. Here the
                // node is a completed, uncorrupted village — there is no fight for it to start.
                if rest_here {
                    let inside_before = r.map.inside().map(str::to_string);
                    if !matches!(r.click_area_button("Enter"), Ok(true)) {
                        return Stop::Failed(format!("Enter did nothing at {here}"));
                    }
                    // Confirm by the *change*, never by "we are in a subworld" — inside a village
                    // that is already true before the click, and asking it that way is what once had
                    // a run report that it had entered somewhere it was standing in. Announcement is
                    // not readiness: the press is not done until the world says it moved.
                    let by = Instant::now() + Duration::from_secs(10);
                    loop {
                        r.pump();
                        if r.map.inside().map(str::to_string) != inside_before {
                            r.log.push_str(&format!(
                                "  inside `{}` now\n",
                                r.map.inside().unwrap_or("?")
                            ));
                            break;
                        }
                        if Instant::now() >= by {
                            return Stop::Failed(format!("no subworld after entering {here}"));
                        }
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    continue;
                }
            }
        }
        // Ask where we are GOING before deciding to fight. The first live run had these the other
        // way round: it saw an unfinished fight underfoot and cleared it unconditionally, when the
        // route ran straight back the way we came. `canTravelToDirect` needs one endpoint complete,
        // and the node behind us always is -- so leaving was legal all along, and the fight it
        // picked was one it walked into for nothing.
        let hop = r.map.next_hop();
        // Three ways this node has to be dealt with rather than walked past.
        //
        // 0. **The crossing said so** — `must_clear_here`, set above. `can_step` cannot answer this
        //    one: it asks whether *a* move off the node is legal, and after a retreat the answer is
        //    always yes, because the node we came from is complete.
        // 1. Leaving is illegal -- the original test, and the one that matters inside a subworld.
        // 2. **We came here on purpose.** `next_target` excludes `here`, so a node's own reason for
        //    existing evaporates the instant we stand on it; without this the run arrives at the
        //    fight it chose, re-plans to somewhere else, and is routed back. Legal to leave and
        //    pointless to leave are different questions and only the first was being asked.
        let arrived_at_target = r.committed_to.as_deref() == Some(here.as_str());
        let must_fight_here = arrived_at_target
            || must_clear_here
            || match hop.as_ref() {
                Some(h) => !r.map.can_step(&here, &h.step),
                None => true,
            };
        if let Some(p) = place
            .as_ref()
            .filter(|p| p.has_combat() && !p.completed && must_fight_here)
        {
            // The anomaly is fought like anything else. It is level 8 against a character that came
            // straight here, so losing is the likely outcome — and finding out how badly is the
            // point. A loss cannot be undone in the game, only by restoring a checkpoint.
            let is_anomaly = r.map.anomaly().map(|a| a.key == p.key).unwrap_or(false);
            // Nothing used to ask how much health was left before starting a fight, and a run found
            // out what that costs: it cleared `l41`, came out at **1 of 20**, and the planner — with
            // no campfire or village anywhere in its 22 known places — fell through to its documented
            // "get on with the objective" behaviour, walked to `l50`, and clicked Combat on a level-6
            // crypt. Nine turns later the console printed `game over`.
            //
            // The health-first priority was not broken. It had nowhere to route to, and "nowhere to
            // rest" is not a reason to fight anyway.
            //
            // Deliberately blunt: `rest::health_is_low` is below half, so this also refuses fights a
            // healthier judgement would take. That is the safe direction while nothing here models
            // enemy strength — the heading carries a level (`level 6 crypt`) and weighing it against
            // health is the better rule, once something reads it. Unknown health counts as hurt, for
            // the same reason it does when answering an event.
            //
            // ## Three exemptions, and the third is the one the dev added on 2026-08-08
            //
            // The **anomaly** is exempt. It is the objective, it is level 8 against whoever arrives,
            // and losing to it is a documented expected outcome rather than an accident to prevent.
            //
            // A **forest** is exempt, because entering one is not committing to a fight. A forest is
            // a subworld whose interior nodes are peaceful or not individually
            // (`competeOnVisit = subnodeIsPeaceful`, `forest.lua:109,123,137,…`), so crossing one
            // can cost nothing. `Risk::Forest` and not `type_is("forest")`: a **corrupted** forest
            // ranks `Corrupt`, and corruption puts the whole interior under attack
            // (`village.lua:371-395`).
            //
            // **A fight the crossing demands is exempt**, which is `must_clear_here` — we are inside
            // a subworld, standing on something that blocks departure, and the path goes through it.
            // The alternative is not "keep the health": it is to stop where we stand. A live run did
            // exactly that in front of a **level 1** spider nest at 1/20, twenty-four steps into a
            // forest it never crossed, and the dev's verdict was that occasionally bypassing combat
            // nodes has cost more than it is worth. For the MVP we stick to the path and fight
            // through what is on it.
            //
            // This is the same argument the deleted `no_way_round` made and could barely reach: it
            // needed us to have backed out of the node once and been routed straight back. Backing
            // out is gone — `WorldMap::cross_toward` has the account — so the observation is made
            // where it was always available, at the point the crossing says the node is the move.
            //
            // What is left gated is the case the gate was built for: **choosing** to walk onto
            // hostile ground while hurt. A run cleared `l41`, came out at 1 of 20, had no rest site
            // among its 22 known places, walked to `l50` and clicked Combat on a level 6 crypt. That
            // is `arrived_at_target`, not a crossing, and it still stops.
            //
            // The trade-off is real: a crossing that leads onto a level 4 guard post now takes it at
            // 1/20. Weighing the node's level against health is the better rule and is still not
            // implemented — see the open question on `easiest_hostile`.
            let enterable = p.risk() == crate::overworld::Risk::Forest;
            let too_hurt = health.map(crate::rest::health_is_low).unwrap_or(true)
                && !enterable
                && !must_clear_here;
            if too_hurt && !is_anomaly {
                let hp = health
                    .map(|h| format!("{}/{}", h.current, h.max))
                    .unwrap_or_else(|| "unreadable".into());
                r.log.push_str(&format!(
                    "{step}. **not fighting `{here}` ({}) at {hp}** — stopping instead of dying\n",
                    p.heading
                ));
                return Stop::TooHurtToFight(format!("{here} ({}) at {hp}", p.heading));
            }
            // **The press is the same; the word is not.** `getLocationButtons` tests
            // `typeData.subworld` before `basicCombatZone` (`overworldview.lua:462-467`), so on a
            // container this click is `Explore` or `Visit` and enters — it does not start a fight.
            // Calling that "fighting `l4`" was the other half of #54: one line claimed an entry that
            // did not happen, and this one described the entry that did as something else.
            let what = match () {
                _ if p.subworld_container => "entering",
                _ if p.is_chest() => "opening",
                _ => "fighting",
            };
            let toward = match p.subworld_container {
                true => hop
                    .as_ref()
                    .map(|h| format!(" to cross toward `{}`", h.plan.target))
                    .unwrap_or_default(),
                false => String::new(),
            };
            r.log.push_str(&format!(
                "{step}. {what} {}`{here}` ({}){toward}\n",
                if is_anomaly { "**THE ANOMALY** " } else { "" },
                p.heading
            ));
            // What that button opens is not ours to predict, so nothing here tries to.
            //
            // `getLocationButtons` tests `typeData.subworld` BEFORE `basicCombatZone`
            // (`overworldview.lua:462-467`), so the button labelled for a fight enters a forest or a
            // village instead whenever the node is one -- and their headings read exactly like
            // fights. A chest's button is `Open`, and goes straight into combat with no pregame at
            // all. Three outcomes from one press, and this used to try to tell them apart from a
            // single console announcement, which is why a chest ended a run.
            //
            // Now it presses, waits for the transition, and lets the top of the loop identify what
            // arrived -- where `Screen::Pregame` and `Screen::CombatEntered` already have handlers
            // that were being duplicated here. Being inside a subworld needs no handler at all: the
            // map path deals with it.
            let inside_before = r.map.inside().map(str::to_string);
            // **Read, not gated.** This branch presses the slot knowing full well it may not say
            // `Combat`: `getLocationButtons` tests `typeData.subworld` before `basicCombatZone`
            // (`overworldview.lua:462-467`), so the same press enters a forest (`Explore`) or a
            // village (`Visit`), and a chest offers `Open`. Requiring `Combat` here would refuse
            // every crossing we make.
            //
            // So `Combat` cannot be required here and never has been. What *can* be required is
            // that the slot holds a **live** button of some kind, which is a different question the
            // same measurement answers — greying costs more agreement than the lettering does, and
            // [`crate::act::AREA_BUTTON_LIVE`] is calibrated in the gap between the two.
            //
            // That is the half this branch was missing. Run `0251Z` step 39 logged
            // `area slot: something else (Combat 0.7367, gate 0.95)`, pressed, and stopped with
            // `Combat did not open at l38`. 0.7367 is exactly the corpus reading for a greyed
            // `Combat`, so the observer had identified the state and the only bar on offer was one
            // that a live `Travel` (0.8566) also fails.
            //
            // [`Run::look_for_a_live_slot`] re-selects and looks again, which is the recovery for a
            // stale slot, and then presses regardless: the bar is calibrated on three short words
            // and may not veto. See there.
            if !r.look_for_a_live_slot() {
                r.log.push_str(
                    "  nothing pressable after re-selecting — pressing anyway, since the live bar \
                     is calibrated on three planks and `Travel` is not one of them\n",
                );
            }
            r.snap_area_slot("combat-live");
            if !matches!(r.click_area_button("Combat"), Ok(true)) {
                // **A screen diff is a worse witness than the observer, so ask the observer.**
                //
                // `click_area_button` judges a press by how much the window changed one second
                // later. That is a reasonable *first* question and a terrible last one: the pregame
                // animates in, and a frame caught before it starts is identical to the map it
                // replaced. The dev has said the observer belongs here as the fallback, and they are
                // right — `identify` names the screen whatever the diff happened to catch.
                //
                // Live 2026-08-15 at `l16sub5`: `clicked Combat: screen moved 0.002`, run over.
                // `gave-up.png` from that stop is unmistakably the pregame — `Bursall Hedge — level
                // 2 road` across the top, `Start` at the bottom — with the scene behind it still
                // black because it had not finished rendering. The press had landed. Nothing looked.
                //
                // Bounded by the same [`COMBAT_OPENS_BY`] the other post-press re-look uses, so a
                // press that genuinely did nothing still ends the run rather than spinning.
                let by = Instant::now() + COMBAT_OPENS_BY;
                let mut opened = None;
                while Instant::now() < by {
                    let s = crate::act::identify(r.win);
                    if matches!(s, crate::act::Screen::Pregame | crate::act::Screen::CombatEntered) {
                        opened = Some(s);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
                match opened {
                    Some(s) => {
                        r.log.push_str(&format!(
                            "  the diff saw nothing but the observer found {s:?} — the press landed                              after all
"
                        ));
                        r.combat_expected = true;
                        continue;
                    }
                    None => {
                        r.snap_screen("combat-no-diff");
                        r.log_button_scores();
                    }
                }
                return Stop::Failed(format!("Combat did not open at {here}"));
            }
            r.combat_expected = true;
            r.settle_after_mode_change(inside_before);
            continue;
        }

        let Some(mut hop) = hop else { return Stop::NoPlan };
        // **One press for as far as the game will carry us.** The dev's ask, 2026-08-16.
        //
        // Deliberately *after* the fight decision above, which asks `can_step(&here, &h.step)` and
        // means "is a move off this node legal". Substituting a distant node before that would make
        // it answer no — because a distant node is not adjacent — and the run would fight whatever
        // it happened to be standing on. So the far hop is chosen once walking is settled, and never
        // changes what we do here, only how far one press takes us.
        //
        // `far_hop` returns `None` for anything an ordinary step would have handled, and the node it
        // names is on the same route `hop.step` starts — see there. The dump must still place it, so
        // a node the screen does not show falls back to the single step rather than failing.
        //
        // ## Aiming at a node the dump does not name
        //
        // A dump prints positions for **adjacent connections only** (`overworldview.lua:1030-1035`),
        // so a node two hops out has no coordinate in it. I read that as "it cannot be aimed at" and
        // the dev corrected it: the dump also carries several nodes we have *already placed*, and
        // any one of them fixes the camera exactly. That is [`WorldMap::registration`], which has
        // been building a world frame all along, and [`WorldMap::screen_position`] is the way back
        // out of it. Measured over 80 settled dumps: every shared node agrees on the shift to
        // 0.0000 px.
        //
        // So a far hop is aimed from the frame — **and then it has to be somewhere we can click.**
        //
        // ## The claim that used to be here was false, and it ended the run of 2026-08-16 2103Z
        //
        // It said "the pan machinery below fetches it onto the screen exactly as it does for a
        // neighbour that sits under the HUD". There is no pan machinery below. That loop lives in
        // the **crossing** branch, several hundred lines up; this branch selects and retries and
        // has never done anything else.
        //
        // A frame position is not a promise of visibility. It is where the node *is*, and the map is
        // bigger than the window: `shrine7` came out at **(1265, 1567)** on a 1080-tall client — 487
        // px below the bottom edge — and the run clicked it. The per-click confirmation did its job
        // (`moved the strip 0.0000`), three attempts were spent, a re-centre followed, `shrine7` was
        // not in the new dump either, and that was the run.
        //
        // So the placement is checked against [`crate::observe::hud::is_map_point`] before it is
        // used, which is the same check every dumped neighbour has passed since a road at (213, 18)
        // opened the character screen. Failing it costs one press's worth of ambition: `hop.step` is
        // still the ordinary adjacent step, which is on screen by construction, and the run walks
        // there instead. Panning the far target into view is the better answer and is #28's machinery
        // applied to this branch — worth doing, and not worth guessing at while a run is stopping
        // over it.
        let mut far_target: Option<crate::observe::adjacency::Node> = None;
        if let Some(far) = r.map.far_hop(&here, &hop.plan.target) {
            match fresh.nodes.iter().find(|n| n.key == far) {
                Some(n) => {
                    r.log.push_str(&format!(
                        "  the road to `{far}` is clear the whole way — one press instead of hop by hop\n"
                    ));
                    far_target = Some(n.clone());
                    hop.step = far;
                }
                // Not in this dump, so place it from the frame. `None` here means the frame cannot
                // speak for it — an unregistered island, or a dump sharing nothing with the frame —
                // and the ordinary single step is the answer, as it was before any of this.
                // Two ways to have no usable coordinate, and they are worth telling apart in the
                // log: the frame could not speak for the node at all, or it did and the answer is
                // somewhere no click may go. The first is an unregistered island; the second is an
                // ordinary consequence of the map being bigger than the window.
                // Bound before the `match` rather than used as its scrutinee, because the
                // off-window arm now needs `&mut r` and a scrutinee holding a borrow of `r.map`
                // would forbid it.
                None => {
                    let placed = r.map.screen_position(&fresh, &far).map(|(x, y)| {
                        // Unknown client size counts as unusable: aiming needs the window's
                        // measurements, and guessing at them is what this branch is being fixed for.
                        let clickable = r
                            .win
                            .client_size()
                            .map(|(cw, ch)| {
                                crate::observe::hud::is_map_point(x as i32, y as i32, cw, ch)
                            })
                            .unwrap_or(false);
                        ((x, y), clickable)
                    });
                    match placed {
                    Some(((x, y), false)) => {
                        r.log.push_str(&format!(
                            "  `{far}` is travellable in one press but the frame puts it at \
                             ({x:.0}, {y:.0}), which is off the map we can click\n"
                        ));
                        // **Make the map smaller rather than give up the hop.**
                        //
                        // The dev, 2026-08-17: *fast-hopping to the shrine after the anomaly opened
                        // also did not work; we did adjacent hops the slow way again.* The run of
                        // 0436Z declined `shrine7` four times running — placed at y = 1566, 1538,
                        // 1440, 1235 against a 1080-px window — and walked it one node at a time.
                        // The hop was computed correctly every time; only the aiming failed.
                        //
                        // **Zoom, not drag, and the reason is staleness.** Dragging the node into
                        // view is #56 and would leave every *other* coordinate in `fresh` shifted by
                        // an amount we measured rather than knew — including `hop.step`'s, which is
                        // where we fall back to. Zooming invalidates them honestly:
                        // [`Run::zoom_out`] sets `positions_stale_at` and `needs_recentre`, so the
                        // `continue` below re-derives the whole step from a dump taken after the
                        // change. Nothing is carried across it. That is the same reasoning the
                        // crossing branch already records where it reaches for the zoom first.
                        //
                        // It cannot spin: `zoom_out` is once per run — a step out halves
                        // `targetZoomMul`, clamped at `0.5` (`overworldview.lua:996`), so from the
                        // default `1` the first press is already at the floor — and it returns
                        // `false` thereafter, which falls through to the ordinary step exactly as
                        // before. Halving toward the centre is also enough for the numbers above:
                        // 1566 comes back to about 1053.
                        if r.zoom_out() {
                            continue;
                        }
                        r.log.push_str(&format!(
                            "  and the map is already as small as it goes — stepping to `{}` \
                             instead\n",
                            hop.step
                        ));
                    }
                    Some(((x, y), true)) => {
                        r.log.push_str(&format!(
                            "  `{far}` is travellable in one press and is not in this dump — placed \
                             from the world frame at ({x:.0}, {y:.0})\n"
                        ));
                        far_target = Some(crate::observe::adjacency::Node {
                            key: far.clone(),
                            heading: r.map.get(&far).map(|p| p.heading.clone()).unwrap_or_default(),
                            x,
                            y,
                            connections: r.map.get(&far).map(|p| p.connections).unwrap_or(0),
                        });
                        hop.step = far;
                    }
                    // **Say WHICH of the three, because they are three different repairs.** The
                    // line here used to read "the frame cannot place it", which reads as too few
                    // anchors and sends the reader at #21 — and in two of the three runs that
                    // produced this branch the frame was usable at every surface dump they took.
                    // See [`crate::overworld::WorldMap::unplaceable`].
                    None => {
                        let why = r.map.unplaceable(&fresh, &far);
                        r.log.push_str(&format!(
                            "  `{far}` is travellable in one press but cannot be aimed at: \
                             {why} — stepping to `{}` instead\n",
                            hop.step
                        ));
                    }
                    }
                }
            }
        }
        let hop = hop;
        let found = far_target.or_else(|| fresh.nodes.iter().find(|n| n.key == hop.step).cloned());
        let Some(target) = found else {
            return Stop::Failed(format!("{} is not on screen from {here}", hop.step));
        };
        // The suffix says whether this step is *on the way* or merely *the way it lies*. Without it
        // a run heading nowhere reads exactly like a run heading somewhere -- five identical
        // `(for start, Anomaly)` lines were read as a journey and were five guesses.
        // **And what the places are called**, which this line never said. #84, from the dev asking
        // *why did we enter the Cotham Botscage level 9 forest* — a question the report could not
        // answer, because `194. l27 -> **l30** (for l30, Explore)` gives the key and the goal and
        // the heading appears only on the `fighting` and `entering` lines. The heading is where the
        // level lives (`AreaHeading`, `overworldview.lua:388-389`), so without it a report cannot
        // say what a step cost or why a destination was worth it.
        //
        // The destination is named separately only when it is not the step itself, which on a
        // routed journey is most of them and is exactly the case the question was about.
        let named = |key: &str| match r.map.get(key).map(|p| p.heading.clone()).unwrap_or_default() {
            h if h.trim().is_empty() => String::new(),
            h => format!(" *{h}*"),
        };
        let toward = match hop.plan.target == hop.step {
            true => String::new(),
            false => named(&hop.plan.target),
        };
        r.log.push_str(&format!(
            "{step}. {here} -> **{}**{} (for {}{}, {:?}){}\n",
            hop.step,
            named(&hop.step),
            hop.plan.target,
            toward,
            hop.plan.reason,
            match hop.routed {
                true => "",
                false => " — no route there; stepping the way it lies",
            }
        ));
        // **Whether exploring was actually steered, and by what.** An `Explore` line looks identical
        // whether the corruption pulled the choice or nearest-unvisited did, which is how a steering
        // rule that had stopped working went unnoticed. `placed of total` is the honest part: a
        // bearing that can measure two candidates out of nine is barely steering at all.
        if let Some((toward, placed, total)) = &hop.plan.steered_by {
            r.log.push_str(&format!(
                "  steering toward `{toward}` ({placed} of {total} candidates placed)
"
            ));
        } else if matches!(
            hop.plan.reason,
            crate::overworld::Goal::Explore | crate::overworld::Goal::RouteTo(_)
        ) {
            // Both shapes of exploring, because the silent one is the interesting one: a `RouteTo`
            // with nothing to aim by is a hop that knows its errand and is walking at random.
            r.log.push_str("  not steered — exploring by hops alone
");
        }
        // What this hop is *for*, so that arriving there is recognised as arriving. See
        // [`Run::committed_to`].
        r.committed_to = Some(hop.plan.target.clone());

        // Select, and prove it: the lone area button is `affirmative`, and at a combat node that
        // button is Combat. Space without a confirmed selection starts a fight.
        //
        // Retried, because a missed click is a *coordinate* failure and coordinates can be renewed.
        // The node position comes from an adjacency dump, and a dump describes the map at the instant
        // it was printed — see `recentre` for the three ways the map moves without announcing it. So
        // when a click lands on empty ground the useful response is not to give up but to ask the
        // game where things are now, which is exactly what pressing the arrow does.
        //
        // A miss is cheap to detect and ruinous to miss: `affirmative` acts on
        // `overworldview.getMousePressedOn()` (`overworld.lua:1355-1357`), so with nothing selected
        // the Space that follows has no subject and travel never starts. The run that taught us this
        // then sat in the arrival wait for its full sixty seconds and died, having printed "the
        // affirmative slot is empty" two hundred times on the way.
        let mut selected = false;
        let mut at = (target.x as i32, target.y as i32);
        // **Pay back the pan we skipped, because this click is not at a fixed coordinate.**
        //
        // See [`Run::skipped_the_pan`]. The shortcut is taken on the strength of the next action
        // being a press on the area slot; a hop is the case where that turned out to be wrong, and
        // it is the only one that needs the camera still. Cheaper than it looks — it happens on the
        // steps the shortcut fired on and nowhere else.
        if std::mem::take(&mut r.skipped_the_pan) {
            let (cw, ch) = r.win.client_size().unwrap_or((1920, 1080));
            r.log.push_str("  this step skipped the pan and now wants a node — settling first\n");
            if let Some(a) = r.recentre().filter(|a| !camera_is_lost(&a.nodes, cw, ch)) {
                if let Some(n) = a.nodes.iter().find(|n| n.key == hop.step) {
                    at = (n.x as i32, n.y as i32);
                }
            }
        }
        for attempt in 1..=SELECT_RETRIES {
            let Ok(before) = crate::win::capture::capture_window(r.win) else {
                return Stop::Failed("capture failed".into());
            };
            let Ok((sx, sy)) = r.win.client_to_screen(at.0, at.1) else {
                return Stop::Failed("coordinate conversion failed".into());
            };
            let _ = r.tap(&format!("travel: select `{}`", hop.step), sx, sy);
            std::thread::sleep(Duration::from_millis(900));
            r.pump();
            let Ok(after) = crate::win::capture::capture_window(r.win) else {
                return Stop::Failed("capture failed".into());
            };
            let moved = before.diff_fraction(&after, AREA_BUTTONS);
            if moved > SELECT_MOVED {
                selected = true;
                break;
            }
            r.log.push_str(&format!(
                "  selecting {} at ({}, {}) moved the strip {moved:.4}, attempt {attempt} of {SELECT_RETRIES}\n",
                hop.step, at.0, at.1
            ));
            if attempt == SELECT_RETRIES {
                break;
            }
            // Fresh coordinates, from a dump that is now required to be newer than the arrow press —
            // and to describe a view the player is actually in. Retrying a failed selection against
            // a camera that has not caught up spends all `SELECT_RETRIES` aiming at the same wrong
            // place, which reads in the log as the click not registering.
            let (cw, ch) = r.win.client_size().unwrap_or((1920, 1080));
            match r.recentre().filter(|a| !camera_is_lost(&a.nodes, cw, ch)) {
                Some(a) => match a.nodes.iter().find(|n| n.key == hop.step) {
                    Some(n) => {
                        at = (n.x as i32, n.y as i32);
                        r.log.push_str(&format!(
                            "  re-centred; `{}` is now at ({}, {})\n",
                            hop.step, at.0, at.1
                        ));
                    }
                    None => {
                        r.log.push_str(&format!(
                            "  re-centred, but `{}` is not in the new dump\n",
                            hop.step
                        ));
                        break;
                    }
                },
                None => {
                    r.log.push_str("  re-centre produced no fresh dump\n");
                    break;
                }
            }
        }
        if !selected {
            return Stop::Failed(format!(
                "selecting {} did not register after {SELECT_RETRIES} attempts",
                hop.step
            ));
        }
        r.keys.focus();
        std::thread::sleep(Duration::from_millis(200));
        if !r.tap_key(&format!("travel: press Travel for `{}`", hop.step), VK_SPACE, SC_SPACE) {
            return Stop::Failed("could not send Travel".into());
        }
        r.park();

        let by = Instant::now() + Duration::from_secs(60);
        let mut arrived = false;
        // `!r.combat_expected` is #82: `handle_event` below arms it when the choice it answers is a
        // `[Combat]` one, and from that moment no arrival is coming — the fight has replaced the
        // walk. Answering the Highwayman near Ulrome on 2026-08-22, this loop took 162 readings of
        // an empty affirmative slot and then timed out; `CombatEntered` was named on the next pass
        // of the outer loop, which is where the answer had been all along.
        while Instant::now() < by && !arrived && !r.combat_expected {
            std::thread::sleep(Duration::from_millis(300));
            r.pump();
            // Text before options here too. Arrival is detected from an adjacency dump, and a lore
            // screen holds that dump back — so without this the loop would spin out its full 60 s
            // waiting for a map that cannot be drawn until the text is gone.
            r.clear_text_screen();
            r.handle_event();
            // **The named node, not merely a different one.**
            //
            // This used to be `h != here`, which is right for a single hop and wrong the moment one
            // press covers several: `core.arriveAt` runs at *every* node on the path
            // (`overworldview.lua:1210-1216`), so the first intermediate arrival would end the wait
            // while the avatar was still walking, and the next step would plan from a node we were
            // about to leave.
            arrived = r.map.here().map(|h| h == hop.step).unwrap_or(false);
        }
        if arrived {
            r.hop_misses = 0;
        }
        if r.combat_expected && !arrived {
            r.log.push_str("  an event started a fight on the way — handing back to the observer\n");
            continue;
        }
        // **Short of the named node is progress, not failure.** An event on the way pauses the walk
        // (arrivals raise lore and choices), and a fight stops it outright. `here` is correct either
        // way and the top of the loop re-plans from it, so anything that moved us is a step taken.
        // Only standing still is a failure.
        if !arrived {
            let moved = r.map.here().map(|h| h != here).unwrap_or(false);
            if !moved {
                // **A press that took us nowhere is a setback, not the end of the run.**
                //
                // The dev, 2026-08-20: *add safeguards so that something as a failed click can be
                // recovered. We have already built other recovery methods like re-centering.* This
                // is that, and it is the same cure the selection loop above already reaches for.
                //
                // Two things can put us here and a locate-me answers both. The Travel may simply
                // have been ignored. Or the *selection* only looked like it landed: that check is a
                // screen diff over the area strip (`SELECT_MOVED`), and a diff cannot tell a strip
                // that changed because a node was selected from one that changed because the camera
                // was still gliding — which is exactly the state a step arrives in when it took the
                // shortcut past the pan wait.
                //
                // Re-centring settles the view and re-prints every position, so the retry aims at
                // coordinates that describe the screen rather than the one before it.
                r.hop_misses += 1;
                if r.hop_misses < MAX_HOP_MISSES {
                    r.log.push_str(&format!(
                        "  pressed Travel for `{}` and did not move (try {} of {MAX_HOP_MISSES}) — \
                         re-centring and planning again\n",
                        hop.step, r.hop_misses
                    ));
                    r.needs_recentre = true;
                    continue;
                }
                return Stop::Failed(format!(
                    "no arrival at {} after {MAX_HOP_MISSES} tries",
                    hop.step
                ));
            }
            r.hop_misses = 0;
            r.log.push_str(&format!(
                "  stopped at `{}` on the way to `{}` — re-planning from where we stand\n",
                r.map.here().unwrap_or("somewhere"),
                hop.step
            ));
        }
        let now = r.apply_save();
        if let (Some(b), Some(a)) = (*health, now) {
            r.map.note_health(b, a);
            r.map.rested(a);
        }
        *health = now;
        let _ = Goal::Explore;
    }
    Stop::Exhausted
}
