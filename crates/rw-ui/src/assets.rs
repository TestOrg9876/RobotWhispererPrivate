//! Icon assets, embedded in the binary on every target.
//!
//! gpui-component's own asset source embeds icons natively but *fetches* them
//! from a CDN on wasm, which needs an absolute endpoint and fails silently with
//! `builder error` if one is not supplied. Embedding on both targets removes the
//! network dependency and the configuration that goes with it.

use std::borrow::Cow;

use anyhow::anyhow;
use gpui::{AssetSource, Result, SharedString};

/// The icon set, taken from gpui-component so `IconName` resolves.
#[derive(rust_embed::RustEmbed)]
#[folder = "assets"]
#[include = "icons/**/*.svg"]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.is_empty() {
            return Ok(None);
        }
        Self::get(path)
            .map(|file| Some(file.data))
            .ok_or_else(|| anyhow!("no embedded asset at {path:?}"))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(Self::iter()
            .filter(|candidate| candidate.starts_with(path))
            .map(Into::into)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icon_set_is_embedded() {
        let icons: Vec<_> = Assets::iter().collect();
        assert!(
            icons.len() > 50,
            "expected the full icon set, found {}",
            icons.len()
        );
        assert!(icons.iter().all(|path| path.ends_with(".svg")));
    }

    #[test]
    fn icons_used_by_the_shell_all_resolve() {
        // A missing icon renders as nothing, with no error, so assert the ones
        // the UI actually asks for are present.
        for name in [
            "icons/plus.svg",
            "icons/inbox.svg",
            "icons/bot.svg",
            "icons/star.svg",
            "icons/circle-check.svg",
            "icons/circle-x.svg",
            "icons/loader-circle.svg",
            "icons/dash.svg",
            "icons/palette.svg",
            "icons/panel-left.svg",
            "icons/panel-bottom.svg",
            "icons/play.svg",
            "icons/close.svg",
        ] {
            assert!(
                Assets.load(name).unwrap().is_some(),
                "missing {name}, which the shell renders"
            );
        }
    }

    #[test]
    fn an_unknown_path_is_an_error_not_a_silent_none() {
        assert!(Assets.load("icons/definitely-not-real.svg").is_err());
    }

    #[test]
    fn empty_paths_are_ignored() {
        assert!(Assets.load("").unwrap().is_none());
    }
}
