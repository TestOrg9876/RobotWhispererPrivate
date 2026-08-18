//! Theme registration and selection.
//!
//! The seven palettes live in `themes/robot-whisperer.json` as a gpui-component
//! `ThemeSet`, ported from the pre-rewrite `src/app.css`. They are embedded
//! rather than read from disk so the web build has no asset-origin dependency.

use anyhow::{Context as _, Result};
use gpui::App;
use gpui_component::{ActiveTheme as _, Theme, ThemeRegistry};

/// The bundled theme set, embedded at compile time.
const THEME_SET: &str = include_str!("../themes/robot-whisperer.json");

/// Applied when nothing has been persisted yet; matches `is_default` in the JSON.
pub const DEFAULT_THEME: &str = "Dark";

/// Registers the bundled themes. Call once during app init, before the first window.
pub fn register(cx: &mut App) -> Result<()> {
    ThemeRegistry::global_mut(cx)
        .load_themes_from_str(THEME_SET)
        .context("bundled theme set failed to load")
}

/// Names of every registered Robot Whisperer theme, in the JSON's order.
///
/// Read from the embedded source rather than the registry so the order is the
/// authored one — `ThemeRegistry::themes()` is a map and does not preserve it.
pub fn names() -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Named {
        name: String,
    }
    #[derive(serde::Deserialize)]
    struct Set {
        themes: Vec<Named>,
    }

    serde_json::from_str::<Set>(THEME_SET)
        .map(|set| set.themes.into_iter().map(|theme| theme.name).collect())
        .unwrap_or_default()
}

/// Switches to a registered theme by name. Unknown names are ignored, matching
/// the old `settingsStore` behaviour when `localStorage` held a stale value.
pub fn apply(name: &str, cx: &mut App) {
    let Some(config) = ThemeRegistry::global(cx).themes().get(name).cloned() else {
        tracing::warn!("no theme named {name:?}; keeping the current one");
        return;
    };

    let mode = config.mode;
    Theme::global_mut(cx).apply_config(&config);
    Theme::change(mode, None, cx);
}

/// The active theme's name, which is what gets persisted.
pub fn current(cx: &App) -> String {
    cx.theme().theme_name().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::ThemeSet;

    fn theme_set() -> ThemeSet {
        serde_json::from_str(THEME_SET).expect("bundled theme set must parse")
    }

    #[test]
    fn bundles_the_seven_ported_themes() {
        let set = theme_set();
        assert_eq!(set.themes.len(), 7);
        assert_eq!(names().len(), 7);
        assert!(names().contains(&DEFAULT_THEME.to_string()));
    }

    #[test]
    fn exactly_one_theme_is_marked_default() {
        let set = theme_set();
        let defaults: Vec<_> = set
            .themes
            .iter()
            .filter(|theme| theme.is_default)
            .map(|theme| theme.name.clone())
            .collect();
        assert_eq!(defaults.len(), 1, "expected one default, got {defaults:?}");
        assert_eq!(defaults[0], DEFAULT_THEME);
    }

    /// `ThemeConfigColors` has no `deny_unknown_fields` and no aliases, so a
    /// misspelled key deserialises to `None` and is silently ignored. This
    /// asserts the keys we actually author land in real fields.
    #[test]
    fn colour_keys_are_recognised_not_silently_dropped() {
        for theme in theme_set().themes {
            let colors = &theme.colors;
            for (label, value) in [
                ("background", &colors.background),
                ("foreground", &colors.foreground),
                ("border", &colors.border),
                ("primary.background", &colors.primary),
                ("sidebar.background", &colors.sidebar),
                ("title_bar.background", &colors.title_bar),
                ("status_bar.background", &colors.status_bar),
                ("tab.active.background", &colors.tab_active),
                ("danger.background", &colors.danger),
                ("success.background", &colors.success),
                ("warning.background", &colors.warning),
                ("selection.background", &colors.selection),
                ("drop_target.background", &colors.drop_target),
                ("scrollbar.thumb.background", &colors.scrollbar_thumb),
            ] {
                assert!(
                    value.is_some(),
                    "theme {:?} did not populate {label}: the JSON key does not \
                     match the serde rename, so it is being ignored",
                    theme.name
                );
            }
        }
    }

    #[test]
    fn every_colour_parses_as_a_colour() {
        // A malformed value would otherwise surface as a wrong colour at runtime.
        for theme in theme_set().themes {
            let json = serde_json::to_value(&theme.colors).expect("colors serialise");
            let object = json.as_object().expect("colors is an object");
            for (key, value) in object {
                let Some(text) = value.as_str() else { continue };
                assert!(
                    text.starts_with('#') && (text.len() == 7 || text.len() == 9),
                    "theme {:?} key {key} has {text:?}, expected #rrggbb or #rrggbbaa",
                    theme.name
                );
            }
        }
    }

    #[test]
    fn light_and_dark_modes_are_both_represented() {
        let set = theme_set();
        assert!(set.themes.iter().any(|theme| theme.mode.is_dark()));
        assert!(set.themes.iter().any(|theme| !theme.mode.is_dark()));
    }
}
