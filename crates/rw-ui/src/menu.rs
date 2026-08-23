//! One menu definition, drawn wherever the platform expects to find it.
//!
//! macOS owns the menu bar: it sits at the top of the *screen*, it always has
//! an application menu named after the app, and Settings lives in that menu
//! under `Cmd+,`. Linux and Windows expect the same commands inside the window.
//! Keeping two lists in step is how they drift, so the menu is described once
//! here as [`gpui::Menu`]s — [`install`] hands them to the operating system,
//! and [`popup`] renders the same commands into the title bar's button for the
//! platforms that have no menu bar to hand them to.

use gpui::{App, Menu, MenuItem, OsAction, OwnedMenu, OwnedMenuItem, SystemMenuType, Window};
use gpui_component::dock::ToggleZoom;
use gpui_component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use gpui_component::menu::PopupMenu;

use crate::actions::{
    CommandPalette, ExportWorkspace, ImportWorkspace, ManageConnections, NewDashboard, NewRequest,
    OpenRecording, OpenSettings, Quit, ReplayRecording, SaveRecording, SaveRequest, ShowWorld,
    ToggleConsole, ToggleRecording, ToggleSidebar,
};
use crate::session::RobotWhisperer;

/// Whether the operating system draws the menu bar itself.
///
/// Where it does, the title bar's own menu button is a second copy of the same
/// commands two centimetres below the first, so it is not drawn.
pub const NATIVE_MENU_BAR: bool = cfg!(target_os = "macos");

/// The one section that exists only for the operating system's sake.
const EDIT: &str = "Edit";

/// Registers the menu bar and the one action that only it needs.
pub fn install(cx: &mut App) {
    // Quit has no view to dispatch to — the window may not even be open — so it
    // is handled globally rather than on the workspace like the rest.
    cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
    refresh(cx);
}

/// Rebuilds the menu bar from current state.
///
/// Called whenever something a menu item's *wording* depends on changes. Only
/// recording does today, and "Start recording" on a menu that is already
/// recording is the kind of small lie that costs a whole capture.
pub fn refresh(cx: &mut App) {
    cx.set_menus(menus(cx));
}

/// The whole menu, in the order a menu bar wants it.
///
/// The first entry is the application menu: macOS renames it to the bundle name
/// and puts Settings, Services and Quit in it, which is exactly what is here.
pub fn menus(cx: &App) -> Vec<Menu> {
    described(
        RobotWhisperer::try_global(cx)
            .is_some_and(|whisperer| whisperer.recorder.read(cx).is_recording()),
    )
}

/// The description itself, with the one piece of state it depends on passed in
/// rather than read — which is what lets it be checked without an application.
fn described(recording: bool) -> Vec<Menu> {
    vec![
        Menu {
            name: "Robot Whisperer".into(),
            items: vec![
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Quit Robot Whisperer", Quit),
            ],
            disabled: false,
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("New request", NewRequest),
                MenuItem::action("New dashboard", NewDashboard),
                MenuItem::separator(),
                MenuItem::action("Save request", SaveRequest),
                MenuItem::separator(),
                MenuItem::action("Import workspace…", ImportWorkspace),
                MenuItem::action("Export workspace…", ExportWorkspace),
            ],
            disabled: false,
        },
        // The editing actions belong to whichever text field has focus, and are
        // listed so macOS can attach its standard responders to them: without an
        // Edit menu carrying `OsAction`s, the system shortcuts for cut and paste
        // reach nothing.
        Menu {
            name: EDIT.into(),
            items: vec![
                MenuItem::os_action("Undo", Undo, OsAction::Undo),
                MenuItem::os_action("Redo", Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", Cut, OsAction::Cut),
                MenuItem::os_action("Copy", Copy, OsAction::Copy),
                MenuItem::os_action("Paste", Paste, OsAction::Paste),
                MenuItem::os_action("Select all", SelectAll, OsAction::SelectAll),
            ],
            disabled: false,
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Command palette…", CommandPalette),
                MenuItem::separator(),
                MenuItem::action("Request list", ToggleSidebar),
                MenuItem::action("Console", ToggleConsole),
                MenuItem::action("3D world", ShowWorld),
                MenuItem::separator(),
                MenuItem::action("Full screen pane", ToggleZoom),
            ],
            disabled: false,
        },
        Menu {
            name: "Connections".into(),
            items: vec![MenuItem::action("Manage connections…", ManageConnections)],
            disabled: false,
        },
        Menu {
            name: "Recording".into(),
            items: vec![
                MenuItem::action(
                    if recording {
                        "Stop recording"
                    } else {
                        "Record every subscribed topic"
                    },
                    ToggleRecording,
                ),
                MenuItem::action("Replay last recording", ReplayRecording),
                MenuItem::separator(),
                MenuItem::action("Save recording…", SaveRecording),
                MenuItem::action("Open recording…", OpenRecording),
            ],
            disabled: false,
        },
    ]
}

