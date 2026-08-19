//! The Robot Whisperer GPUI application.
//!
//! Both shells — `rw-desktop` natively and `rw-web` in the browser — build
//! storage and a schema registry for their platform and then call [`init`], so
//! everything above that boundary is shared.

pub mod actions;
pub mod assets;
pub mod discovery;
pub mod form;
pub mod image;
pub mod palette;
pub mod panels;
pub mod prefs;
pub mod series;
pub mod session;
pub mod theme;
pub mod tick;
pub mod tokens;
pub mod value;
pub mod workspace;
pub mod workspace_view;

use std::sync::Arc;

use anyhow::Result;
use gpui::{App, AppContext as _, Bounds, Focusable as _, WindowBounds, WindowOptions, px, size};
use gpui_component::{Root, TitleBar};
use rw_pipeline::CanonicalPipeline;

pub use session::{RobotWhisperer, Sessions};
pub use workspace::{SharedStorage, Workspace};

/// Registers themes, key bindings and global state.
pub fn init(storage: SharedStorage, pipeline: Arc<CanonicalPipeline>, cx: &mut App) -> Result<()> {
    gpui_component::init(cx);
    theme::register(cx)?;
    actions::bind_keys(cx);

    let workspace = cx.new(|_| Workspace::new(storage));
    let sessions = cx.new(|_| Sessions::new(pipeline));
    cx.set_global(RobotWhisperer {
        workspace,
        sessions,
    });

    Ok(())
}

/// Opens the main window.
pub fn open_window(cx: &mut App) -> Result<()> {
    let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitleBar::title_bar_options()),
        ..Default::default()
    };

    // The theme preference is applied here rather than in `init` because
    // `Preference::System` reads the window appearance, which needs a window.
    let prefs = prefs::Prefs::load();
    theme::apply(&prefs.theme(), cx);

    cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| workspace_view::WorkspaceView::new(prefs, window, cx));
        // Focused up front: an action dispatches from the focused element
        // upwards, so a shortcut pressed before anything has been clicked would
        // otherwise reach nothing.
        let handle = view.focus_handle(cx);
        window.focus(&handle, cx);
        cx.new(|cx| Root::new(view, window, cx))
    })?;

    Ok(())
}
