//! Preferences that live outside the workspace: the theme, and how the window
//! was last arranged.
//!
//! `cx.theme().theme_name()` cannot serve as the stored value because "follow
//! the OS" is a distinct choice from "always Robot Whisperer Dark", even when
//! both currently resolve to the same theme. Persistence is platform-split: a
//! JSON file under the config directory natively, `localStorage` on the web.
//! Reads never fail loudly; a missing or corrupt store yields defaults.
//!
//! The pane arrangement lives here rather than in `rw_core::storage` because it
//! describes this machine's window, not the workspace: importing someone else's
//! export should bring their requests, not rearrange your screen.

use serde::{Deserialize, Serialize};

use crate::theme::Preference;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Prefs {
    /// Persisted form of [`Preference`]: `"system"` or a theme name.
    #[serde(default)]
    theme: Option<String>,
    /// Everything else that is chosen once and then left alone.
    #[serde(default)]
    settings: Settings,
    /// The last dock arrangement of the centre panes, and the layout version it
    /// was written by. Docks around the edge are fixed chrome and are not saved.
    #[serde(default)]
    layout: Option<Layout>,
}

/// The limits and defaults that used to be constants nobody could reach.
///
/// Every field defaults to the value the constant held, so a fresh install and
/// an install that has never opened Settings behave exactly as before. That is
/// not a nicety: the screenshot suite is the regression test for this whole
/// struct, and it only works if the defaults are the old numbers.
///
/// `#[serde(default)]` on each field, not just on the struct, so a preferences
/// file written before a field existed still loads and simply takes the default
/// for the new one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// How many points of a cloud are drawn. Above this the cloud is subsampled
    /// rather than dropped, so the shape survives and the frame rate does too.
    #[serde(default = "default_point_budget")]
    pub point_budget: usize,
    /// How many samples each plotted field keeps.
    #[serde(default = "default_plot_window")]
    pub plot_window: usize,
    /// How many fields a plot will draw at once before it stops adding series.
    #[serde(default = "default_plot_fields")]
    pub plot_fields: usize,
    /// The window a topic's rate and bandwidth are averaged over, in seconds.
    ///
    /// Short enough that a topic which has just stopped says so rather than
    /// reporting the average of the minute before it did.
    #[serde(default = "default_rate_window_secs")]
    pub rate_window_secs: u64,
    /// How much transform history is kept, in seconds. tf2's own default is 10.
    #[serde(default = "default_tf_window_secs")]
    pub tf_window_secs: u64,
    /// Whether a connection subscribes to `/tf` and `/tf_static` by itself.
    ///
    /// On by default, which is what RViz has done since 2010 — TF is not a
    /// topic anyone wants to remember to turn on. Here because it is a
    /// behaviour change, and a behaviour change nobody can turn off is a bug.
    #[serde(default = "default_follow_transforms")]
    pub follow_transforms: bool,
    /// How many console lines are kept.
    #[serde(default = "default_console_lines")]
    pub console_lines: usize,
    /// How many runs of one request are kept.
    #[serde(default = "default_history_depth")]
    pub history_depth: usize,
}

fn default_point_budget() -> usize {
    400_000
}
fn default_plot_window() -> usize {
    600
}
fn default_plot_fields() -> usize {
    12
}
fn default_rate_window_secs() -> u64 {
    5
}
fn default_tf_window_secs() -> u64 {
    10
}
fn default_follow_transforms() -> bool {
    true
}
fn default_console_lines() -> usize {
    2000
}
fn default_history_depth() -> usize {
    50
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            point_budget: default_point_budget(),
            plot_window: default_plot_window(),
            plot_fields: default_plot_fields(),
            rate_window_secs: default_rate_window_secs(),
            tf_window_secs: default_tf_window_secs(),
            follow_transforms: default_follow_transforms(),
            console_lines: default_console_lines(),
            history_depth: default_history_depth(),
        }
    }
}

