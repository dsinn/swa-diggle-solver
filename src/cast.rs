//! Casting the spell a word was scored for — **groundwork, not yet pressed by Diggle**.
//!
//! The user, 2026-11-07: *I intend for Diggle to cast a spell in most cases* — and the search already
//! scores it as if it does ([`crate::spells::SpellBook::choose`]). This is what pressing the button
//! takes, worked out and tested as far as can be without driving the game.
//!
//! ## What happens on Enter
//!
//! `wordboard.submit` builds the option list (`getMagicWordPowers`, `wordboard.lua:168-209`). One
//! option and Enter plays it; two or more open the **"Submit as:"** panel and wait
//! (`wordboard.lua:483-490`). A second Enter does not confirm — `affirmative` re-submits, re-opening
//! the same panel (`rpg.lua:235-238`). Only a click on a panel or a `castSpellN` key casts.
//!
//! ## Which option: predicted from the gear and the word
//!
//! [`crate::spells::SpellBook::cast_slot`] says which slot holds the spell, and whether a panel opens
//! at all. The order is fixed by the code, not the data: the attack first, then the spells sorted by
//! their flag (`sortedpairs`). **Dictionary updates do not break it**: which words a spell takes is
//! read from the game's own word lists at run time ([`crate::lexica`]), so a new version's lists are
//! followed. What would break it is a **new spell** — a version or a mod adding a file to
//! `rpg/effects/spell/` — and that is detected rather than guessed:
//! [`crate::spells::unmodelled_spells`] reads the folder's keys and names any the save holds that we
//! do not model. While it is non-empty, a word's slots are not to be trusted.
//!
//! Two more ways the prediction could miss, neither in play today:
//!
//! - `submitWildcardRegex` lets a wildcard word qualify for a spell by pattern (`checkBlind`,
//!   `wordboard.lua:187-189`), adding entries the plain check would not.
//! - String order: Lua compares strings with `strcoll`, the same as bytes in the C locale LÖVE runs
//!   in, and the flags are ASCII.
//!
//! ## Pressing it: a number key
//!
//! `castSpell1`…`castSpell10` call `rpgcast.castIndex(i)` (`rpg.lua:278`), bound by default to `1`…`9`
//! and `0` (`utils/defaultbinds/keyboard.lua:32-41`). Diggle types in combat already, so a key needs
//! no screen geometry — the user's point, 2026-11-07: number keys make click areas unnecessary.
//! [`cast_key`] finds the key from the game's own binds.
//!
//! **The key must only be pressed with the panel open.** The same digits are `itemActivate1`…`10`
//! in the next layer, and the game tries each layer until one acts (`main.lua:480-503`):
//! `castIndex` does nothing while the panel is closed, so the press falls through and **uses an
//! item**. A wrong prediction of "the panel opens" would spend a consumable. So the plan is:
//!
//! 1. Enter. 2. Confirm the panel is up — a template of its backboard,
//!    `ui/graphics/bg/submitboard.png`, read from the game so a restyle follows, drawn at panel
//!    (444, 0) on an 1800x648 canvas centred at (0.5, 0.5625) of the window
//!    (`ui/elements/rpgcast.lua:2-3,80`, `buildDrawDataTable`, `main.lua:255-278`). 3. Press the
//!    key. 4. Confirm the board moved, as a word does now.
//!
//! The panel also says what it offers: each option is drawn with the icon of the item granting the
//! spell, ten times up from its 32-pixel sprite (`rpgcast.lua:96-99,147-150`), and `attack-32.png`
//! for the plain attack. Those icons could check the prediction before the press, if it ever
//! proves wrong in practice.
//!
//! ## A player's own binds can lack the keys
//!
//! The user's config predates `castSpellN`: its digits are `itemActivateN` only, so on that config
//! no key casts, and [`cast_key`] says so (`None`) rather than naming a digit that would use an item.
//! Diggle plays in its own save folder ([`crate::game::savedir::unfused`]), which has the game's
//! defaults until something writes a config there. Where no key exists, the panel's options are
//! 350 pixels apart, centred (`rpgcast.lua:144-155`) — a click is the fallback, not built.

use crate::game::save::{Table, Value};
use std::path::Path;

