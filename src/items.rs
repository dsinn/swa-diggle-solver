//! What kind of thing each item is, read from the game's own `items/*.lua`.
//!
//! Exists to answer one question at the "Choose one:" screen: of the two or three keys the console
//! just printed, which is worth taking? The keys are readable (`goldIdol`, `armourLeatherGloves`)
//! but the *kind* is not encoded in them, and the screen shows nothing we can read for it — see
//! [`crate::itemchoice`] for why identification there is console-only.
//!
//! Every item declares its kind: `items/rewardaugmentgear.lua:4` has `goldIdol = { type = 'gear' }`,
//! `items/resistancegear.lua:138` has `armourLeatherGloves = { type = 'passive' }`. Across the
//! directory there are 17 distinct values, dominated by `gear` (153), `passive` (109),
//! `consumable` (52) and `curse` (46).
//!
//! ## Why this is scanned and not evaluated
//!
//! The files open with `local utils = require'utils.items'` and then call into it —
//! `class = get'class'.exclude{…}`, `purchaseFunction = get'give'.gear`, `highlightIf = function()
//! return rpgview.getEnemyHealth() == 0 end`. They are Lua *programs* that happen to return a
//! table, and they will not evaluate outside the game's environment. Shimming `require` the way
//! [`crate::lexica`] does works there because a dictionary descriptor only needs the module *name*;
//! here the shim would have to supply callables for every component the files touch, and get them
//! right, to learn a string that is sitting in plain sight on its own line.
//!
//! So: a line scan, brace-depth aware. A `type = '…'` is attributed to whichever `key = {` opened
//! the table it sits in, at whatever depth that is — see [`scan`] for why depth cannot be fixed in
//! advance. Comments and string literals are skipped so a `{` inside either cannot shift the count.
//!
//! Anything that does not fit — items built by a constructor call (`items/scrollsmisc.lua`'s
//! `utils.makeScroll(…)`) or keyed by a computed string (`items/fuel.lua`'s
//! `things[('scrapwood%d'):format(i)]`) — simply has no entry. That is the right failure: an unknown
//! kind ranks below a known-useful one rather than being guessed at. It also costs little here:
//! every `gear` and `passive` item written with a literal key is reached — 257 of them — and the
//! five that are not are loop-built variants of items already in the catalogue
//! (`items/golddropmults.lua`'s `goldDropMult%d`, `items/alchemypieces.lua`'s refill states). What
//! the scan misses in bulk is potions, scrolls and fuel, which rank as unusable either way. The test
//! verifies this by a second, independent method — owner by indentation rather than by brace depth.

use std::collections::HashMap;
use std::path::Path;

/// Item key -> the `type` string the game declares for it.
pub struct Catalogue {
    /// Key -> (depth it was declared at, kind). The depth is kept so a shallower declaration in a
    /// later file still wins — see [`scan`].
    kinds: HashMap<String, (i32, String)>,
    /// Key -> (depth, `icon` path), on the same shallowest-wins rule as [`Catalogue::kinds`].
    ///
    /// The depth rule earns its keep here rather than being a formality. `items/boardshapes.lua:43`
    /// declares `hench` with a nested `boardData = { boardGraphics = { icon = … } }` — a *board*
    /// picture two levels down, filed under `boardGraphics`, while the item's own 32-pixel icon sits
    /// at the surface. Reading the deeper one would hand [`crate::heroselect`] an image that is
    /// never drawn on a champion card.
    icons: HashMap<String, (i32, String)>,
    /// Files that could not be read, so a missing kind is visible rather than assumed absent.
    problems: Vec<String>,
}

impl Catalogue {
    /// Reads every `items/*.lua` under `game_dir`.
    pub fn load(game_dir: &Path) -> Result<Self, crate::Error> {
        let dir = game_dir.join("items");
        let mut kinds = HashMap::new();
        let mut icons = HashMap::new();
        let mut problems = Vec::new();
        for e in std::fs::read_dir(&dir)?.filter_map(|e| e.ok()) {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("lua") {
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(src) => scan(&src, &mut kinds, &mut icons),
                Err(err) => problems.push(format!("{}: {err}", path.display())),
            }
        }
        Ok(Self { kinds, icons, problems })
    }

