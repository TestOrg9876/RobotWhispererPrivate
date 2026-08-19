//! Saving and restoring the arrangement of the centre panes.
//!
//! The dock can serialise itself — `DockArea::dump` produces a `PanelState`
//! tree and `load` rebuilds one — but a tree saved yesterday describes
//! yesterday's workspace. A request deleted in the meantime would come back as
//! a tab with nothing behind it, and an empty tab group deserialises into a
//! panel lookup for the name `TabPanel`, which is not a panel at all.
//!
//! So a restored layout is pruned first: entries whose request is gone are
//! dropped, and every group left empty by that goes with them.

use gpui_component::dock::{PanelInfo, PanelState};

/// `panel_name` of a request editor, and the key its id is stored under.
pub const REQUEST: &str = "Request";
const REQUEST_ID: &str = "request";
/// `panel_name` of the welcome panel.
pub const WELCOME: &str = "Welcome";

/// The saved form of a request editor: its id, and nothing else. Everything
/// else about a request is already in storage.
pub fn request_panel(id: i64) -> PanelInfo {
    PanelInfo::panel(serde_json::json!({ REQUEST_ID: id }))
}

/// The request id a saved panel entry stands for, if it is a request editor.
pub fn request_of(info: &PanelInfo) -> Option<i64> {
    match info {
        PanelInfo::Panel(value) => value.get(REQUEST_ID)?.as_i64(),
        _ => None,
    }
}

/// Drops entries for requests that no longer exist, and any group left empty.
///
/// `exists` answers for one request id. Returns `None` when nothing worth
/// showing survives, which is the caller's cue to keep the default layout.
pub fn prune(state: &PanelState, exists: &dyn Fn(i64) -> bool) -> Option<PanelState> {
    match &state.info {
        PanelInfo::Stack { sizes, axis } => {
            // Sizes are positional, so a dropped child has to take its size
            // with it or every pane after it inherits the wrong width.
            let mut kept = Vec::new();
            let mut kept_sizes = Vec::new();
            for (index, child) in state.children.iter().enumerate() {
                if let Some(child) = prune(child, exists) {
                    kept.push(child);
                    if let Some(size) = sizes.get(index) {
                        kept_sizes.push(*size);
                    }
                }
            }
            if kept.is_empty() {
                return None;
            }
            // Only carry sizes through when they still describe every pane; a
            // short list would be applied to the wrong ones.
            let sizes = if kept_sizes.len() == kept.len() {
                kept_sizes
            } else {
                Vec::new()
            };
            Some(PanelState {
                panel_name: state.panel_name.clone(),
                children: kept,
                info: PanelInfo::Stack { sizes, axis: *axis },
            })
        }
        PanelInfo::Tabs { active_index } => {
            let kept: Vec<_> = state
                .children
                .iter()
                .filter_map(|child| prune(child, exists))
                .collect();
            if kept.is_empty() {
                return None;
            }
            let active_index = (*active_index).min(kept.len() - 1);
            Some(PanelState {
                panel_name: state.panel_name.clone(),
                children: kept,
                info: PanelInfo::Tabs { active_index },
            })
        }
        PanelInfo::Panel(_) => match state.panel_name.as_str() {
            REQUEST => request_of(&state.info)
                .filter(|id| exists(*id))
                .map(|_| state.clone()),
            WELCOME => Some(state.clone()),
            // Anything else is either a panel this build no longer registers or
            // an empty tab group, which dumps with no info at all.
            _ => None,
        },
        // Never built by this app; a saved one could only come from a future
        // version, and rebuilding it blind would be worse than ignoring it.
        PanelInfo::Tiles { .. } => None,
    }
}

