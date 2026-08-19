//! The application's panels.
//!
//! Each is an `Entity<T: Panel>` living in the dock, which is what gives them
//! tabs that drag, reorder, split and restore without any of them knowing.

mod collections;
mod connections;
mod console;
mod dashboard;
mod palette_view;
pub(crate) mod pane;
mod request;
mod robot;
mod settings_view;
mod welcome;

pub use collections::{CollectionsEvent, CollectionsPanel};
pub use connections::ConnectionsPanel;
pub use console::ConsolePanel;
pub use dashboard::DashboardPanel;
pub use palette_view::{PaletteEvent, PaletteView};
pub use pane::{PaneChanged, VizPanel};
pub use request::RequestPanel;
pub use robot::RobotPanel;
pub use settings_view::{SettingsEvent, SettingsView};
pub use welcome::{WelcomeEvent, WelcomePanel};