    /// The `icon` path an item declares, relative to the game directory.
    ///
    /// `ui/elements/herodisplay.lua:196` blits exactly this file, with no scale arguments, for
    /// every entry in a champion card's rows — which is what makes a card readable without OCR.
    pub fn icon(&self, key: &str) -> Option<&str> {
        self.icons.get(key).map(|(_, p)| p.as_str())
    }

    /// The declared `type` of an item, or `None` if the scan never saw one for that key.
    pub fn kind(&self, key: &str) -> Option<&str> {
        self.kinds.get(key).map(|(_, k)| k.as_str())
    }

    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    pub fn problems(&self) -> &[String] {
        &self.problems
    }
}

/// Walks one file, recording `key -> (depth, type)` for every table that declares a kind.
///
/// The rule is *not* "top-level entry". Half the directory does not have one: `items/potions.lua`
/// assigns to a `local itemList` and then fills it from `do … end` blocks, and `items/fuel.lua`
/// builds its five stack sizes in a `for i=1,5` loop. Anchoring to the returned table reached 307 of
/// the 528 declarations and missed whole files.
///
/// So instead: a `type = '…'` belongs to whichever key opened the table it sits in, at whatever
/// depth that is. A stack of openers, one slot per depth, is all that takes.
///
/// The depth is kept alongside the kind because nested tables carry kinds too —
/// `items/fuel.lua:26-28` has `craft = { type = 'consumableMerge' }` inside an item. Those are
/// filed under `craft`, which is harmless while nothing asks for an item by that name, but it does
/// mean a real item key could be overwritten by a field of the same name deeper in some other file.
/// **Shallower wins**, so the item body — always nearer the surface than a field of one — keeps the
/// entry.
fn scan(
    src: &str, kinds: &mut HashMap<String, (i32, String)>,
    icons: &mut HashMap<String, (i32, String)>,
) {
    let mut depth = 0i32;
    // `openers[d]` is the key that opened depth `d+1`, or None where we could not read one (a
    // computed key such as `things[('scrapwood%d'):format(i)] = {`).
    let mut openers: Vec<Option<String>> = Vec::new();
    let mut in_block_comment = false;
    for raw in src.lines() {
        let line = strip_comments(raw, &mut in_block_comment);
        // Read the fields before moving the depth: they belong to the table already open. A line
        // that both opens a table and declares one cannot occur, because [`string_field`] requires
        // the line to *start* with the field name — which is what keeps an inline
        // `craft = { type = 'x' }` off the enclosing item.
        if depth >= 1 {
            if let Some(Some(k)) = openers.get((depth - 1) as usize) {
                for (field, out) in
                    [(string_field(&line, "type"), &mut *kinds), (string_field(&line, "icon"), &mut *icons)]
                {
                    if let Some(v) = field {
                        let e = out.entry(k.clone()).or_insert((depth, v.clone()));
                        if depth < e.0 {
                            *e = (depth, v);
                        }
                    }
                }
            }
        }
        let delta = brace_delta(&line);
        if delta > 0 {
            openers.push(opens_table(&line));
            for _ in 1..delta {
                openers.push(None);
            }
        } else {
            for _ in 0..-delta {
                openers.pop();
            }
        }
        depth += delta;
    }
}

