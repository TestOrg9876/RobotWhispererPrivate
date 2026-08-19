//! Collections: the saved requests, which are this app's primary artifact.
//!
//! Postman's model. Requests are named, saved, searched and duplicated; several
//! may target the same service under different names with different payloads.
//! Connections do not appear here — they are environments, selected per request.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::menu::ContextMenuExt as _;
use gpui_component::{
    ActiveTheme as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use rw_core::domain::Request;

use crate::tokens;
use crate::workspace::Workspace;

/// Right-click actions on a request row. Each carries the row's id, so one
/// action serves every row rather than one action type per row.
#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct OpenRequest(pub i64);

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct DuplicateRequest(pub i64);

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct DeleteRequest(pub i64);

/// Move a request into a folder. `None` — spelled as `-1`, since an action's
/// payload has to be a plain value — means the root.
#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct MoveRequest {
    pub request: i64,
    pub folder: i64,
}

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct NewFolder(pub i64);

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct RenameFolder(pub i64);

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct DeleteFolder(pub i64);

/// What the sidebar asks the shell to do.
#[derive(Debug, Clone)]
pub enum CollectionsEvent {
    Open(i64),
    Duplicate(i64),
    Delete(i64),
    New,
}

/// `MoveRequest` carries the destination as a number because an action's payload
/// cannot be an `Option`. This is the one place that encoding is understood.
const ROOT: i64 = -1;

fn folder_of(encoded: i64) -> Option<i64> {
    (encoded != ROOT).then_some(encoded)
}

pub struct CollectionsPanel {
    focus_handle: FocusHandle,
    workspace: Entity<Workspace>,
    search: Entity<InputState>,
    /// Which request is highlighted, so the row reads as selected.
    selected: Option<i64>,
    /// Folders the user has collapsed. Collapsed rather than expanded is stored
    /// so a newly created folder starts open, which is what you want having just
    /// made it.
    collapsed: std::collections::HashSet<i64>,
    /// The folder being renamed, and the field holding the new name.
    renaming: Option<i64>,
    folder_name: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CollectionsEvent> for CollectionsPanel {}
impl EventEmitter<PanelEvent> for CollectionsPanel {}

impl CollectionsPanel {
    pub fn new(workspace: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search requests"));
        let folder_name = cx.new(|cx| InputState::new(window, cx).placeholder("Folder name"));

        let subscriptions = vec![
            cx.observe(&workspace, |_, _, cx| cx.notify()),
            cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            // Enter commits the name; clicking away commits it too, because
            // losing a folder's name by clicking elsewhere would be its own small
            // betrayal. Escape is not special here — there is nothing to go back
            // to for a folder that was just created.
            cx.subscribe(
                &folder_name,
                |this, _, event: &InputEvent, cx| match event {
                    InputEvent::PressEnter { .. } | InputEvent::Blur => this.commit_rename(cx),
                    _ => {}
                },
            ),
        ];

        Self {
            focus_handle: cx.focus_handle(),
            workspace,
            search,
            selected: None,
            collapsed: std::collections::HashSet::new(),
            renaming: None,
            folder_name,
            _subscriptions: subscriptions,
        }
    }

    pub fn view(workspace: Entity<Workspace>, window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(workspace, window, cx))
    }

    /// Marks a request as the selected row. The shell calls this when a request
    /// is opened from anywhere, so the sidebar stays in step.
    pub fn select(&mut self, request: Option<i64>, cx: &mut Context<Self>) {
        self.selected = request;
        cx.notify();
    }

    // ── folders ────────────────────────────────────────────────────────────────

