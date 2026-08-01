//! Parts of the screen that are chrome, not map.
//!
//! The adjacency dump reports every adjacent node's position in screen space whether or not that
//! position is somewhere you could click. Nodes routinely sit off-screen entirely — one live dump put
//! two exits at `(2153, -3)` and `(1958, 1654)` — and, worse, they sit *underneath the HUD*, which is
//! drawn over the map and takes the click.
//!
//! That is not a routing inconvenience, it is a safety problem. A live run picked the exit road at
//! `(213, 18)`, which is inside the inventory button, and opened the character screen; it then spent
//! the rest of its step budget clicking `(187, 918)`, which on *that* screen is `Stats`. Nothing in
//! the design stopped a computed map coordinate from landing on arbitrary UI, and the next such
//! collision need not be as harmless as a stats panel.
//!
//! So a coordinate derived from a dump is a *claim* about where a node is, and must be checked
//! against the chrome before it is clicked.

use crate::win::window::{button_center, ButtonSpec};

/// The inventory button, top-left — the one that swallowed the exit road.
///
/// `overworld.lua:1416-1421`:
///
/// ```lua
/// require'ui.elements.button'(function() return overworld.getPlayerVeryShortName() ... end, 0, 0, {
///     xOffset = 0.75,
///     yOffset = 0.38,
///     userFunctionName = 'inventory',
/// })
/// ```
///
/// No `type`, so it is `default` at 250x100 (`ui/elements/button.lua:17`), giving centre (188, 38)
/// and a box of x 63..313, y -12..88 at 1920x1080.
pub const INVENTORY: ButtonSpec =
    ButtonSpec { ss_x: 0.0, ss_y: 0.0, os_x: 0.75, os_y: 0.38, w: 250.0, h: 100.0 };

/// The options/menu button, top-right (`ui/options.lua:333-337`), a `small` 100x100.
pub const MENU: ButtonSpec =
    ButtonSpec { ss_x: 1.0, ss_y: 0.0, os_x: -2.63, os_y: 0.38, w: 100.0, h: 100.0 };

/// Half-width of the margin added around each chrome box, in unscaled pixels.
///
/// A click merely *near* a button is not obviously safe: the boxes here are derived from the button
/// declarations, and a decorative border can extend past them. Cheap insurance, since a node this
/// close to the HUD is not usefully clickable anyway.
const MARGIN: f64 = 8.0;

/// Is this client-space point somewhere a map click may be sent?
///
/// False when the point is outside the window, or inside any known piece of chrome. This is a
/// whitelist of *known* HUD, not a proof of safety — the top and bottom `fancyboard` overlays are
/// decorative and not enumerated here, so a `true` means "not known to be chrome", which is why
/// callers should still verify that the click did what they expected.
pub fn is_map_point(x: i32, y: i32, client_w: i32, client_h: i32) -> bool {
    if x < 0 || y < 0 || x >= client_w || y >= client_h {
        return false;
    }
    ![INVENTORY, MENU].iter().any(|spec| inside(spec, x, y, client_w, client_h))
}

/// Which piece of chrome a point falls in, for a log line that says why a click was refused.
pub fn chrome_at(x: i32, y: i32, client_w: i32, client_h: i32) -> Option<&'static str> {
    if x < 0 || y < 0 || x >= client_w || y >= client_h {
        return Some("off-screen");
    }
    for (name, spec) in [("inventory button", INVENTORY), ("menu button", MENU)] {
        if inside(&spec, x, y, client_w, client_h) {
            return Some(name);
        }
    }
    None
}

fn inside(spec: &ButtonSpec, x: i32, y: i32, client_w: i32, client_h: i32) -> bool {
    let s = crate::layout::scale(client_w, client_h);
    let (cx, cy) = button_center(spec, client_w, client_h);
    let (hw, hh) = ((spec.w * s / 2.0) + MARGIN, (spec.h * s / 2.0) + MARGIN);
    ((x - cx) as f64).abs() <= hw && ((y - cy) as f64).abs() <= hh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exit_road_that_opened_the_character_screen_is_refused() {
        // The exact coordinate from the live dump for `l10_path_to_l19`, which was adjacent to the
        // guard post and therefore a perfectly legal *routing* choice.
        assert!(!is_map_point(213, 18, 1920, 1080));
        assert_eq!(chrome_at(213, 18, 1920, 1080), Some("inventory button"));
    }

    #[test]
    fn off_screen_nodes_are_refused() {
        // Both from the same dump's exit list. The map is bigger than the window, so this is normal
        // rather than exceptional.
        assert!(!is_map_point(2153, -3, 1920, 1080));
        assert!(!is_map_point(1958, 1654, 1920, 1080));
    }

    #[test]
    fn ordinary_map_positions_are_allowed() {
        // The four adjacent nodes from that same dump, all comfortably on the map.
        for (x, y) in [(1183, 679), (1209, 428), (854, 718), (1013, 702)] {
            assert!(is_map_point(x, y, 1920, 1080), "({x}, {y}) should be clickable");
            assert_eq!(chrome_at(x, y, 1920, 1080), None);
        }
    }

    #[test]
    fn the_area_button_strip_stays_clickable() {
        // (187, 918) is Travel/Combat/Visit. If chrome detection ever swallowed this, the run could
        // select nodes but never act on them.
        assert!(is_map_point(187, 918, 1920, 1080));
    }

    #[test]
    fn chrome_scales_with_the_window() {
        // Half-size window: the inventory button is at (94, 19), so the point that was inside it at
        // full size is now well outside.
        assert!(!is_map_point(94, 19, 960, 540));
        assert!(is_map_point(213, 18, 960, 540));
    }
}
