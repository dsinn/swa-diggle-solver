//! Buying at a general store, where the console does the reading and arithmetic does the aiming.
//!
//! ## The shop is the one screen we barely have to look at
//!
//! `shop.onActive` prints `Opened shop UI` and then the whole inventory through `table.repr`
//! (`shop.lua:248-256`), in the same serialisation `mainSaveData` uses — so the stock arrives on the
//! feed we already scrape, as a table our own save parser reads. Live 2026-08-15 at Rowlston Covert:
//!
//! ```text
//! Opened shop UI
//! Shop inventory = {
//!     { type = "healthBuff",  stock = 1 },
//!     { type = "antivenom",   stock = 3 },
//!     …
//! ```
//!
//! ## And the grid is arithmetic, not templates
//!
//! `shop.lua:287-292` lays the items out in a fixed double loop:
//!
//! ```lua
//! for y=0.3, 0.6, 0.3 do
//!     for x=0.245, 0.7551, 0.17 do
//!         table.insert(core.objects, require'ui.elements.shopitem'(x, y, index+relativeIndex))
//! ```
//!
//! Eight slots, two rows of four, at plain `ss` multipliers — the same scheme as every other button
//! this project clicks. At 1920x1080 that is x ∈ {470, 797, 1123, 1450}, y ∈ {324, 648}, which the
//! captured frame confirms to the pixel: `Heart` sits centred on the first, and the dump's order is
//! the screen's order left to right.
//!
//! ## One click buys
//!
//! `shopitem`'s `mousereleased` (`ui/elements/shopitem.lua:147-165`) checks stock, gold and the
//! item's `purchaseFunction`, then gives the item, reduces the stock and subtracts the cost. There is
//! no confirmation dialogue, which means a misaimed click is a purchase of something else — so this
//! aims from the index the game printed rather than from anything read off the screen.

use crate::win::window::GameWindow;

/// The item grid, from `shop.lua:287-292`. Fractions of the window, in the `ss` sense.
const COL: [f64; 4] = [0.245, 0.415, 0.585, 0.755];
const ROW: [f64; 2] = [0.3, 0.6];

/// How many items are on screen at once, and therefore the page size: the arrows step
/// `relativeIndex` by 8 (`shop.lua:205-232`).
pub const PER_PAGE: usize = COL.len() * ROW.len();

/// The paging arrows, in the same window fractions as the grid.
///
/// `shop.lua:202-236` declares **four** of them — a left and a right at `y = 0.3`, and the same pair
/// again at `y = 0.6` — and all four run the identical body, `relativeIndex = relativeIndex ± 8`
/// followed by `refreshButtons(true)`. So there is nothing to choose between them and this uses the
/// upper pair.
///
/// Both carry `showIf = function() return #shopInventory>8 end`, so on a short shelf they are not
/// drawn at all and the coordinate holds nothing. Their `activeIf` bounds the ends: a right arrow on
/// the last page is inactive and a press does nothing, which is a state the caller has to detect
/// rather than assume away — see [`crate::navigate`]'s paging, which verifies by watching the grid
/// change.
pub const ARROW_LEFT: (f64, f64) = (0.085, 0.3);
pub const ARROW_RIGHT: (f64, f64) = (0.915, 0.3);

/// The grid region, as a client rectangle `(x, y, w, h)` — what a page turn visibly changes.
///
/// Wide enough to hold all eight slots and their labels, and clear of both the arrows (`x = 0.085`
/// and `0.915`) and the edge preview items `refreshButtons` draws at `x = 0` and `x = 1`. Those
/// previews shift with the page too, so including them would still be correct; leaving them out
/// keeps the comparison about the thing being bought.
pub fn grid_region(win: &GameWindow) -> Option<(i32, i32, i32, i32)> {
    let (cw, ch) = win.client_size().ok()?;
    let x0 = (cw as f64 * 0.16).round() as i32;
    let x1 = (cw as f64 * 0.84).round() as i32;
    let y0 = (ch as f64 * 0.20).round() as i32;
    let y1 = (ch as f64 * 0.78).round() as i32;
    Some((x0, y0, x1 - x0, y1 - y0))
}