    fn toggle_folder(&mut self, id: i64, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&id) {
            self.collapsed.insert(id);
        }
        cx.notify();
    }

    fn start_rename(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let name = self
            .workspace
            .read(cx)
            .collections()
            .iter()
            .find(|collection| collection.id == id)
            .map(|collection| collection.name.clone())
            .unwrap_or_default();

        self.renaming = Some(id);
        self.folder_name.update(cx, |state, cx| {
            state.set_value(name, window, cx);
            // Selected, not just placed at the end: `set_value` leaves the caret
            // after the text, so typing a new name would append to the old one.
            state.select_all(window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    /// Commits a rename, or creates the folder if this was a new one.
    fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.renaming.take() else {
            return;
        };
        let name = self.folder_name.read(cx).value().trim().to_string();
        if !name.is_empty() {
            self.workspace
                .update(cx, |workspace, cx| {
                    workspace.rename_collection(id, name, cx)
                })
                .detach();
        }
        cx.notify();
    }

    fn new_folder(&mut self, parent: Option<i64>, window: &mut Window, cx: &mut Context<Self>) {
        let creating = self.workspace.update(cx, |workspace, cx| {
            workspace.create_collection("New folder".to_string(), parent, cx)
        });

        cx.spawn_in(window, async move |panel, window| {
            let Some(collection) = creating.await else {
                return;
            };
            window
                .update(|window, cx| {
                    panel
                        .update(cx, |panel, cx| {
                            // Straight into a rename: a folder called "New
                            // folder" is not what anybody wanted, and making
                            // them find the rename command is a second step for
                            // no reason.
                            panel.start_rename(collection.id, window, cx)
                        })
                        .ok();
                })
                .ok();
        })
        .detach();
    }

    fn folder_row(
        &self,
        id: i64,
        name: &str,
        depth: usize,
        total: usize,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.renaming == Some(id) {
            return h_flex()
                .h(px(tokens::CONTROL_HEIGHT))
                .w_full()
                .pl(px(8. + depth as f32 * 14.))
                .pr_2()
                .items_center()
                .child(Input::new(&self.folder_name).xsmall())
                .into_any_element();
        }

        h_flex()
            .id(("folder", id as usize))
            .h(px(tokens::CONTROL_HEIGHT))
            .w_full()
            .pl(px(4. + depth as f32 * 14.))
            .pr_2()
            .gap_1()
            .items_center()
            .rounded(cx.theme().radius)
            .hover(|row| row.bg(cx.theme().list_hover))
            .child(
                gpui_component::Icon::new(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall()
                .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .truncate()
                    .child(name.to_string()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(total.to_string()),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_folder(id, cx)))
            .context_menu(move |menu, _window, _cx| {
                menu.menu("New folder inside", Box::new(NewFolder(id)))
                    .menu("Rename", Box::new(RenameFolder(id)))
                    .separator()
                    .menu("Delete folder", Box::new(DeleteFolder(id)))
            })
            .into_any_element()
    }

    /// The folders a request can be moved into, for its context menu.
    fn destinations(&self, cx: &App) -> Vec<(i64, String)> {
        self.workspace
            .read(cx)
            .collections()
            .iter()
            .map(|collection| (collection.id, collection.name.clone()))
            .collect()
    }

    fn row(&self, request: &Request, depth: usize, cx: &mut Context<Self>) -> AnyElement {
        let id = request.id;
        let selected = self.selected == Some(id);
        let target = request.target.clone();
        let kind = request.kind;
        let folders = self.destinations(cx);
        let current = request.collection_id;

        h_flex()
            .id(("request", id as usize))
            .h(px(tokens::CONTROL_HEIGHT))
            .w_full()
            .pl(px(8. + depth as f32 * 14.))
            .pr_2()
            .gap_2()
            .items_center()
            .rounded(cx.theme().radius)
            .when(selected, |row| row.bg(cx.theme().list_active))
            .when(!selected, |row| {
                row.hover(|row| row.bg(cx.theme().list_hover))
            })
            .child(tokens::status_dot(tokens::kind_color(kind, cx)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .truncate()
                            .child(request.name.clone()),
                    )
                    .when(!target.is_empty(), |stack| {
                        stack.child(
                            tokens::mono(cx)
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .truncate()
                                .child(target.clone()),
                        )
                    }),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.selected = Some(id);
                cx.emit(CollectionsEvent::Open(id));
                cx.notify();
            }))
            .context_menu(move |mut menu, _window, _cx| {
                menu = menu
                    .menu("Open", Box::new(OpenRequest(id)))
                    .menu("Duplicate", Box::new(DuplicateRequest(id)));

                if !folders.is_empty() || current.is_some() {
                    menu = menu.separator().label("Move to");
                    if current.is_some() {
                        menu = menu.menu(
                            "No folder",
                            Box::new(MoveRequest {
                                request: id,
                                folder: ROOT,
                            }),
                        );
                    }
                    for (folder, name) in &folders {
                        if current == Some(*folder) {
                            continue;
                        }
                        menu = menu.menu(
                            name.clone(),
                            Box::new(MoveRequest {
                                request: id,
                                folder: *folder,
                            }),
                        );
                    }
                }

                menu.separator().menu("Delete", Box::new(DeleteRequest(id)))
            })
            .into_any_element()
    }

    /// No button here on purpose.
    ///
    /// The + above this is two centimetres away and the welcome screen offers
    /// the same action; a third copy of it just makes the reader work out
    /// whether the three do different things.
    fn empty_state(&self, cx: &mut Context<Self>) -> AnyElement {
        tokens::empty_state(
            IconName::Inbox,
            "No requests yet",
            "Save a topic, service or action call to reuse it later.",
            cx,
        )
        .into_any_element()
    }
}

impl Focusable for CollectionsPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for CollectionsPanel {
    fn panel_name(&self) -> &'static str {
        "Collections"
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let total = self.workspace.read(cx).requests().len();
        h_flex().gap_1p5().items_baseline().child("Requests").child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(total.to_string()),
        )
    }

    fn closable(&self, _cx: &App) -> bool {
        false
    }
}

