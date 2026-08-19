//! Keeping track of where a panel actually is.
//!
//! `DockArea::add_panel` and `remove_panel` walk the `DockItem` tree the shell
//! builds at start-up. That tree never learns about splits the user makes by
//! dragging a tab, so a panel dragged into a new pane becomes invisible to
//! both — closing it from the sidebar would quietly do nothing, and re-opening
//! it would put a second copy in the original pane.
//!
//! `Panel::on_added_to` hands a panel its owning `TabPanel` every time it
//! moves, which is the one handle that stays true. Panels the shell opens and
//! closes on the user's behalf keep a [`Home`] and record it there.

use std::sync::Arc;

use gpui::{App, WeakEntity, Window};
use gpui_component::dock::{PanelView, TabPanel};

/// The tab group a panel is currently sitting in.
#[derive(Default)]
pub struct Home(Option<WeakEntity<TabPanel>>);

impl Home {
    /// Records the tab group the panel has just been added to.
    pub fn moved_to(&mut self, tab_panel: WeakEntity<TabPanel>) {
        self.0 = Some(tab_panel);
    }

    /// The tab group, handed out by value so a caller can read it off a panel
    /// and then act on the app context without holding a borrow of either.
    pub fn tab_panel(&self) -> Option<WeakEntity<TabPanel>> {
        self.0.clone()
    }
}

/// Brings a panel's tab to the front, wherever it now lives.
///
/// Returns false if the panel has no home yet, so the caller can fall back to
/// the dock. `TabPanel` has no public "activate this tab", so an inactive tab
/// is taken out and put back — safe because a tab group whose active tab is not
/// this one necessarily holds more than one, and so cannot collapse itself out
/// of the layout on the way through.
pub fn reveal(
    home: Option<WeakEntity<TabPanel>>,
    panel: Arc<dyn PanelView>,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let Some(tabs) = home.and_then(|home| home.upgrade()) else {
        return false;
    };
    tabs.update(cx, |tabs, cx| {
        let showing = tabs
            .active_panel(cx)
            .is_some_and(|active| active.view().entity_id() == panel.view().entity_id());
        if !showing {
            tabs.remove_panel(panel.clone(), window, cx);
            tabs.add_panel(panel, window, cx);
        }
    });
    true
}

/// Takes a panel out of whichever tab group holds it.
pub fn close(
    home: Option<WeakEntity<TabPanel>>,
    panel: Arc<dyn PanelView>,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let Some(tabs) = home.and_then(|home| home.upgrade()) else {
        return false;
    };
    tabs.update(cx, |tabs, cx| tabs.remove_panel(panel, window, cx));
    true
}
