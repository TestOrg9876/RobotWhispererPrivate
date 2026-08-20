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
        /// Create a dashboard and open it.
        NewDashboard,
        /// Open the 3D world.
        ShowWorld,
        /// Start capturing every subscribed topic, or stop and keep what was
        /// captured.
        ToggleRecording,
        /// Write the last recording to a file.
        SaveRecording,
        /// Open a recording as a connection and play it back.
        OpenRecording,
        /// Play the recording just captured, without a trip through the disk.
        ReplayRecording,
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

/// Retarget or restyle one pane of a dashboard.
///
/// Each carries the pane it is about, because the menu these come from is drawn
/// by the dock on the tab strip — outside the pane — and an action dispatches
/// up from wherever the click happened. The dashboard is the first thing above
/// both the strip and the panes, so it is the one that routes them.
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct SetPaneConnection {
    pub pane: u64,
    pub connection: i64,
}

/// Point a dashboard pane at a topic.
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct SetPaneTopic {
    pub pane: u64,
    pub topic: SharedString,
}

/// Choose how a dashboard pane shows what it is watching.
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct SetPaneView {
    pub pane: u64,
    pub view: SharedString,
}

/// Put a topic in the 3D world.
///
/// Like the dashboard's pane actions, these carry the pane they are about: the
/// menus they come from are drawn by the dock on the tab strip, outside the
/// pane, and an action dispatches up from wherever the click happened.
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct AddWorldLayer {
    pub pane: u64,
    pub connection: i64,
    pub topic: SharedString,
}

/// Put a robot from the catalog in the 3D world.
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct AddWorldRobot {
    pub pane: u64,
    pub robot: SharedString,
}

/// Take a layer out of the 3D world.
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct RemoveWorldLayer {
    pub pane: u64,
    pub layer: u64,
}

/// Choose the frame everything in the world is drawn relative to.
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct SetWorldFrame {
    pub pane: u64,
    pub frame: SharedString,
}

/// Hang a robot layer off a frame of the transform tree.
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct SetWorldAnchor {
    pub pane: u64,
    pub layer: u64,
    pub frame: SharedString,
}

/// Point the camera back at whatever the world is showing.
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct ResetWorldView {
    pub pane: u64,
}

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