/// Guarantees a stack at the root of the arrangement.
///
/// A `TabPanel` with no parent `StackPanel` reports itself locked, and a locked
/// tab strip can neither be dragged nor dropped onto — so a layout restored as
/// a bare tab group would come back with its splitting disabled.
pub fn rooted(state: PanelState) -> PanelState {
    if matches!(state.info, PanelInfo::Stack { .. }) {
        return state;
    }
    PanelState {
        panel_name: "StackPanel".into(),
        children: vec![state],
        info: PanelInfo::Stack {
            sizes: Vec::new(),
            axis: 1,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::px;

    fn request(id: i64) -> PanelState {
        PanelState {
            panel_name: REQUEST.into(),
            children: Vec::new(),
            info: request_panel(id),
        }
    }

    fn tabs(children: Vec<PanelState>, active_index: usize) -> PanelState {
        PanelState {
            panel_name: "TabPanel".into(),
            children,
            info: PanelInfo::Tabs { active_index },
        }
    }

    fn stack(children: Vec<PanelState>, sizes: Vec<gpui::Pixels>) -> PanelState {
        PanelState {
            panel_name: "StackPanel".into(),
            children,
            info: PanelInfo::Stack { sizes, axis: 0 },
        }
    }

    fn all(_: i64) -> bool {
        true
    }

    #[test]
    fn a_stack_at_the_root_is_left_alone() {
        let saved = stack(vec![tabs(vec![request(1)], 0)], vec![px(800.)]);
        assert_eq!(rooted(saved.clone()), saved);
    }

    #[test]
    fn a_bare_tab_group_gains_a_stack_so_it_stays_splittable() {
        let saved = tabs(vec![request(1)], 0);
        let rooted = rooted(saved.clone());
        assert!(matches!(rooted.info, PanelInfo::Stack { .. }));
        assert_eq!(rooted.children, vec![saved]);
    }

    #[test]
    fn a_request_id_survives_the_round_trip() {
        assert_eq!(request_of(&request_panel(42).clone()), Some(42));
    }

    #[test]
    fn a_panel_that_is_not_a_request_has_no_id() {
        assert_eq!(request_of(&PanelInfo::tabs(0)), None);
        assert_eq!(request_of(&PanelInfo::panel(serde_json::json!({}))), None);
    }

    #[test]
    fn an_intact_layout_is_left_alone() {
        let saved = stack(vec![tabs(vec![request(1), request(2)], 1)], vec![px(800.)]);
        assert_eq!(prune(&saved, &all), Some(saved));
    }

    #[test]
    fn a_deleted_request_is_dropped() {
        let saved = stack(vec![tabs(vec![request(1), request(2)], 0)], vec![px(800.)]);
        let pruned = prune(&saved, &|id| id == 2).expect("one request survives");
        assert_eq!(pruned.children[0].children.len(), 1);
        assert_eq!(request_of(&pruned.children[0].children[0].info), Some(2));
    }

    #[test]
    fn a_group_emptied_by_pruning_goes_too() {
        let saved = stack(
            vec![tabs(vec![request(1)], 0), tabs(vec![request(2)], 0)],
            vec![px(400.), px(400.)],
        );
        let pruned = prune(&saved, &|id| id == 2).expect("one pane survives");
        assert_eq!(pruned.children.len(), 1);
        assert_eq!(pruned.info.sizes().map(Vec::len), Some(1));
    }

    #[test]
    fn a_layout_with_nothing_left_yields_nothing() {
        let saved = stack(vec![tabs(vec![request(1)], 0)], vec![px(800.)]);
        assert_eq!(prune(&saved, &|_| false), None);
    }

    #[test]
    fn the_active_tab_stays_in_range() {
        let saved = tabs(vec![request(1), request(2), request(3)], 2);
        let pruned = prune(&saved, &|id| id == 1).expect("one tab survives");
        assert_eq!(pruned.info.active_index(), Some(0));
    }

    #[test]
    fn a_dropped_pane_takes_its_size_with_it() {
        let saved = stack(
            vec![
                tabs(vec![request(1)], 0),
                tabs(vec![request(2)], 0),
                tabs(vec![request(3)], 0),
            ],
            vec![px(100.), px(200.), px(300.)],
        );
        let pruned = prune(&saved, &|id| id != 2).expect("two panes survive");
        assert_eq!(
            pruned.info.sizes(),
            Some(&vec![px(100.), px(300.)]),
            "the surviving panes keep their own widths"
        );
    }

    #[test]
    fn the_welcome_panel_is_kept() {
        let welcome = PanelState {
            panel_name: WELCOME.into(),
            children: Vec::new(),
            info: PanelInfo::panel(serde_json::Value::Null),
        };
        assert_eq!(prune(&welcome, &|_| false), Some(welcome));
    }

    #[test]
    fn an_unknown_panel_is_ignored_rather_than_rebuilt() {
        let unknown = PanelState {
            panel_name: "Hologram".into(),
            children: Vec::new(),
            info: PanelInfo::panel(serde_json::Value::Null),
        };
        assert_eq!(prune(&unknown, &all), None);
    }

    #[test]
    fn an_empty_tab_group_dumps_without_info_and_is_ignored() {
        // `TabPanel::dump` only sets its info inside the loop over its panels,
        // so a group with none of them looks like an unnamed panel.
        let empty = PanelState::default();
        assert_eq!(prune(&empty, &all), None);
    }
}
