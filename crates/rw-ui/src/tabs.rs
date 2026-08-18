//! Open workspace tabs, replacing `tabsStore.svelte.ts`.

use rw_core::domain::Request;

/// What a tab shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Request(i64),
    Dashboard(String),
}

impl Target {
    /// Stable identity, used both as the tab key and as the dedup key when
    /// opening. Requests and dashboards share the namespace, so they are
    /// prefixed to keep a request `1` distinct from a dashboard `"1"`.
    fn key(&self) -> String {
        match self {
            Self::Request(id) => format!("request:{id}"),
            Self::Dashboard(id) => format!("dashboard:{id}"),
        }
    }
}

/// One open tab.
#[derive(Debug, Clone)]
pub struct Tab {
    pub key: String,
    pub target: Target,
    pub title: String,
    /// Set while the editor holds unsaved edits, shown as a dot in the tab bar.
    pub dirty: bool,
}

/// The open tabs and which one has focus.
#[derive(Debug, Default)]
pub struct Tabs {
    open: Vec<Tab>,
    active: Option<String>,
}

impl Tabs {
    pub fn all(&self) -> &[Tab] {
        &self.open
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    pub fn active(&self) -> Option<&Tab> {
        let key = self.active.as_deref()?;
        self.open.iter().find(|tab| tab.key == key)
    }

    pub fn is_active(&self, key: &str) -> bool {
        self.active.as_deref() == Some(key)
    }

    pub fn activate(&mut self, key: &str) {
        if self.open.iter().any(|tab| tab.key == key) {
            self.active = Some(key.to_string());
        }
    }

    /// Opens a tab, or focuses it if already open. Returns its key.
    pub fn open(&mut self, target: Target, title: impl Into<String>) -> String {
        let key = target.key();
        if !self.open.iter().any(|tab| tab.key == key) {
            self.open.push(Tab {
                key: key.clone(),
                target,
                title: title.into(),
                dirty: false,
            });
        }
        self.active = Some(key.clone());
        key
    }

    pub fn open_request(&mut self, request: &Request) -> String {
        self.open(Target::Request(request.id), request.name.clone())
    }

    /// Closes a tab. If it was active, focus moves to whichever tab now
    /// occupies its index, or the last tab if it was the final one.
    pub fn close(&mut self, key: &str) {
        let Some(index) = self.open.iter().position(|tab| tab.key == key) else {
            return;
        };
        self.open.remove(index);

        if self.active.as_deref() == Some(key) {
            self.active = self
                .open
                .get(index.min(self.open.len().saturating_sub(1)))
                .map(|tab| tab.key.clone());
        }
    }

    pub fn close_request(&mut self, id: i64) {
        self.close(&Target::Request(id).key());
    }

    pub fn set_dirty(&mut self, key: &str, dirty: bool) {
        if let Some(tab) = self.open.iter_mut().find(|tab| tab.key == key) {
            tab.dirty = dirty;
        }
    }

    pub fn rename(&mut self, key: &str, title: impl Into<String>) {
        if let Some(tab) = self.open.iter_mut().find(|tab| tab.key == key) {
            tab.title = title.into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(tabs: &Tabs) -> Vec<&str> {
        tabs.all().iter().map(|tab| tab.key.as_str()).collect()
    }

    #[test]
    fn opening_twice_focuses_instead_of_duplicating() {
        let mut tabs = Tabs::default();
        tabs.open(Target::Request(1), "One");
        tabs.open(Target::Request(2), "Two");
        tabs.open(Target::Request(1), "One");

        assert_eq!(keys(&tabs), ["request:1", "request:2"]);
        assert!(tabs.is_active("request:1"));
    }

    #[test]
    fn requests_and_dashboards_do_not_collide() {
        let mut tabs = Tabs::default();
        tabs.open(Target::Request(1), "Request");
        tabs.open(Target::Dashboard("1".into()), "Dashboard");
        assert_eq!(keys(&tabs), ["request:1", "dashboard:1"]);
    }

    #[test]
    fn closing_the_active_tab_focuses_the_one_that_takes_its_place() {
        let mut tabs = Tabs::default();
        tabs.open(Target::Request(1), "One");
        tabs.open(Target::Request(2), "Two");
        tabs.open(Target::Request(3), "Three");
        tabs.activate("request:2");

        tabs.close("request:2");

        assert_eq!(keys(&tabs), ["request:1", "request:3"]);
        assert!(tabs.is_active("request:3"));
    }

    #[test]
    fn closing_the_last_tab_falls_back_to_the_new_last() {
        let mut tabs = Tabs::default();
        tabs.open(Target::Request(1), "One");
        tabs.open(Target::Request(2), "Two");

        tabs.close("request:2");

        assert!(tabs.is_active("request:1"));
    }

    #[test]
    fn closing_the_only_tab_clears_focus() {
        let mut tabs = Tabs::default();
        tabs.open(Target::Request(1), "One");

        tabs.close("request:1");

        assert!(tabs.is_empty());
        assert!(tabs.active().is_none());
    }

    #[test]
    fn closing_an_inactive_tab_keeps_the_current_focus() {
        let mut tabs = Tabs::default();
        tabs.open(Target::Request(1), "One");
        tabs.open(Target::Request(2), "Two");

        tabs.close("request:1");

        assert!(tabs.is_active("request:2"));
    }

    #[test]
    fn closing_an_unknown_key_is_a_no_op() {
        let mut tabs = Tabs::default();
        tabs.open(Target::Request(1), "One");

        tabs.close("request:99");

        assert_eq!(keys(&tabs), ["request:1"]);
        assert!(tabs.is_active("request:1"));
    }

    #[test]
    fn activate_ignores_keys_that_are_not_open() {
        let mut tabs = Tabs::default();
        tabs.open(Target::Request(1), "One");

        tabs.activate("request:42");

        assert!(tabs.is_active("request:1"));
    }

    #[test]
    fn dirty_and_rename_target_the_right_tab() {
        let mut tabs = Tabs::default();
        tabs.open(Target::Request(1), "One");
        tabs.open(Target::Request(2), "Two");

        tabs.set_dirty("request:1", true);
        tabs.rename("request:2", "Renamed");

        assert!(tabs.all()[0].dirty);
        assert!(!tabs.all()[1].dirty);
        assert_eq!(tabs.all()[1].title, "Renamed");
    }
}
