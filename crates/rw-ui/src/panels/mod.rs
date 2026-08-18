//! The application's panels.
//!
//! Each is an `Entity<T: Panel>` living in the dock, which is what gives them
//! tabs that drag, reorder, split and restore without any of them knowing.

mod collections;
mod connections;
mod console;
mod palette_view;
mod request;
mod settings_view;
mod welcome;

pub use collections::{CollectionsEvent, CollectionsPanel};
pub use connections::ConnectionsPanel;
pub use console::ConsolePanel;
pub use palette_view::{PaletteEvent, PaletteView};
pub use request::RequestPanel;
pub use settings_view::{SettingsEvent, SettingsView};
pub use welcome::{WelcomeEvent, WelcomePanel};