/// The `relativeIndex` that puts 1-based inventory `index` on screen.
///
/// Always a multiple of [`PER_PAGE`], because the arrows only ever move in whole pages.
pub fn page_of(index: usize) -> usize {
    index.saturating_sub(1) / PER_PAGE * PER_PAGE
}

/// Where an arrow is, in client pixels.
pub fn arrow_at(win: &GameWindow, arrow: (f64, f64)) -> Option<(i32, i32)> {
    let (cw, ch) = win.client_size().ok()?;
    Some(((cw as f64 * arrow.0).round() as i32, (ch as f64 * arrow.1).round() as i32))
}

/// Where the item at 1-based inventory `index` is drawn, given the current page offset.
///
/// `None` when it is not on this page — the caller pages first rather than clicking a slot that
/// holds something else. There is no confirmation dialogue in this screen, so "something else" means
/// "bought something else".
pub fn slot_at(win: &GameWindow, index: usize, page_offset: usize) -> Option<(i32, i32)> {
    let (cw, ch) = win.client_size().ok()?;
    let n = index.checked_sub(1)?.checked_sub(page_offset)?;
    if n >= PER_PAGE {
        return None;
    }
    let x = (cw as f64 * COL[n % COL.len()]).round() as i32;
    let y = (ch as f64 * ROW[n / COL.len()]).round() as i32;
    Some((x, y))
}

/// The inventory as the game just printed it, newest dump first.
///
/// Parsed with the save reader, because `table.repr` is `print(table.serialize(...))` and
/// `table.serialize` is what writes `mainSaveData` (`utils/table.lua:344-374`). The label is
/// `Shop inventory = {`, which makes the whole block a single assignment our parser already
/// understands.
pub fn inventory(lines: &[String]) -> Vec<(String, i64)> {
    let start = match lines.iter().rposition(|l| l.trim_start().starts_with("Shop inventory")) {
        Some(i) => i,
        None => return Vec::new(),
    };
    // The block ends at the first line that closes it at column zero. Everything the game prints
    // inside is indented, so this is unambiguous without counting braces.
    let mut text = String::from("return {\n");
    for l in lines.iter().skip(start + 1) {
        if l.starts_with('}') {
            break;
        }
        text.push_str(l);
        text.push('\n');
    }
    text.push_str("}\n");
    let Ok(t) = crate::game::save::parse(&text) else { return Vec::new() };
    // **The array part, not the named part.** `Table` splits the two the way Lua does, and shop
    // entries are a plain sequence — so they land in `arr` and a dotted path like `"1.type"` finds
    // nothing at all. Order is the whole point here: it is the order the game inserted them, which
    // `refreshButtons` walks to place the slots, so it is the order they are drawn.
    t.arr
        .iter()
        .filter_map(|v| match v {
            crate::game::save::Value::Table(item) => {
                let ty = item.str_at("type")?.to_string();
                Some((ty, item.int_at("stock").unwrap_or(0)))
            }
            _ => None,
        })
        .collect()
}

/// The 1-based index of `item_type` in a printed inventory, if it is in stock.
pub fn index_of(inv: &[(String, i64)], item_type: &str) -> Option<usize> {
    inv.iter().position(|(t, stock)| t == item_type && *stock > 0).map(|i| i + 1)
}

/// How many of `item_type` are on the shelf, as the shop just printed it.
///
/// **A settlement can stock more than one, and assuming otherwise left them behind.** The dev found
/// it on 2026-08-15. `specialStock` seeds the shelf (`shop.lua:372-379`) and nothing caps it at a
/// single item, so the count has to be read rather than assumed — and it is right there in the dump
/// the shop prints on opening.
pub fn stock_of(inv: &[(String, i64)], item_type: &str) -> i64 {
    inv.iter().find(|(t, _)| t == item_type).map(|(_, n)| *n).unwrap_or(0)
}

/// The `Heart`: `healthBuff`, four maximum health for a hundred gold (`items/ephemeral.lua:4-9`).
pub const HEART: &str = "healthBuff";

#[cfg(test)]
mod tests {
    use super::*;