impl Render for CollectionsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.search.read(cx).value().trim().to_lowercase();
        let searching = !query.is_empty();

        let workspace = self.workspace.read(cx);
        let collections = workspace.collections().to_vec();
        let requests = workspace.requests().to_vec();

        // Searching opens every folder: hiding a match behind a collapsed folder
        // makes the search look as though it found nothing.
        let collapsed = self.collapsed.clone();
        let plan = crate::tree::rows(
            &collections,
            &requests,
            |request| {
                query.is_empty()
                    || request.name.to_lowercase().contains(&query)
                    || request.target.to_lowercase().contains(&query)
            },
            |folder| searching || !collapsed.contains(&folder),
            searching,
        );

        let by_id: std::collections::HashMap<i64, &Request> = requests
            .iter()
            .map(|request| (request.id, request))
            .collect();
        let rows: Vec<_> = plan
            .iter()
            .filter_map(|row| match row {
                crate::tree::Row::Folder {
                    id,
                    name,
                    depth,
                    total,
                    expanded,
                } => Some(self.folder_row(*id, name, *depth, *total, *expanded, cx)),
                crate::tree::Row::Request { id, depth } => {
                    by_id.get(id).map(|request| self.row(request, *depth, cx))
                }
            })
            .collect();

        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .on_action(cx.listener(|this, action: &OpenRequest, _, cx| {
                this.selected = Some(action.0);
                cx.emit(CollectionsEvent::Open(action.0));
                cx.notify();
            }))
            .on_action(cx.listener(|_, action: &DuplicateRequest, _, cx| {
                cx.emit(CollectionsEvent::Duplicate(action.0));
            }))
            .on_action(cx.listener(|_, action: &DeleteRequest, _, cx| {
                cx.emit(CollectionsEvent::Delete(action.0));
            }))
            .on_action(cx.listener(|this, action: &MoveRequest, _, cx| {
                let folder = folder_of(action.folder);
                let request = action.request;
                this.workspace
                    .update(cx, |workspace, cx| {
                        workspace.move_request(request, folder, cx)
                    })
                    .detach();
            }))
            .on_action(cx.listener(|this, action: &NewFolder, window, cx| {
                this.new_folder(Some(action.0), window, cx);
            }))
            .on_action(cx.listener(|this, action: &RenameFolder, window, cx| {
                this.start_rename(action.0, window, cx);
            }))
            .on_action(cx.listener(|this, action: &DeleteFolder, _, cx| {
                let id = action.0;
                this.collapsed.remove(&id);
                this.workspace
                    .update(cx, |workspace, cx| workspace.delete_collection(id, cx))
                    .detach();
            }))
            .child(
                h_flex()
                    .flex_shrink_0()
                    .p_2()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.search).small().cleanable(true)),
                    )
                    .child(
                        Button::new("new-folder")
                            .ghost()
                            .small()
                            .icon(IconName::Folder)
                            .tooltip("New folder")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.new_folder(None, window, cx);
                            })),
                    )
                    .child(
                        Button::new("new-request")
                            .ghost()
                            .small()
                            .icon(IconName::Plus)
                            .tooltip("New request")
                            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                                cx.emit(CollectionsEvent::New);
                            })),
                    ),
            )
            .child(tokens::hairline(cx))
            .child(if rows.is_empty() && !searching {
                self.empty_state(cx)
            } else {
                v_flex()
                    .id("request-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_2()
                    .gap_0p5()
                    .children(rows)
                    .when(requests.is_empty() && searching, |list| {
                        list.child(tokens::empty_state(
                            IconName::Search,
                            "No matches",
                            "No request's name or target contains that text.",
                            cx,
                        ))
                    })
                    .into_any_element()
            })
    }
}