/// The key that casts option `slot` (from 0) of an open "Submit as" panel, or `None` when no key
/// does so safely.
///
/// The binds are the player's (`keybinds` in `userConfig`, in `save_dir`), else the game's
/// defaults (`utils/defaultbinds/keyboard.lua`, read from `game_dir`). A key counts only if it is
/// bound to `castSpell{slot+1}` with no modifier, and **no earlier layer binds it** to anything
/// else: the game stops at the first layer whose function acts (`main.lua:480-488`), so an earlier
/// `itemActivate` on the same key would use the item even with the panel open.
pub fn cast_key(game_dir: &Path, save_dir: &Path, slot: usize) -> Option<String> {
    let layers = binds(game_dir, save_dir)?;
    key_for(&layers, &format!("castSpell{}", slot + 1))
}

/// The binding layers, the player's or the game's.
fn binds(game_dir: &Path, save_dir: &Path) -> Option<Vec<Table>> {
    let user = std::fs::read_to_string(save_dir.join("userConfig"))
        .ok()
        .and_then(|src| crate::game::save::parse(&src).ok())
        .and_then(|cfg| cfg.table_at("keybinds").cloned());
    let table = match user {
        Some(t) => t,
        None => {
            let src =
                std::fs::read_to_string(game_dir.join("utils/defaultbinds/keyboard.lua")).ok()?;
            crate::game::save::parse_module(&src).ok()?
        }
    };
    Some(layers_of(&table))
}

/// A bind table's layers, in order.
fn layers_of(t: &Table) -> Vec<Table> {
    t.arr
        .iter()
        .filter_map(|v| match v {
            Value::Table(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

fn key_for(layers: &[Table], function: &str) -> Option<String> {
    for (i, layer) in layers.iter().enumerate() {
        for (key, v) in &layer.map {
            if v.as_str() != Some(function) {
                continue;
            }
            let modded = layer.table_at("_mod").is_some_and(|m| m.map.contains_key(key));
            let shadowed = layers[..i].iter().any(|earlier| earlier.map.contains_key(key));
            if !modded && !shadowed {
                return Some(key.clone());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn game() -> PathBuf {
        PathBuf::from("../sternly-worded-adventures")
    }

    fn dir(name: &str, config: Option<&str>) -> PathBuf {
        let d = std::env::temp_dir().join(format!("diggle-cast-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let _ = std::fs::remove_file(d.join("userConfig"));
        if let Some(c) = config {
            std::fs::write(d.join("userConfig"), c).unwrap();
        }
        d
    }

    /// The game's defaults: `1`…`9` cast slots 0…8, `0` slot 9, and there is no eleventh.
    #[test]
    fn the_defaults_cast_on_the_number_keys() {
        let d = dir("defaults", None);
        assert_eq!(cast_key(&game(), &d, 0).as_deref(), Some("1"));
        assert_eq!(cast_key(&game(), &d, 2).as_deref(), Some("3"));
        assert_eq!(cast_key(&game(), &d, 9).as_deref(), Some("0"));
        assert_eq!(cast_key(&game(), &d, 10), None);
    }

    /// The user's config predates the cast binds: digits use items, so no key casts.
    #[test]
    fn a_config_without_cast_binds_has_no_key() {
        let d = dir(
            "old",
            Some(
                r#"return { keybinds = {
                    { ["return"] = "hotspotSelect", _mod = {} },
                    { ["1"] = "itemActivate1", ["2"] = "itemActivate2", _mod = {} },
                } }"#,
            ),
        );
        assert_eq!(cast_key(&game(), &d, 0), None);
    }

    /// A cast bind behind an item bind on the same key would use the item: not safe.
    #[test]
    fn a_shadowed_cast_bind_is_refused() {
        let d = dir(
            "shadowed",
            Some(
                r#"return { keybinds = {
                    { ["1"] = "itemActivate1", _mod = {} },
                    { ["1"] = "castSpell1", ["f"] = "castSpell2", _mod = {} },
                } }"#,
            ),
        );
        assert_eq!(cast_key(&game(), &d, 0), None, "1 uses an item first");
        assert_eq!(cast_key(&game(), &d, 1).as_deref(), Some("f"), "f is free");
    }
}
