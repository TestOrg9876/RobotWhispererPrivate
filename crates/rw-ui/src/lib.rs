//! The Robot Whisperer GPUI application.
//!
//! Both shells — `rw-desktop` natively and `rw-web` in the browser — construct
//! storage for their platform and then call [`run`], so everything above the
//! storage boundary is shared.

pub mod shell;
pub mod tabs;
pub mod theme;
pub mod workspace;

use anyhow::Result;
use gpui::{App, AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component::{Root, TitleBar};

pub use workspace::{RobotWhisperer, SharedStorage, Workspace};

/// Initialises component state, themes, and the global app state.
///
/// Call once before opening a window.
pub fn init(storage: SharedStorage, theme_name: Option<&str>, cx: &mut App) -> Result<()> {
    gpui_component::init(cx);
    theme::register(cx)?;
    theme::apply(theme_name.unwrap_or(theme::DEFAULT_THEME), cx);
    RobotWhisperer::init(storage, cx);
    Ok(())
}

/// Opens the main window.
pub fn open_window(cx: &mut App) -> Result<()> {
    let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitleBar::title_bar_options()),
        ..Default::default()
    };

    cx.open_window(options, |window, cx| {
        let shell = cx.new(|cx| shell::Shell::new(window, cx));
        cx.new(|cx| Root::new(shell, window, cx))
    })?;

    Ok(())
}
