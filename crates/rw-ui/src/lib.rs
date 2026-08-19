//! The Robot Whisperer GPUI application.
//!
//! Both shells — `rw-desktop` natively and `rw-web` in the browser — build
//! storage and a schema registry for their platform and then call [`init`], so
//! everything above that boundary is shared.

pub mod actions;
pub mod assets;
pub mod cloud;
pub mod discovery;
pub mod docking;
pub mod form;
pub mod gpu;
pub mod image;
pub mod layout;
pub mod nesting;
pub mod palette;
pub mod panels;
pub mod prefs;
pub mod recorder;
pub mod runs;
pub mod scene_view;
pub mod series;
pub mod session;
pub mod theme;
pub mod tick;
pub mod tokens;
pub mod value;
pub mod views;
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

    register_panels(cx);

    let workspace = cx.new(|_| Workspace::new(storage));
    let sessions = cx.new(|_| Sessions::new(pipeline));
    let runs = cx.new(|_| runs::Runs::default());
    let gpu = gpu::Gpu::spawn(cx);
    let recorder = cx.new(|_| recorder::Recorder::default());
    cx.set_global(docking::Restored::default());
    cx.set_global(RobotWhisperer {
        workspace,
        sessions,
        runs,
        gpu,
        recorder,
    });

    Ok(())
}

/// Teaches the dock how to rebuild the panels it can find in a saved layout.
///
/// Only panels that appear in a saved arrangement are registered: the window's
/// centre tabs, and the panes inside a dashboard. The sidebar and console are
/// fixed chrome the shell builds itself, and rebuilding them here would hand
/// the dock a second copy of each, unsubscribed and disconnected from
/// everything.
fn register_panels(cx: &mut App) {
    use gpui_component::dock::{PanelView, register_panel};

    register_panel(cx, layout::REQUEST, |_, _, info, window, cx| {
        let panel = layout::request_of(info)
            .and_then(|id| {
                let request = RobotWhisperer::global(cx)
                    .workspace
                    .read(cx)
                    .request(id)
                    .cloned()?;
                Some((id, panels::RequestPanel::view(&request, window, cx)))
            })
            .map(|(id, panel)| {
                cx.global_mut::<docking::Restored>()
                    .requests
                    .push((id, panel.clone()));
                Box::new(panel) as Box<dyn PanelView>
            });

        // Pruning drops entries whose request is gone before the dock ever sees
        // them, so this only fires if storage changed underneath us. The
        // welcome panel is a truthful stand-in: there is nothing to show.
        panel.unwrap_or_else(|| Box::new(welcome_panel(cx)) as Box<dyn PanelView>)
    });

    // A dashboard's panes: rebuilt from the config each stored, and left in
    // the `Restored` global for the dashboard that asked for the load.
    register_panel(cx, layout::PANE, |_, _, info, _, cx| {
        let pane = panels::pane::VizPanel::view(panels::pane::config_of(info), cx);
        cx.global_mut::<docking::Restored>()
            .panes
            .push(pane.clone());
        Box::new(pane) as Box<dyn PanelView>
    });

    register_panel(cx, layout::WELCOME, |_, _, _, _, cx| {
        Box::new(welcome_panel(cx)) as Box<dyn PanelView>
    });
}

/// Builds the welcome panel and records it for the shell to claim.
fn welcome_panel(cx: &mut App) -> gpui::Entity<panels::WelcomePanel> {
    let panel = panels::WelcomePanel::view(cx);
    cx.global_mut::<docking::Restored>().welcome = Some(panel.clone());
    panel
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
