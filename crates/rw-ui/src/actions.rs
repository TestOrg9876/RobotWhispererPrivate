//! Application actions and their default key bindings.
//!
//! Actions are how a professional GPUI app exposes behaviour: they surface in
//! menus, in the command palette, and in keymaps without the call sites knowing
//! about any of those.

use gpui::{Action, App, KeyBinding, SharedString, actions};
use gpui_component::dock::{ClosePanel, ToggleZoom};
use serde::Deserialize;

actions!(
    robot_whisperer,
    [
        /// Create a request and open it in the centre dock.
        NewRequest,
        /// Open the connection editor.
        NewConnection,
        /// Connect or disconnect the selected connection.
        ToggleConnection,
        /// Show or hide the explorer dock.
        ToggleExplorer,
        /// Show or hide the console dock.
        ToggleConsole,
        /// Reset the dock layout to its default arrangement.
        ResetLayout,
        /// Quit the application.
        Quit,
    ]
);

/// Switch to a named theme. Carries the theme name so one action serves the
/// whole menu instead of one action per theme.
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct SetTheme(pub SharedString);

/// The default keymap. `ClosePanel` and `ToggleZoom` come from the dock module
/// so panels behave like the rest of the gpui-component ecosystem.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-n", NewRequest, None),
        KeyBinding::new("ctrl-n", NewRequest, None),
        KeyBinding::new("cmd-shift-n", NewConnection, None),
        KeyBinding::new("ctrl-shift-n", NewConnection, None),
        KeyBinding::new("cmd-b", ToggleExplorer, None),
        KeyBinding::new("ctrl-b", ToggleExplorer, None),
        KeyBinding::new("cmd-j", ToggleConsole, None),
        KeyBinding::new("ctrl-j", ToggleConsole, None),
        KeyBinding::new("cmd-w", ClosePanel, None),
        KeyBinding::new("ctrl-w", ClosePanel, None),
        KeyBinding::new("shift-escape", ToggleZoom, None),
        KeyBinding::new("cmd-q", Quit, None),
    ]);
}
