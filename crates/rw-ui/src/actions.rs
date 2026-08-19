//! Application actions and their default key bindings.
//!
//! Actions are how behaviour reaches menus, the keymap and buttons without any of
//! those knowing about each other.

use gpui::{Action, App, KeyBinding, SharedString, actions};
use gpui_component::dock::{ClosePanel, ToggleZoom};
use serde::Deserialize;

actions!(
    robot_whisperer,
    [
        /// Search every command, request and connection from one field.
        CommandPalette,
        /// Create a request and open it.
        NewRequest,
        /// Add, edit, connect and disconnect ROS systems.
        ManageConnections,
        /// Application settings: theme, and what else accumulates.
        OpenSettings,
        /// Show or hide the request list.
        ToggleSidebar,
        /// Show or hide the console.
        ToggleConsole,
        /// Open the robot viewer.
        ShowRobot,
        /// Save the active request.
        SaveRequest,
        /// Write the workspace to a file that can be shared or committed.
        ExportWorkspace,
        /// Read a workspace file into this one.
        ImportWorkspace,
    ]
);

/// Switch to a named theme, or `"system"` to follow the OS. One action serves the
/// whole menu rather than one action per theme.
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct SetTheme(pub SharedString);

/// Open the transport for a stored connection.
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct Connect(pub i64);

/// Close the transport for a stored connection.
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct Disconnect(pub i64);

/// The default keymap. `ClosePanel` and `ToggleZoom` come from the dock module so
/// panels behave like the rest of the gpui-component ecosystem.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-k", CommandPalette, None),
        KeyBinding::new("ctrl-k", CommandPalette, None),
        KeyBinding::new("cmd-p", CommandPalette, None),
        KeyBinding::new("ctrl-p", CommandPalette, None),
        KeyBinding::new("cmd-n", NewRequest, None),
        KeyBinding::new("ctrl-n", NewRequest, None),
        KeyBinding::new("cmd-shift-c", ManageConnections, None),
        KeyBinding::new("ctrl-shift-c", ManageConnections, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("ctrl-,", OpenSettings, None),
        KeyBinding::new("cmd-s", SaveRequest, None),
        KeyBinding::new("ctrl-s", SaveRequest, None),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
        KeyBinding::new("ctrl-b", ToggleSidebar, None),
        KeyBinding::new("cmd-j", ToggleConsole, None),
        KeyBinding::new("ctrl-j", ToggleConsole, None),
        KeyBinding::new("cmd-w", ClosePanel, None),
        KeyBinding::new("ctrl-w", ClosePanel, None),
        KeyBinding::new("shift-escape", ToggleZoom, None),
    ]);
}
