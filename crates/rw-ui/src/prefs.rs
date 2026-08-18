//! Preferences that live outside the workspace — currently just the theme.
//!
//! `cx.theme().theme_name()` cannot serve as the stored value because "follow
//! the OS" is a distinct choice from "always Robot Whisperer Dark", even when
//! both currently resolve to the same theme. Persistence is platform-split: a
//! JSON file under the config directory natively, `localStorage` on the web.
//! Reads never fail loudly; a missing or corrupt store yields defaults.

use serde::{Deserialize, Serialize};

use crate::theme::Preference;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Prefs {
    /// Persisted form of [`Preference`]: `"system"` or a theme name.
    #[serde(default)]
    theme: Option<String>,
}

impl Prefs {
    pub fn load() -> Self {
        read()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn theme(&self) -> Preference {
        self.theme
            .as_deref()
            .map(Preference::parse)
            .unwrap_or_default()
    }

    pub fn set_theme(&mut self, preference: &Preference) {
        self.theme = Some(preference.as_str().to_string());
        self.save();
    }

    /// Losing a preference must never interrupt the UI, so failures are logged.
    fn save(&self) {
        match serde_json::to_string(self) {
            Ok(raw) => write(&raw),
            Err(error) => tracing::warn!("could not serialise preferences: {error}"),
        }
    }
}

#[cfg(not(target_family = "wasm"))]
fn path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".config"))
        })?;
    Some(base.join("robot-whisperer").join("prefs.json"))
}

#[cfg(not(target_family = "wasm"))]
fn read() -> Option<String> {
    std::fs::read_to_string(path()?).ok()
}

#[cfg(not(target_family = "wasm"))]
fn write(raw: &str) {
    let Some(path) = path() else {
        tracing::warn!("no config directory; preferences not persisted");
        return;
    };
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        tracing::warn!("could not create {}: {error}", parent.display());
        return;
    }
    if let Err(error) = std::fs::write(&path, raw) {
        tracing::warn!("could not write {}: {error}", path.display());
    }
}

#[cfg(target_family = "wasm")]
const KEY: &str = "rw:prefs:v1";

#[cfg(target_family = "wasm")]
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

#[cfg(target_family = "wasm")]
fn read() -> Option<String> {
    storage()?.get_item(KEY).ok().flatten()
}

#[cfg(target_family = "wasm")]
fn write(raw: &str) {
    if let Some(storage) = storage()
        && storage.set_item(KEY, raw).is_err()
    {
        tracing::warn!("localStorage rejected the write; preferences not persisted");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_following_the_os() {
        assert_eq!(Prefs::default().theme(), Preference::System);
    }

    #[test]
    fn an_empty_document_is_valid_and_yields_defaults() {
        let parsed: Prefs = serde_json::from_str("{}").expect("empty object parses");
        assert_eq!(parsed.theme(), Preference::System);
    }

    #[test]
    fn an_explicit_theme_survives_a_round_trip() {
        let prefs = Prefs {
            theme: Some("Nord".into()),
        };
        let raw = serde_json::to_string(&prefs).expect("serialises");
        let back: Prefs = serde_json::from_str(&raw).expect("deserialises");
        assert_eq!(back.theme(), Preference::Named("Nord".into()));
    }

    #[test]
    fn the_system_sentinel_survives_a_round_trip() {
        let prefs = Prefs {
            theme: Some("system".into()),
        };
        let raw = serde_json::to_string(&prefs).expect("serialises");
        let back: Prefs = serde_json::from_str(&raw).expect("deserialises");
        assert_eq!(back.theme(), Preference::System);
    }
}
