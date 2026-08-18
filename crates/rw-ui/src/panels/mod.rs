//! Dock panels. Each is an `Entity<T: Panel>`, so the dock can tab, drag, zoom
//! and serialise it without knowing what it shows.

mod console;
mod explorer;
mod request;

pub use console::ConsolePanel;
pub use explorer::{ExplorerEvent, ExplorerPanel};
pub use request::RequestPanel;
