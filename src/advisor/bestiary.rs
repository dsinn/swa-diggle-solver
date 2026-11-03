//! Which enemy types a name belongs to, read from the game's own enemy and name files.
//!
//! `combatSaveData` records the enemy's name and statuses but not its type or categories
//! (`rpgview.lua:2566-2580`); on resume the game rebuilds the enemy from the area's seeded list. Its
//! name is the clue: each enemy type draws names from a list — `name = require'rpg.names.skeleton'`
//! (`rpg/enemies/skeletons.lua:25`) — so a name points to the lists it appears in, and those to the
//! enemy types that use them.
//!
//! Only "is it undead" is asked today, for Turn undead (`rpg/effects/spell/TurnUndead.lua:22-25`).

use crate::game::save::{parse_module, parse_module_named, Table, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Debug, Default)]
pub struct Bestiary {
    /// Name list module (`rpg.names.skeleton`) → the names in it.
    lists: HashMap<String, HashSet<String>>,
    /// Name list module → whether each enemy type drawing from it is undead.
    undead_by_list: HashMap<String, Vec<bool>>,
    /// Files that would not evaluate. Their enemies are unknown.
    pub problems: Vec<String>,
}

impl Bestiary {
    pub fn load(game_dir: &Path) -> Self {
        let mut b = Bestiary::default();
        let files = |dir: &str| -> Vec<std::path::PathBuf> {
            std::fs::read_dir(game_dir.join(dir))
                .map(|d| {
                    d.flatten()
                        .map(|e| e.path())
                        .filter(|p| p.extension().is_some_and(|x| x == "lua"))
                        .collect()
                })
                .unwrap_or_default()
        };
        for path in files("rpg/enemies") {
            let parsed = std::fs::read_to_string(&path)
                .map_err(crate::Error::from)
                .and_then(|src| parse_module_named(&src));
            match parsed {
                Ok(t) => b.collect_enemies(&t),
                Err(e) => b.problems.push(format!("{}: {e}", path.display())),
            }
        }
        for path in files("rpg/names") {
            let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().to_string()) else {
                continue;
            };
            let Ok(t) = std::fs::read_to_string(&path)
                .map_err(crate::Error::from)
                .and_then(|s| parse_module(&s))
            else {
                continue;
            };
            let names = t.arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
            b.lists.insert(format!("rpg.names.{stem}"), names);
        }
        b
    }

    /// Every table in an enemy file that names a list and has categories is an enemy type.
    fn collect_enemies(&mut self, t: &Table) {
        if let (Some(list), Some(_)) = (t.str_at("name.__module"), t.table_at("categories")) {
            let undead = t.path("categories.undead").is_some();
            self.undead_by_list.entry(list.to_string()).or_default().push(undead);
        }
        for v in t.arr.iter().chain(t.map.values()) {
            if let Value::Table(inner) = v {
                self.collect_enemies(inner);
            }
        }
    }

    /// The names in one list, for tests and for reading.
    pub fn names_in(&self, list: &str) -> Vec<String> {
        let mut v: Vec<String> = self.lists.get(list).into_iter().flatten().cloned().collect();
        v.sort();
        v
    }

    /// Is an enemy with this name undead? `None` when the name is in no list an enemy draws from,
    /// or the lists it is in disagree.
    pub fn undead(&self, name: &str) -> Option<bool> {
        let verdicts: Vec<bool> = self
            .lists
            .iter()
            .filter(|(_, names)| names.contains(name))
            .filter_map(|(list, _)| self.undead_by_list.get(list))
            .flatten()
            .copied()
            .collect();
        match verdicts.first() {
            Some(&first) if verdicts.iter().all(|&v| v == first) => Some(first),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bestiary() -> Bestiary {
        Bestiary::load(std::path::Path::new("../sternly-worded-adventures"))
    }

    /// Skeletons draw their names from `rpg/names/skeleton.lua` and are all undead
    /// (`rpg/enemies/skeletons.lua`).
    #[test]
    fn a_skeleton_name_is_undead() {
        assert_eq!(bestiary().undead("Bones McCoy"), Some(true));
    }

    /// The control: a slime's name is known, and it is not undead.
    #[test]
    fn a_slime_name_is_not_undead() {
        let b = bestiary();
        let slime = b.names_in("rpg.names.slime").into_iter().next().expect("slime names load");
        assert_eq!(b.undead(&slime), Some(false), "{slime}");
    }

    #[test]
    fn an_unknown_name_is_unknown() {
        assert_eq!(bestiary().undead("Nobody In Particular"), None);
    }

    /// Every enemy file evaluates: a file that silently failed would make its enemies unknown.
    #[test]
    fn every_enemy_file_loads() {
        let b = bestiary();
        assert!(b.problems.is_empty(), "{:?}", b.problems);
    }
}