/// Removes `--` line comments and `--[[ … ]]` blocks, leaving string literals intact.
fn strip_comments(line: &str, in_block: &mut bool) -> String {
    let b: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if *in_block {
            if c == ']' && b.get(i + 1) == Some(&']') {
                *in_block = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if let Some(q) = quote {
            out.push(c);
            if c == '\\' {
                if let Some(&n) = b.get(i + 1) {
                    out.push(n);
                    i += 2;
                    continue;
                }
            } else if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            out.push(c);
            i += 1;
            continue;
        }
        if c == '-' && b.get(i + 1) == Some(&'-') {
            if b.get(i + 2) == Some(&'[') && b.get(i + 3) == Some(&'[') {
                *in_block = true;
                i += 4;
                continue;
            }
            break; // rest of the line is a comment
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Net `{` minus `}`, ignoring braces inside string literals.
///
/// Item descriptions and icon paths are strings, and at least one file writes table constructors
/// inside a quoted example, so counting raw braces would drift the depth for the rest of the file.
fn brace_delta(line: &str) -> i32 {
    let mut d = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in line.chars() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '{' => d += 1,
            '}' => d -= 1,
            _ => {}
        }
    }
    d
}

/// `bezoar = {` -> `Some("bezoar")`. Rejects quoted keys (`['00011'] = {`), which never carry a
/// `type`, and anything whose right-hand side is a call rather than a table.
fn opens_table(line: &str) -> Option<String> {
    let (name, rest) = line.trim().split_once('=')?;
    let name = name.trim();
    if name.is_empty() || !rest.trim_start().starts_with('{') {
        return None;
    }
    let mut chars = name.chars();
    if !chars.next()?.is_ascii_alphabetic() {
        return None;
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(name.to_string())
}

/// `type = 'gear'` -> `Some("gear")` for `field = "type"`. Must not match `typeName = …`, hence the
/// `=` check after the prefix rather than a bare `starts_with`.
///
/// The line must *start* with the field name, which is what keeps an inline
/// `craft = { type = 'x' }` off the enclosing item — see [`scan`].
fn string_field(line: &str, field: &str) -> Option<String> {
    let rest = line.trim().strip_prefix(field)?.trim_start().strip_prefix('=')?.trim_start();
    let q = rest.chars().next()?;
    if q != '\'' && q != '"' {
        return None;
    }
    let body = &rest[q.len_utf8()..];
    let end = body.find(q)?;
    Some(body[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn game_dir() -> PathBuf {
        PathBuf::from("../sternly-worded-adventures")
    }

    fn present() -> bool {
        game_dir().join("items/rewardaugmentgear.lua").is_file()
    }

    /// Scans one source and flattens away the depth, which only matters for collisions.
    fn scanned(src: &str) -> HashMap<String, String> {
        let mut out = HashMap::new();
        scan(src, &mut out, &mut HashMap::new());
        out.into_iter().map(|(k, (_, t))| (k, t)).collect()
    }

    /// As [`scanned`], for the icon paths.
    fn scanned_icons(src: &str) -> HashMap<String, String> {
        let mut out = HashMap::new();
        scan(src, &mut HashMap::new(), &mut out);
        out.into_iter().map(|(k, (_, t))| (k, t)).collect()
    }

    /// The shape of a real entry, abridged from `items/resistancegear.lua:4-27`: nested tables
    /// before and after the `type`, a call with a table argument, and a description in quotes.
    #[test]
    fn reads_the_kind_out_of_a_real_shaped_entry() {
        let got = scanned(
            "local utils = require'utils.items'\n\
             local get = utils.getComponent\n\
             return {\n\
             \x20   bezoar = {\n\
             \x20       name = 'Bezoar',\n\
             \x20       type = 'gear',\n\
             \x20       usefulness = {\n\
             \x20           combat = true,\n\
             \x20       },\n\
             \x20       class = get'class'.exclude{\n\
             \x20           apothecary = true,\n\
             \x20       },\n\
             \x20   },\n\
             \x20   gloves = {\n\
             \x20       type = 'passive',\n\
             \x20   },\n\
             }",
        );
        assert_eq!(got.get("bezoar").map(|s| s.as_str()), Some("gear"));
        assert_eq!(got.get("gloves").map(|s| s.as_str()), Some("passive"));
    }

    /// A `type` nested one level deeper belongs to the inner table, not the item. The real shape is
    /// `items/fuel.lua:26-28`, where a `craft` block declares `consumableMerge` inside a
    /// `consumable` item; reading the inner one as the item's kind would rank the item wrongly.
    #[test]
    fn a_nested_type_is_not_the_items_kind() {
        let got = scanned(
            "return {\n\
             \x20   thing = {\n\
             \x20       craft = {\n\
             \x20           type = 'reagent',\n\
             \x20       },\n\
             \x20       type = 'consumable',\n\
             \x20   },\n\
             }",
        );
        assert_eq!(got.get("thing").map(|s| s.as_str()), Some("consumable"));
        // Filed under its own key rather than discarded. Harmless — nothing asks for an item named
        // `craft` — and it is the collision this arrangement has to survive, tested next.
        assert_eq!(got.get("craft").map(|s| s.as_str()), Some("reagent"));
    }

    /// If a field shares a name with a real item, the item must win. Depth decides: an item body is
    /// always nearer the surface than a field inside one.
    #[test]
    fn a_deeper_namesake_does_not_overwrite_an_item() {
        let mut out = HashMap::new();
        // Read in the order that loses if depth is ignored: the deep one first.
        scan(
            "return {\n\
             \x20   host = {\n\
             \x20       goldIdol = {\n\
             \x20           type = 'reagent',\n\
             \x20       },\n\
             \x20   },\n\
             }",
            &mut out,
            &mut HashMap::new(),
        );
        scan(
            "return {\n\x20   goldIdol = {\n\x20       type = 'gear',\n\x20   },\n}",
            &mut out,
            &mut HashMap::new(),
        );
        assert_eq!(out.get("goldIdol").map(|(_, t)| t.as_str()), Some("gear"));
    }

    /// A brace inside a string must not shift the depth for everything after it.
    #[test]
    fn braces_inside_strings_do_not_move_the_depth() {
        let got = scanned(
            "return {\n\
             \x20   one = {\n\
             \x20       description = 'Write it as {a, b} to win.',\n\
             \x20       type = 'scroll',\n\
             \x20   },\n\
             \x20   two = {\n\
             \x20       type = 'potion',\n\
             \x20   },\n\
             }",
        );
        assert_eq!(got.get("one").map(|s| s.as_str()), Some("scroll"));
        assert_eq!(got.get("two").map(|s| s.as_str()), Some("potion"), "depth survived the string");
    }

    #[test]
    fn comments_are_ignored() {
        let got = scanned(
            "return {\n\
             \x20   one = {\n\
             \x20       -- type = 'gear',\n\
             \x20       --[[ { { { ]]\n\
             \x20       type = 'curse',\n\
             \x20   },\n\
             }",
        );
        assert_eq!(got.get("one").map(|s| s.as_str()), Some("curse"));
    }

    #[test]
    fn typename_is_not_type() {
        assert_eq!(string_field("typeName = 'gear',", "type"), None);
        assert_eq!(string_field("type = 'gear',", "type"), Some("gear".into()));
    }

    /// The icon a champion card blits is the item's own, not the tile board's.
    ///
    /// Abridged from `items/boardshapes.lua:43-85`, which is the real hazard: `hench` carries a
    /// board picture at `boardData.boardGraphics.icon` two levels down *and* its own 32-pixel icon
    /// at the surface. [`crate::heroselect`] matches the latter against a card's `Passives:` row, so
    /// taking the deeper one would search for an image the card never draws.
    #[test]
    fn a_nested_board_icon_does_not_displace_the_items_own() {
        let got = scanned_icons(
            "return {\n\
             \x20   hench = {\n\
             \x20       type = 'passive',\n\
             \x20       boardData = {\n\
             \x20           boardGraphics = {\n\
             \x20               icon = 'ui/graphics/tileboard/icon-5x3-hex3-16.png',\n\
             \x20           },\n\
             \x20       },\n\
             \x20       icon = 'ui/graphics/items/class-hench-32.png',\n\
             \x20   },\n\
             }",
        );
        assert_eq!(
            got.get("hench").map(|s| s.as_str()),
            Some("ui/graphics/items/class-hench-32.png"),
            "the shallower declaration must win"
        );
        assert_eq!(
            got.get("boardGraphics").map(|s| s.as_str()),
            Some("ui/graphics/tileboard/icon-5x3-hex3-16.png"),
            "and the deeper one is filed under the table that opened it, not lost"
        );
    }

    /// The keys [`crate::heroselect`] names must still resolve to art that is on disk.
    #[test]
    fn every_champion_marker_still_has_its_picture() {
        if !present() {
            eprintln!("SKIP: game source not present at {}", game_dir().display());
            return;
        }
        let cat = Catalogue::load(&game_dir()).expect("the item catalogue should load");
        for marker in crate::heroselect::MARKERS {
            let icon = cat
                .icon(marker.item)
                .unwrap_or_else(|| panic!("`{}` declares no icon", marker.item));
            assert!(
                game_dir().join(icon).is_file(),
                "`{}` points at {icon}, which is not on disk",
                marker.item
            );
        }
    }

    /// An item built by a constructor call has no literal kind, and must not inherit the previous
    /// item's.
    #[test]
    fn a_constructor_built_item_is_absent_rather_than_wrong() {
        let got = scanned(
            "return {\n\
             \x20   plain = {\n\
             \x20       type = 'gear',\n\
             \x20   },\n\
             \x20   built = utils.makeScroll('x', 'y', 10, {'a','b'}, 'ee', 'text'),\n\
             }",
        );
        assert_eq!(got.get("plain").map(|s| s.as_str()), Some("gear"));
        assert_eq!(got.get("built"), None);
    }

    /// Against the real directory: the two keys a live reward screen offered us, and — the part that
    /// matters — **every** `gear` and `passive` declaration accounted for.
    ///
    /// Only those two kinds are asserted because only those two change a decision. The scan reaches
    /// 308 of the directory's 528 declarations; the ~220 it does not are the constructor-built
    /// potions, scrolls and fuel stacks, which rank as unusable whether they are found or not. A
    /// count over all kinds would therefore fail loudly for no consequence, and a floor would not
    /// notice the one regression worth catching: a gear or passive item silently going missing.
    #[test]
    fn every_gear_and_passive_item_is_found() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        let cat = Catalogue::load(&game_dir()).unwrap();
        assert!(cat.problems().is_empty(), "problems: {:?}", cat.problems());
        // `items/rewardaugmentgear.lua:4` and `items/resistancegear.lua:138`.
        assert_eq!(cat.kind("goldIdol"), Some("gear"));
        assert_eq!(cat.kind("armourLeatherGloves"), Some("passive"));

        // Cross-checked by a *second, independent* method: find each declaration's owner by
        // indentation rather than by counting braces. If the two disagree, one of them is wrong and
        // the test says which line.
        let (mut checked, mut computed) = (0, Vec::new());
        for e in std::fs::read_dir(game_dir().join("items")).unwrap().filter_map(|e| e.ok()) {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("lua") {
                continue;
            }
            let src = std::fs::read_to_string(&p).unwrap();
            let lines: Vec<&str> = src.lines().collect();
            for (i, l) in lines.iter().enumerate() {
                let t = l.trim();
                let kind = if t.starts_with("type = 'gear'") {
                    "gear"
                } else if t.starts_with("type = 'passive'") {
                    "passive"
                } else {
                    continue;
                };
                let indent = l.len() - l.trim_start().len();
                // The owner is the nearest line above that is outdented from this one.
                let owner = lines[..i].iter().rev().find(|p| {
                    !p.trim().is_empty() && p.len() - p.trim_start().len() < indent
                });
                let Some(owner) = owner.map(|o| o.trim()) else { continue };
                match opens_table(owner) {
                    Some(key) => {
                        assert_eq!(
                            cat.kind(&key),
                            Some(kind),
                            "{}:{} declares {key} as {kind}",
                            p.file_name().unwrap().to_string_lossy(),
                            i + 1
                        );
                        checked += 1;
                    }
                    // A computed key — `gold[key] = {` in a `for i=0,200,10` loop. Unreachable by
                    // any scan of the text, and every one of them is a variant of an item already
                    // in the catalogue, so there is nothing to recover.
                    None => computed.push(format!(
                        "{}:{} {owner}",
                        p.file_name().unwrap().to_string_lossy(),
                        i + 1
                    )),
                }
            }
        }
        assert!(checked >= 250, "expected hundreds of literal-key items, checked {checked}");
        // Pinned so that a *new* unreadable shape shows up as a failure rather than passing quietly.
        assert_eq!(computed.len(), 5, "computed-key declarations: {computed:#?}");
    }
}
