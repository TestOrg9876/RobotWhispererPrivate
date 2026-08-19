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
use crate::tree;
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

/// Create a collection inside this one.
#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct AddCollection(pub i64);

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct RenameCollection(pub i64);

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct DeleteCollection(pub i64);

/// The height of a request row: two lines of text with room to breathe.
///
/// Collections are shorter, because a collection is one line and matching the
/// two-line rows would leave it looking hollow.
const ROW_HEIGHT: f32 = 42.;
const COLLECTION_HEIGHT: f32 = 30.;

/// What is being dragged.
///
/// One type for both kinds of row: a drop target accepts either, and only the
/// collection case needs the checks that keep the tree a tree.
#[derive(Clone)]
pub enum Dragged {
    Request { id: i64, name: String },
    Collection { id: i64, name: String },
}

impl Dragged {
    fn name(&self) -> &str {
        match self {
            Dragged::Request { name, .. } | Dragged::Collection { name, .. } => name,
        }
    }
}

/// What follows the pointer while dragging.
///
/// A chip with the name on it. The row itself would be as wide as the sidebar
/// and would cover the thing being aimed at.
struct DragPreview {
    label: String,
}

impl Render for DragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded(cx.theme().radius)
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .text_xs()
            .text_color(cx.theme().foreground)
            .child(self.label.clone())
    }
}

/// What the sidebar asks the shell to do.
#[derive(Debug, Clone)]
pub enum CollectionsEvent {
    Open(i64),
    Duplicate(i64),
    Delete(i64),
    New,
    /// Something the user tried that could not be done, for the console.
    Complain(String),
}

