//! What to buy in a shop. **A stub: it buys nothing.**
//!
//! Shops keep turning up whether or not we have a use for them. A `[Shop]` choice is the *safe*
//! branch of several road events — the Woodsman's alternative is `[Combat] - "The book hungers for
//! blood."` — so `safe_choice_avoiding_combat` picks it correctly and lands the run in a shop UI it
//! has no idea what to do with. A live run stalled there.
//!
//! So this exists to make the shop a **pass-through**, not to shop. [`wanted`] returns nothing, the
//! caller leaves immediately, and the run carries on. That is a deliberate floor rather than a
//! placeholder to be embarrassed about: buying needs an item model, a value judgement and a budget,
//! none of which exist, and inventing them badly is worse than declining.
//!
//! ## What it will need to know, when it is written
//!
//! The console hands over the whole stock. `core.onActive` (`shop.lua:248-256`) prints
//!
//! ```text
//! Opened shop UI
//! Shop inventory = { { type = "antivenom", cost = 12, ... }, ... }
//! ```
//!
//! and `table.repr` is the save serializer (`utils/table.lua:435-437`), so
//! [`crate::game::save::parse`] reads that block directly — the same trick the event choices use.
//! Nothing here parses it yet, because nothing here would do anything with it.
//!
//! ## The first thing worth buying, when the time comes
//!
//! Antivenom. The Woodsman's own line is *"You should buy some antivenom or antiserum first. I got
//! what you need right here."* / *"Then you'll be ready to take on the spider forest"* — and the node
//! he is standing on is `Saltagh Park — level 1 spider forest`. That is the game telling us plainly
//! that the shop in front of us sells the counter to the hazard behind him. A buyer that only ever
//! bought antivenom before a spider forest would already be worth having.

/// Something to buy: the item key the shop listed, and what it costs.
///
/// Defined ahead of any code that produces one so the caller's shape is settled. [`wanted`] returns
/// an empty list, so nothing constructs this yet.
#[derive(Debug, Clone, PartialEq)]
pub struct Purchase {
    pub item: String,
    pub cost: i64,
}

/// What to buy from this shop, given what it stocks and what we can afford.
///
/// **Always empty.** The arguments are taken rather than ignored so the signature does not change
/// when the body does, and so a caller written against it today is written against the real thing.
///
/// `gold` is the player's purse from `mainSaveData.player.gold`; `stock` is whatever the caller
/// parsed out of the console's `Shop inventory` block, empty being perfectly normal.
pub fn wanted(_gold: i64, _stock: &[Purchase]) -> Vec<Purchase> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stub_buys_nothing_however_rich_we_are() {
        // Pinned rather than assumed. A run carrying 737 gold met the Woodsman's shop and the
        // correct behaviour was still to walk out -- the point of this module is that the shop stops
        // being a stall, not that it starts being useful.
        let stock = vec![
            Purchase { item: "antivenom".into(), cost: 12 },
            Purchase { item: "antiserum".into(), cost: 20 },
        ];
        assert!(wanted(737, &stock).is_empty());
        assert!(wanted(0, &[]).is_empty());
    }
}