/// The same menu, as the popup behind the title bar's button.
///
/// Flat, with each bar menu's name as a section label rather than a submenu:
/// one button's worth of chrome standing in for a whole menu bar is already a
/// level of nesting, and a second one puts every command two hovers away. The
/// application menu has no counterpart in a window, so its items land at the
/// bottom — where a button-borne menu is where people look for Settings and
/// Quit anyway.
pub fn popup(
    menu: PopupMenu,
    _window: &mut Window,
    cx: &mut gpui::Context<PopupMenu>,
) -> PopupMenu {
    let mut sections: Vec<OwnedMenu> = menus(cx).into_iter().map(Menu::owned).collect();
    let app = sections.remove(0);

    let menu = sections
        .into_iter()
        // Editing belongs to whichever field has focus, and is listed only so
        // macOS can attach its standard responders to it. In a menu reached by
        // clicking a button, the field has just lost focus.
        .filter(|section| section.name.as_ref() != EDIT)
        .fold(menu, |menu, section| {
            section.items.iter().fold(menu.label(section.name), item)
        });

    app.items.iter().fold(menu.separator(), item)
}

fn item(menu: PopupMenu, entry: &OwnedMenuItem) -> PopupMenu {
    match entry {
        // Sections are already separated by their labels; a rule as well is
        // twice the furniture for the same job.
        OwnedMenuItem::Separator => menu,
        OwnedMenuItem::Action { name, action, .. } => menu.menu(name.clone(), action.boxed_clone()),
        // The OS fills this one in; there is nothing to draw without it.
        OwnedMenuItem::SystemMenu(_) => menu,
        // The description is two levels deep, and a test says so.
        OwnedMenuItem::Submenu(_) => menu,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(menu: &Menu) -> Vec<&str> {
        menu.items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { name, .. } => Some(name.as_ref()),
                _ => None,
            })
            .collect()
    }

    /// macOS moves the first menu's items into the application menu and expects
    /// Settings among them. Put it anywhere else and `Cmd+,` still works while
    /// the place every Mac user looks is empty.
    #[test]
    fn settings_and_quit_live_in_the_application_menu() {
        let menus = described(false);
        let app = &menus[0];
        assert_eq!(app.name.as_ref(), "Robot Whisperer");
        assert!(names(app).contains(&"Settings\u{2026}"));
        assert!(names(app).contains(&"Quit Robot Whisperer"));
    }

    /// Settings is reachable from exactly one place in the menu, so the two
    /// renderings of it cannot disagree about where that is.
    #[test]
    fn settings_appears_once() {
        let menus = described(false);
        let settings = menus
            .iter()
            .flat_map(names)
            .filter(|name| *name == "Settings\u{2026}")
            .count();
        assert_eq!(settings, 1);
    }

    /// The recording item says what the next click will do. A menu held by the
    /// operating system is built once, so this is the assertion that the
    /// wording is a function of state and not a constant.
    #[test]
    fn the_recording_item_names_the_next_click() {
        let wording = |recording| {
            described(recording)
                .into_iter()
                .find(|menu| menu.name.as_ref() == "Recording")
                .map(|menu| names(&menu)[0].to_owned())
                .expect("a Recording menu")
        };
        assert_eq!(wording(true), "Stop recording");
        assert_ne!(wording(false), "Stop recording");
    }

    /// `popup` flattens one level and drops what is below it, so a third level
    /// added to the description would silently vanish from every platform that
    /// has no menu bar.
    #[test]
    fn the_description_is_two_levels_deep() {
        for menu in described(false) {
            for entry in &menu.items {
                assert!(
                    !matches!(entry, MenuItem::Submenu(_)),
                    "{} nests a submenu, which the popup rendering drops",
                    menu.name
                );
            }
        }
    }
}
