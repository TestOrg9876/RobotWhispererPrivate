//! The application's panels.
//!
//! Each is an `Entity<T: Panel>`, so the fixed shell and the dock host the very
//! same entities and switching between them costs nothing.

mod collections;
mod console;
mod request;

pub use collections::{CollectionsEvent, CollectionsPanel};
pub use console::ConsolePanel;
pub use request::RequestPanel;