pub struct CollectionsPanel {
    focus_handle: FocusHandle,
    workspace: Entity<Workspace>,
    search: Entity<InputState>,
    /// Which request is highlighted, so the row reads as selected.
    selected: Option<i64>,
    /// Collections the user has collapsed. Collapsed rather than expanded is
    /// stored so a newly created collection starts open, which is what you want
    /// having just made it.
    collapsed: std::collections::HashSet<i64>,
    /// The collection being renamed, and the field holding the new name.
    renaming: Option<i64>,
    collection_name: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CollectionsEvent> for CollectionsPanel {}
impl EventEmitter<PanelEvent> for CollectionsPanel {}

impl CollectionsPanel {
    pub fn new(workspace: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search requests"));
        let collection_name =
            cx.new(|cx| InputState::new(window, cx).placeholder("Collection name"));

        let subscriptions = vec![
            cx.observe(&workspace, |_, _, cx| cx.notify()),
            cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            // Enter commits the name; clicking away commits it too, because
            // losing a name by clicking elsewhere would be its own small
            // betrayal. Escape is not special here — there is nothing to go back
            // to for a collection that was just created.
            cx.subscribe(
                &collection_name,
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
            collection_name,
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

    // ── collections ────────────────────────────────────────────────────────────

    fn toggle_collection(&mut self, id: i64, cx: &mut Context<Self>) {
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
        self.collection_name.update(cx, |state, cx| {
            state.set_value(name, window, cx);
            // Selected, not just placed at the end: `set_value` leaves the caret
            // after the text, so typing a new name would append to the old one.
            state.select_all(window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.renaming.take() else {
            return;
        };
        let name = self.collection_name.read(cx).value().trim().to_string();
        if !name.is_empty() {
            self.workspace
                .update(cx, |workspace, cx| {
                    workspace.rename_collection(id, name, cx)
                })
                .detach();
        }
        cx.notify();
    }

    fn add_collection(&mut self, parent: Option<i64>, window: &mut Window, cx: &mut Context<Self>) {
        let collections = self.workspace.read(cx).collections().to_vec();
        if !tree::can_nest_inside(&collections, parent) {
            return self.complain(
                format!("Collections nest {} deep at most.", tree::MAX_DEPTH),
                cx,
            );
        }

        let creating = self.workspace.update(cx, |workspace, cx| {
            workspace.create_collection("New collection".to_string(), parent, cx)
        });

        cx.spawn_in(window, async move |panel, window| {
            let Some(collection) = creating.await else {
                return;
            };
            window
                .update(|window, cx| {
                    panel
                        .update(cx, |panel, cx| {
                            // Straight into a rename: a collection called "New
                            // collection" is not what anybody wanted, and making
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

    /// Applies a drop. The tree decides whether it is allowed; this reports why
    /// when it is not, because a drop that silently does nothing is the worst of
    /// the three outcomes.
    fn accept_drop(&mut self, dragged: &Dragged, onto: Option<i64>, cx: &mut Context<Self>) {
        match dragged {
            Dragged::Request { id, .. } => {
                let id = *id;
                self.workspace
                    .update(cx, |workspace, cx| workspace.move_request(id, onto, cx))
                    .detach();
            }
            Dragged::Collection { id, .. } => {
                let id = *id;
                let collections = self.workspace.read(cx).collections().to_vec();
                match tree::check_move(&collections, id, onto) {
                    Ok(()) => self
                        .workspace
                        .update(cx, |workspace, cx| workspace.move_collection(id, onto, cx))
                        .detach(),
                    Err(tree::Refusal::WouldDetachItself) => {
                        self.complain("A collection cannot go inside itself.", cx)
                    }
                    Err(tree::Refusal::TooDeep) => self.complain(
                        format!("That would nest deeper than {}.", tree::MAX_DEPTH),
                        cx,
                    ),
                }
            }
        }
        cx.notify();
    }

    fn complain(&self, message: impl Into<String>, cx: &mut Context<Self>) {
        cx.emit(CollectionsEvent::Complain(message.into()));
    }

    #[allow(clippy::too_many_arguments)]
    fn collection_row(
        &self,
        id: i64,
        name: &str,
        depth: usize,
        total: usize,
        expanded: bool,
        can_nest: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.renaming == Some(id) {
            return h_flex()
                .w_full()
                .h(px(COLLECTION_HEIGHT))
                .items_center()
                .child(tokens::rails(depth, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .pl_1()
                        .pr_2()
                        .child(Input::new(&self.collection_name).xsmall()),
                )
                .into_any_element();
        }

        let collections = self.workspace.read(cx).collections().to_vec();

        h_flex()
            .id(("collection", id as usize))
            .w_full()
            .h(px(COLLECTION_HEIGHT))
            .items_center()
            .rounded(cx.theme().radius)
            .hover(|row| row.bg(cx.theme().sidebar_accent.opacity(0.6)))
            // The target lights up only for a drop it would accept, so an
            // impossible move looks impossible before it is attempted.
            .drag_over::<Dragged>(move |style, dragged: &Dragged, _, cx| {
                let allowed = match dragged {
                    Dragged::Request { .. } => true,
                    Dragged::Collection { id: dragged, .. } => {
                        tree::can_move(&collections, *dragged, Some(id))
                    }
                };
                if allowed {
                    style
                        .bg(cx.theme().sidebar_accent)
                        .border_1()
                        .border_color(cx.theme().ring)
                } else {
                    style
                }
            })
            .on_drop(cx.listener(move |this, dragged: &Dragged, _, cx| {
                this.accept_drop(dragged, Some(id), cx)
            }))
            .on_drag(
                Dragged::Collection {
                    id,
                    name: name.to_string(),
                },
                |dragged, _, _, cx| {
                    let label = dragged.name().to_string();
                    cx.new(|_| DragPreview { label })
                },
            )
            .child(tokens::rails(depth, cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .pl_1()
                    .pr_2()
                    .gap_1p5()
                    .items_center()
                    // Rotated rather than swapped: the caret turning is what
                    // reads as opening, and it is how the library's own menus
                    // behave.
                    .child(
                        gpui_component::Icon::new(IconName::ChevronRight)
                            .size_3()
                            .text_color(cx.theme().muted_foreground)
                            .when(expanded, |icon| icon.rotate(gpui::percentage(0.25))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(cx.theme().sidebar_foreground)
                            .truncate()
                            .child(name.to_string()),
                    )
                    .when(total > 0, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .px_1p5()
                                .rounded_full()
                                .bg(cx.theme().sidebar_accent)
                                .text_size(px(10.))
                                .text_color(cx.theme().muted_foreground)
                                .child(total.to_string()),
                        )
                    }),
            )
            .on_click(
                cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_collection(id, cx)),
            )
            .context_menu(move |mut menu, _window, _cx| {
                // Offered only where it is possible: a menu item that explains
                // itself by failing is worse than one that is not there.
                if can_nest {
                    menu = menu.menu("New collection inside", Box::new(AddCollection(id)));
                }
                menu.menu("Rename", Box::new(RenameCollection(id)))
                    .separator()
                    .menu("Delete collection", Box::new(DeleteCollection(id)))
            })
            .into_any_element()
    }

    fn row(&self, request: &Request, depth: usize, cx: &mut Context<Self>) -> AnyElement {
        let id = request.id;
        let selected = self.selected == Some(id);
        let target = request.target.clone();
        let kind = request.kind;
        let colour = tokens::kind_color(kind, cx);

        h_flex()
            .id(("request", id as usize))
            .w_full()
            .h(px(ROW_HEIGHT))
            .items_center()
            .rounded(cx.theme().radius)
            .when(selected, |row| {
                row.bg(cx.theme().sidebar_accent)
                    .text_color(cx.theme().sidebar_accent_foreground)
            })
            .when(!selected, |row| {
                row.hover(|row| row.bg(cx.theme().sidebar_accent.opacity(0.6)))
            })
            .child(tokens::rails(depth, cx))
            // A bar in the kind's colour on the selected row: the one place the
            // colour appears at full strength, so the eye finds the current
            // request without the list having to shout anywhere else.
            .child(
                div()
                    .flex_none()
                    .w(px(2.))
                    .h(px(ROW_HEIGHT - 12.))
                    .rounded_full()
                    .when(selected, |bar| bar.bg(colour)),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .pl_2()
                    .pr_2()
                    .gap_2()
                    .items_center()
                    .child(tokens::kind_badge(kind, cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(1.))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(if selected {
                                        cx.theme().sidebar_accent_foreground
                                    } else {
                                        cx.theme().sidebar_foreground
                                    })
                                    .truncate()
                                    .child(request.name.clone()),
                            )
                            .when(!target.is_empty(), |stack| {
                                stack.child(
                                    tokens::mono(cx)
                                        .text_size(px(10.))
                                        .text_color(cx.theme().muted_foreground)
                                        .truncate()
                                        .child(target.clone()),
                                )
                            }),
                    ),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.selected = Some(id);
                cx.emit(CollectionsEvent::Open(id));
                cx.notify();
            }))
            // Dragged, not moved through a submenu: a list of every collection in
            // a menu is a workaround for not being able to point at one.
            .on_drag(
                Dragged::Request {
                    id,
                    name: request.name.clone(),
                },
                |dragged, _, _, cx| {
                    let label = dragged.name().to_string();
                    cx.new(|_| DragPreview { label })
                },
            )
            .context_menu(move |menu, _window, _cx| {
                menu.menu("Open", Box::new(OpenRequest(id)))
                    .menu("Duplicate", Box::new(DuplicateRequest(id)))
                    .separator()
                    .menu("Delete", Box::new(DeleteRequest(id)))
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

    /// The panel's own actions, drawn by the dock beside its tab.
    ///
    /// This is the slot a dock provides for exactly this, and using it leaves the
    /// panel's own header free to do one thing: search.
    fn toolbar_buttons(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Vec<Button>> {
        Some(vec![
            Button::new("new-collection")
                .ghost()
                .xsmall()
                .icon(IconName::Folder)
                .tooltip("New collection")
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.add_collection(None, window, cx);
                })),
            Button::new("new-request")
                .ghost()
                .xsmall()
                .icon(IconName::Plus)
                .tooltip("New request")
                .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                    cx.emit(CollectionsEvent::New);
                })),
        ])
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
        let plan = tree::rows(
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
                tree::Row::Collection {
                    id,
                    name,
                    depth,
                    total,
                    expanded,
                    can_nest,
                } => Some(self.collection_row(*id, name, *depth, *total, *expanded, *can_nest, cx)),
                tree::Row::Request { id, depth } => {
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
            .on_action(cx.listener(|this, action: &AddCollection, window, cx| {
                this.add_collection(Some(action.0), window, cx);
            }))
            .on_action(cx.listener(|this, action: &RenameCollection, window, cx| {
                this.start_rename(action.0, window, cx);
            }))
            .on_action(cx.listener(|this, action: &DeleteCollection, _, cx| {
                let id = action.0;
                this.collapsed.remove(&id);
                this.workspace
                    .update(cx, |workspace, cx| workspace.delete_collection(id, cx))
                    .detach();
            }))
            .child(
                // Search alone. The two actions moved to the dock's own toolbar,
                // beside the panel's tab, which is where a dock puts a panel's
                // controls — and it leaves this row to do one thing.
                div().flex_shrink_0().px_2().pt_2().pb_1p5().child(
                    Input::new(&self.search).xsmall().cleanable(true).prefix(
                        gpui_component::Icon::new(IconName::Search)
                            .size_3()
                            .text_color(cx.theme().muted_foreground),
                    ),
                ),
            )
            .child(if rows.is_empty() && !searching {
                self.empty_state(cx)
            } else {
                v_flex()
                    .id("request-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_1p5()
                    .pb_2()
                    .gap(px(1.))
                    // The list's own background is the root, so dragging
                    // something out of a collection is the same gesture as
                    // dragging it in — there is no "move to top level" command to
                    // find.
                    .drag_over::<Dragged>(|style, _, _, cx| style.bg(cx.theme().list_hover))
                    .on_drop(cx.listener(|this, dragged: &Dragged, _, cx| {
                        this.accept_drop(dragged, None, cx)
                    }))
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