/// A saved pane arrangement, kept as raw JSON so a change to the dock's own
/// state format cannot stop the rest of the preferences from loading.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Layout {
    version: usize,
    center: serde_json::Value,
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

    /// The saved arrangement, if it was written by this version of the layout.
    ///
    /// A bumped version means the default arrangement changed, and restoring
    /// the old one would put the user back in a layout the app no longer builds.
    pub fn layout(&self, version: usize) -> Option<&serde_json::Value> {
        let layout = self.layout.as_ref()?;
        (layout.version == version).then_some(&layout.center)
    }

    pub fn set_layout(&mut self, version: usize, center: serde_json::Value) {
        self.layout = Some(Layout { version, center });
        self.save();
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn set_settings(&mut self, settings: Settings) {
        self.settings = settings;
        self.save();
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
    fn a_layout_from_another_version_is_not_restored() {
        let prefs = Prefs {
            layout: Some(Layout {
                version: 1,
                center: serde_json::json!({"panel_name": "Request"}),
            }),
            ..Prefs::default()
        };
        assert!(prefs.layout(1).is_some());
        assert_eq!(
            prefs.layout(2),
            None,
            "a bumped version discards the layout"
        );
    }

    #[test]
    fn a_layout_survives_a_round_trip_alongside_the_theme() {
        let prefs = Prefs {
            theme: Some("Nord".into()),
            layout: Some(Layout {
                version: 3,
                center: serde_json::json!({"panel_name": "StackPanel"}),
            }),
            ..Prefs::default()
        };
        let raw = serde_json::to_string(&prefs).expect("serialises");
        let back: Prefs = serde_json::from_str(&raw).expect("deserialises");
        assert_eq!(back.theme(), Preference::Named("Nord".into()));
        assert_eq!(
            back.layout(3),
            Some(&serde_json::json!({"panel_name": "StackPanel"}))
        );
    }

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
            ..Prefs::default()
        };
        let raw = serde_json::to_string(&prefs).expect("serialises");
        let back: Prefs = serde_json::from_str(&raw).expect("deserialises");
        assert_eq!(back.theme(), Preference::Named("Nord".into()));
    }

    #[test]
    fn the_system_sentinel_survives_a_round_trip() {
        let prefs = Prefs {
            theme: Some("system".into()),
            ..Prefs::default()
        };
        let raw = serde_json::to_string(&prefs).expect("serialises");
        let back: Prefs = serde_json::from_str(&raw).expect("deserialises");
        assert_eq!(back.theme(), Preference::System);
    }

    /// The compatibility guarantee this whole struct rests on: a preferences
    /// file written before settings existed still loads, and every value is the
    /// constant it replaced. If this breaks, everyone's app quietly changes
    /// behaviour on upgrade.
    #[test]
    fn a_preferences_file_from_before_settings_still_loads() {
        let old = r#"{"theme":"Robot Whisperer Dark"}"#;
        let prefs: Prefs = serde_json::from_str(old).expect("an old file still loads");

        assert_eq!(prefs.theme().as_str(), "Robot Whisperer Dark");
        assert_eq!(prefs.settings(), &Settings::default());
    }

    /// And a file written before *one field* existed takes the default for that
    /// field rather than refusing the whole file.
    #[test]
    fn a_settings_block_missing_a_field_keeps_the_rest() {
        let partial = r#"{"settings":{"point_budget":1000}}"#;
        let prefs: Prefs = serde_json::from_str(partial).expect("loads");

        assert_eq!(prefs.settings().point_budget, 1000);
        assert_eq!(
            prefs.settings().history_depth,
            Settings::default().history_depth
        );
        assert!(prefs.settings().follow_transforms);
    }

    /// Every default is the number the constant held. Written out rather than
    /// compared against the constants themselves, so moving a constant behind
    /// `Settings` cannot silently move the default with it.
    #[test]
    fn the_defaults_are_the_constants_they_replaced() {
        let settings = Settings::default();
        assert_eq!(settings.point_budget, 400_000);
        assert_eq!(settings.plot_window, 600);
        assert_eq!(settings.plot_fields, 12);
        assert_eq!(settings.rate_window_secs, 5);
        assert_eq!(settings.tf_window_secs, 10);
        assert!(settings.follow_transforms);
        assert_eq!(settings.console_lines, 2000);
        assert_eq!(settings.history_depth, 50);
    }

    #[test]
    fn settings_round_trip_through_json() {
        let settings = Settings {
            point_budget: 12_345,
            follow_transforms: false,
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).expect("serialises");
        assert_eq!(
            serde_json::from_str::<Settings>(&json).expect("deserialises"),
            settings
        );
    }
}