    /// The dump the run of 2026-08-15 pulled off the console at Rowlston Covert, verbatim.
    fn rowlston() -> Vec<String> {
        "Opened shop UI
Shop inventory = {
    {
        type = \"healthBuff\",
        stock = 1,
    },
    {
        type = \"antivenom\",
        stock = 3,
    },
    {
        type = \"gRegenPotion\",
        stock = 2,
    },
}"
        .lines()
        .map(str::to_string)
        .collect()
    }

    #[test]
    fn the_console_dump_reads_as_a_shop_inventory() {
        let inv = inventory(&rowlston());
        assert_eq!(inv.len(), 3, "three items, in the order the game drew them");
        assert_eq!(inv[0], ("healthBuff".into(), 1));
        assert_eq!(index_of(&inv, HEART), Some(1), "the heart is the first slot");
        assert_eq!(index_of(&inv, "scrollOfRecall"), None, "and what is not stocked has no slot");
    }

    /// Out of stock is not "somewhere on the shelf", it is not for sale.
    #[test]
    fn a_sold_out_item_has_no_index() {
        let mut lines = rowlston();
        for l in lines.iter_mut() {
            if l.contains("stock = 1") {
                *l = "        stock = 0,".into();
            }
        }
        let inv = inventory(&lines);
        assert_eq!(index_of(&inv, HEART), None);
    }

    /// The grid from `shop.lua:287-292`, against the frame it was checked on.
    ///
    /// `spike-frames-live/shop-open.png` at 1920x1080 has `Heart` centred on the first slot and
    /// `Scroll of recall` on the eighth, and these are the numbers that put a click on them.
    #[test]
    fn the_eight_slots_are_where_the_frame_shows_them() {
        // Two rows of four, left to right then down.
        let want = [
            (470, 324),
            (797, 324),
            (1123, 324),
            (1450, 324),
            (470, 648),
            (797, 648),
            (1123, 648),
            (1450, 648),
        ];
        for (n, expect) in want.iter().enumerate() {
            let x = (1920.0 * COL[n % COL.len()]).round() as i32;
            let y = (1080.0 * ROW[n / COL.len()]).round() as i32;
            assert_eq!((x, y), *expect, "slot {}", n + 1);
        }
    }

    /// A ninth item has no slot on the first page, and refusing to place it is the safe answer.
    ///
    /// There is no confirmation dialogue on this screen (`ui/elements/shopitem.lua:147-165` buys on
    /// release), so a slot we cannot place is a purchase of whatever else is sitting in it. Paging
    /// to it is [`page_of`]'s business, and the click still only happens once the offset matches.
    #[test]
    fn an_item_past_the_first_page_has_no_slot_yet() {
        assert_eq!(PER_PAGE, 8);
        // `slot_at` needs a window; the arithmetic it guards is the `n >= PER_PAGE` test, which is
        // what this pins without one.
        let n = 9usize - 1 - 0;
        assert!(n >= PER_PAGE, "item nine is off the first page");
    }

    /// Which page an item is on, and the boundaries either side of it.
    ///
    /// The arrows move `relativeIndex` in whole steps of eight from zero (`shop.lua:205-232`), so
    /// every answer here is a multiple of eight — item 8 is the last of page one, item 9 the first
    /// of page two.
    #[test]
    fn the_page_offset_is_always_a_whole_number_of_pages() {
        assert_eq!(page_of(1), 0);
        assert_eq!(page_of(8), 0, "the eighth item is the last on the first page");
        assert_eq!(page_of(9), 8, "the ninth opens the second");
        assert_eq!(page_of(16), 8);
        assert_eq!(page_of(17), 16);
        for index in 1..40 {
            assert_eq!(page_of(index) % PER_PAGE, 0, "item {index} lands mid-page");
            assert!(page_of(index) < index, "item {index} is behind its own page");
            assert!(index - page_of(index) <= PER_PAGE, "item {index} is past its own page");
        }
    }

    /// Paged to, every item lands in a slot — which is the property the buyer depends on.
    #[test]
    fn every_item_has_a_slot_once_its_page_is_turned_to() {
        for index in 1..40usize {
            let n = index - 1 - page_of(index);
            assert!(n < PER_PAGE, "item {index} is not on its own page");
        }
    }
}
