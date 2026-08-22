//! The sidebar: saved requests, arranged into collections.
//!
//! Built on `gpui_component::tree`, which owns the flattening, indentation,
//! expansion, virtualisation, keyboard navigation and scrollbar. This file
//! supplies the row content and the rules the component cannot know — how deep
//! collections may nest, and which drops would detach a branch.

use std::collections::{HashMap, HashSet};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    list::ListItem,
    menu::ContextMenuExt as _,
    tree::{Tree, TreeEvent, TreeItem, TreeState},
    v_flex,
};
use rw_core::domain::{Collection, Request, RequestKind};

use crate::nesting;
use crate::runs::{RunState, Runs};
use crate::session::RobotWhisperer;
use crate::tokens;
use crate::workspace::Workspace;

/// Right-click actions. Each carries the row's id, so one action serves every row.
#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct OpenRequest(pub i64);

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct DuplicateRequest(pub i64);

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct DeleteRequest(pub i64);

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct RenameRequest(pub i64);

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct AddCollection(pub i64);

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct RenameCollection(pub i64);

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct DeleteCollection(pub i64);

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct DeleteDashboard(pub i64);

/// What the sidebar asks the shell to do.
#[derive(Debug, Clone)]
pub enum CollectionsEvent {
    Open(i64),
    Duplicate(i64),
    Delete(i64),
    New,
    /// Open a saved dashboard.
    OpenDashboard(i64),
    /// Create one and open it.
    NewDashboard,
    DeleteDashboard(i64),
    /// Something the user tried that could not be done, for the console.
    Complain(String),
}

/// Which row a tree entry stands for.
///
/// The component keys entries by string, so the two kinds of row are told apart
/// by a prefix. One place encodes it and one decodes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Collection(i64),
    Request(i64),
}

impl Row {
    fn id(self) -> SharedString {
        match self {
            Row::Collection(id) => format!("c{id}").into(),
            Row::Request(id) => format!("r{id}").into(),
        }
    }

    fn parse(id: &str) -> Option<Self> {
        let (tag, rest) = id.split_at_checked(1)?;
        let value = rest.parse().ok()?;
        match tag {
            "c" => Some(Row::Collection(value)),
            "r" => Some(Row::Request(value)),
            _ => None,
        }
    }
}

/// Everything a row needs to draw itself.
///
/// Snapshotted before the tree renders rather than read back out of the panel:
/// the component builds its rows while the panel is still mid-render, and
/// reaching into the panel from there is a borrow waiting to fail.
#[derive(Clone)]
struct RowData {
    name: SharedString,
    /// `None` for a collection. This is also what tells the two kinds of row
    /// apart when drawing one, so the renderer needs no second flag.
    kind: Option<RequestKind>,
    state: RunState,
    renaming: bool,
}

impl RowData {
    fn dragged(&self, row: Row) -> Dragged {
        match row {
            Row::Request(id) => Dragged::Request {
                id,
                name: self.name.clone(),
            },
            Row::Collection(id) => Dragged::Collection {
                id,
                name: self.name.clone(),
            },
        }
    }
}

/// What is being dragged.
#[derive(Clone)]
pub(crate) enum Dragged {
    Request { id: i64, name: SharedString },
    Collection { id: i64, name: SharedString },
}

impl Dragged {
    fn name(&self) -> SharedString {
        match self {
            Dragged::Request { name, .. } | Dragged::Collection { name, .. } => name.clone(),
        }
    }
}

/// What follows the pointer while dragging: the name on a chip, because the row
/// itself would be as wide as the sidebar and cover the target.
struct DragPreview {
    label: SharedString,
}

impl Render for DragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_0p5()
            .rounded(cx.theme().radius)
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .text_xs()
            .child(self.label.clone())
    }
}

pub struct CollectionsPanel {
    focus_handle: FocusHandle,
    workspace: Entity<Workspace>,
    runs: Entity<Runs>,
    search: Entity<InputState>,
    tree: Entity<TreeState>,

    /// Which collections are open. Kept here rather than read back off the tree
    /// because the items are rebuilt whenever the workspace changes, and an
    /// expansion that lives only on a rebuilt item does not survive.
    expanded: HashSet<i64>,
    /// What the tree was last built from, so it is rebuilt when that changes and
    /// not on every frame — rebuilding resets the component's selection.
    built_from: String,
    /// The request the shell has open, so the row reads as current.
    selected: Option<i64>,
    /// Which dashboard row is highlighted, kept apart from `selected` so
    /// opening a dashboard does not un-highlight the request above it.
    selected_dashboard: Option<i64>,

    /// The collection being renamed, and the field holding the new name.
    /// The row being renamed, whichever kind it is.
    renaming: Option<Row>,
    collection_name: Entity<InputState>,

    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CollectionsEvent> for CollectionsPanel {}
impl EventEmitter<PanelEvent> for CollectionsPanel {}

impl CollectionsPanel {
    pub fn new(workspace: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let runs = RobotWhisperer::global(cx).runs.clone();
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search requests"));
        let collection_name =
            cx.new(|cx| InputState::new(window, cx).placeholder("Collection name"));
        let tree = cx.new(|cx| TreeState::new(cx));

        let subscriptions = vec![
            cx.observe(&workspace, |_, _, cx| cx.notify()),
            cx.observe(&runs, |_, _, cx| cx.notify()),
            cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            // Enter commits the name; clicking away commits it too, because
            // losing a name by clicking elsewhere would be its own small
            // betrayal.
            cx.subscribe(&collection_name, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit_rename(cx);
                }
            }),
            cx.subscribe(&tree, |this, _, event: &TreeEvent, cx| {
                let (TreeEvent::Expanded(id) | TreeEvent::Collapsed(id)) = event;
                if let Some(Row::Collection(collection)) = Row::parse(id) {
                    match event {
                        TreeEvent::Expanded(_) => this.expanded.insert(collection),
                        TreeEvent::Collapsed(_) => this.expanded.remove(&collection),
                    };
                    cx.notify();
                }
            }),
        ];

        Self {
            focus_handle: cx.focus_handle(),
            workspace,
            runs,
            search,
            tree,
            expanded: HashSet::new(),
            built_from: String::new(),
            selected: None,
            selected_dashboard: None,
            renaming: None,
            collection_name,
            _subscriptions: subscriptions,
        }
    }

    pub fn view(workspace: Entity<Workspace>, window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(workspace, window, cx))
    }

    /// Marks a request as current. The shell calls this when one is opened from
    /// anywhere, so the sidebar stays in step.
    pub fn select(&mut self, request: Option<i64>, cx: &mut Context<Self>) {
        self.selected = request;
        cx.notify();
    }

    // ── building the tree ──────────────────────────────────────────────────────

    /// Rebuilds the component's items when what they are built from has changed.
    ///
    /// Cheap to call every frame, and it has to be: the alternative is rebuilding
    /// unconditionally, which resets the component's own selection and scroll.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().trim().to_lowercase();
        let workspace = self.workspace.read(cx);
        let collections = workspace.collections().to_vec();
        let requests: Vec<Request> = workspace
            .requests()
            .iter()
            .filter(|request| {
                query.is_empty()
                    || request.name.to_lowercase().contains(&query)
                    || request.target.to_lowercase().contains(&query)
            })
            .cloned()
            .collect();

        // Searching opens everything: a match behind a closed collection makes
        // the search look as though it found nothing.
        let searching = !query.is_empty();
        let signature = format!(
            "{query}|{:?}|{:?}|{:?}",
            collections
                .iter()
                .map(|c| (c.id, c.parent_id, &c.name))
                .collect::<Vec<_>>(),
            requests
                .iter()
                .map(|r| (r.id, r.collection_id, &r.name, r.kind))
                .collect::<Vec<_>>(),
            self.expanded,
        );
        if signature == self.built_from {
            return;
        }
        self.built_from = signature;

        let items = self.items(&collections, &requests, None, 0, searching);
        self.tree.update(cx, |tree, cx| tree.set_items(items, cx));
    }

    /// The items under `parent`, collections first — the convention every file
    /// tree follows.
    fn items(
        &self,
        collections: &[Collection],
        requests: &[Request],
        parent: Option<i64>,
        depth: usize,
        searching: bool,
    ) -> Vec<TreeItem> {
        if depth >= nesting::MAX_DEPTH {
            return Vec::new();
        }

        let mut items: Vec<TreeItem> = collections
            .iter()
            .filter(|collection| collection.parent_id == parent)
            .map(|collection| {
                let children = self.items(
                    collections,
                    requests,
                    Some(collection.id),
                    depth + 1,
                    searching,
                );
                let open = searching || self.expanded.contains(&collection.id);
                TreeItem::new(Row::Collection(collection.id).id(), collection.name.clone())
                    .children(children)
                    .expanded(open)
            })
            .collect();

        items.extend(
            requests
                .iter()
                .filter(|request| request.collection_id == parent)
                .map(|request| TreeItem::new(Row::Request(request.id).id(), request.name.clone())),
        );

        // Requests whose collection is missing, deleted, or unreachable would
        // otherwise vanish with it. At the root, where they can be found.
        if parent.is_none() {
            let reachable = |id: Option<i64>| {
                id.is_none_or(|id| collections.iter().any(|entry| entry.id == id))
            };
            items.extend(
                requests
                    .iter()
                    .filter(|request| !reachable(request.collection_id))
                    .map(|request| {
                        TreeItem::new(Row::Request(request.id).id(), request.name.clone())
                    }),
            );
        }

        items
    }

    // ── rows ───────────────────────────────────────────────────────────────────

    /// What every row needs, keyed by the id the tree uses.
    fn snapshot(&self, cx: &App) -> HashMap<SharedString, RowData> {
        let workspace = self.workspace.read(cx);
        let runs = self.runs.read(cx);

        let collections = workspace.collections().iter().map(|collection| {
            (
                Row::Collection(collection.id).id(),
                RowData {
                    name: collection.name.clone().into(),
                    kind: None,
                    state: RunState::Idle,
                    renaming: self.renaming == Some(Row::Collection(collection.id)),
                },
            )
        });

        let requests = workspace.requests().iter().map(|request| {
            (
                Row::Request(request.id).id(),
                RowData {
                    name: request.name.clone().into(),
                    kind: Some(request.kind),
                    state: runs.get(request.id),
                    renaming: self.renaming == Some(Row::Request(request.id)),
                },
            )
        });

        collections.chain(requests).collect()
    }

    // ── collections ────────────────────────────────────────────────────────────

    /// Renames a row in place. Collections and requests alike: a name is a
    /// name, and the sidebar is where you can see the one you are changing
    /// next to its neighbours.
    fn start_rename(&mut self, row: Row, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace.read(cx);
        let name = match row {
            Row::Collection(id) => workspace
                .collections()
                .iter()
                .find(|collection| collection.id == id)
                .map(|collection| collection.name.clone()),
            Row::Request(id) => workspace
                .requests()
                .iter()
                .find(|request| request.id == id)
                .map(|request| request.name.clone()),
        }
        .unwrap_or_default();

        self.renaming = Some(row);
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
        let Some(row) = self.renaming.take() else {
            return;
        };
        let name = self.collection_name.read(cx).value().trim().to_string();
        if !name.is_empty() {
            self.workspace
                .update(cx, |workspace, cx| match row {
                    Row::Collection(id) => workspace.rename_collection(id, name, cx),
                    Row::Request(id) => workspace.rename_request(id, name, cx),
                })
                .detach();
        }
        cx.notify();
    }

    fn add_collection(&mut self, parent: Option<i64>, window: &mut Window, cx: &mut Context<Self>) {
        let collections = self.workspace.read(cx).collections().to_vec();
        if !nesting::can_nest_inside(&collections, parent) {
            return self.complain(
                format!("Collections nest {} deep at most.", nesting::MAX_DEPTH),
                cx,
            );
        }

        // Open the parent, or the new one appears nowhere.
        if let Some(parent) = parent {
            self.expanded.insert(parent);
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
                            // them find the rename command is a step with no
                            // decision in it.
                            panel.start_rename(Row::Collection(collection.id), window, cx)
                        })
                        .ok();
                })
                .ok();
        })
        .detach();
    }

    /// Applies a drop. The nesting rules decide whether it is allowed; this
    /// reports why when it is not, because a drop that silently does nothing is
    /// the worst of the three outcomes.
    fn accept_drop(&mut self, dragged: &Dragged, onto: Option<i64>, cx: &mut Context<Self>) {
        match dragged {
            Dragged::Request { id, .. } => {
                let id = *id;
                if let Some(collection) = onto {
                    self.expanded.insert(collection);
                }
                self.workspace
                    .update(cx, |workspace, cx| workspace.move_request(id, onto, cx))
                    .detach();
            }
            Dragged::Collection { id, .. } => {
                let id = *id;
                let collections = self.workspace.read(cx).collections().to_vec();
                match nesting::check_move(&collections, id, onto) {
                    Ok(()) => {
                        if let Some(collection) = onto {
                            self.expanded.insert(collection);
                        }
                        self.workspace
                            .update(cx, |workspace, cx| workspace.move_collection(id, onto, cx))
                            .detach();
                    }
                    Err(nesting::Refusal::WouldDetachItself) => {
                        self.complain("A collection cannot go inside itself.", cx)
                    }
                    Err(nesting::Refusal::TooDeep) => self.complain(
                        format!("That would nest deeper than {}.", nesting::MAX_DEPTH),
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
}

/// One row, in the shape a file manager uses: a single line, indented by its
/// depth, with the disclosure arrow only where there is something to disclose.
fn render_row(
    index: usize,
    data: &RowData,
    depth: usize,
    expanded: bool,
    rename_field: &Entity<InputState>,
    cx: &App,
) -> ListItem {
    let indent = tokens::scaled(tokens::SIDEBAR_INDENT, depth as f32);

    if data.renaming {
        return ListItem::new(index).child(
            h_flex()
                .w_full()
                .pl(indent)
                .child(Input::new(rename_field).xsmall()),
        );
    }

    let mut row = h_flex().w_full().gap_1p5().items_center().pl(indent);
    match data.kind {
        // A collection: the disclosure arrow, then the folder itself.
        None => {
            row = row
                .child(
                    Icon::new(IconName::ChevronRight)
                        .size_3()
                        .text_color(cx.theme().muted_foreground)
                        .when(expanded, |icon| icon.rotate(gpui::percentage(0.25))),
                )
                .child(
                    Icon::new(if expanded {
                        IconName::FolderOpen
                    } else {
                        IconName::Folder
                    })
                    .size_3p5()
                    .text_color(cx.theme().muted_foreground),
                );
        }
        // A request: a space where the arrow would be, so names line up whether
        // or not the row above can be opened, then the kind.
        Some(kind) => {
            row = row.child(div().w(tokens::KIND_GUTTER).flex_none()).child(
                tokens::mono(cx)
                    .flex_none()
                    .w(tokens::KIND_WIDTH)
                    .text_size(tokens::KIND_TEXT)
                    .text_color(tokens::kind_color(kind, cx))
                    .child(tokens::kind_short(kind)),
            );
        }
    }

    ListItem::new(index).child(
        row.child(div().flex_1().min_w_0().truncate().child(data.name.clone()))
            // The rate beside the dot, when the row is a live subscription:
            // `ros2 topic hz` without leaving the list you are already reading.
            .when_some(rate(&data.state, cx), |row, rate| {
                row.child(
                    div()
                        .flex_none()
                        .mr_1p5()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(rate),
                )
            })
            .when_some(state_dot(&data.state, cx), |row, dot| row.child(dot)),
    )
}

/// How fast a live subscription's topic is going, for the row it is on.
fn rate(state: &RunState, cx: &App) -> Option<SharedString> {
    let RunState::Live(Some(handle)) = state else {
        return None;
    };
    let label = RobotWhisperer::global(cx)
        .sessions
        .read(cx)
        .pipeline()
        .stats(handle)?
        .hz_label()?;
    Some(SharedString::from(label))
}

/// A dot on the row's right, and only when there is something to say.
///
/// Idle is the overwhelming majority of rows, and marking it would mean marking
/// everything.
fn state_dot(state: &RunState, cx: &App) -> Option<gpui::AnyElement> {
    let colour = match state {
        RunState::Idle => return None,
        RunState::Live(_) => cx.theme().success,
        RunState::Failed(_) => cx.theme().danger,
    };
    Some(
        div()
            .flex_none()
            .mr_1()
            .size(tokens::designed(6.))
            .rounded_full()
            .bg(colour)
            .into_any_element(),
    )
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

impl CollectionsPanel {
    /// The dashboards section: a heading, a way to add one, and the list.
    ///
    /// Flat rather than a second tree. A dashboard is a whole screen of work
    /// and people keep a handful of them, not a filing system — the tree above
    /// is for requests, which arrive in the hundreds.
    fn dashboards(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let dashboards = self.workspace.read(cx).dashboards().to_vec();
        let selected = self.selected_dashboard;

        v_flex()
            .flex_shrink_0()
            .max_h(tokens::designed(220.))
            .border_t_1()
            .border_color(cx.theme().sidebar_border)
            .child(
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_between()
                    .px_2p5()
                    .py_1p5()
                    .child(tokens::section_label("Dashboards", cx))
                    .child(
                        Button::new("new-dashboard")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .tooltip("New dashboard")
                            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                                cx.emit(CollectionsEvent::NewDashboard);
                            })),
                    ),
            )
            .child(
                v_flex()
                    .id("dashboards")
                    .flex_1()
                    .min_h_0()
                    .px_1()
                    .pb_1()
                    .overflow_y_scroll()
                    .when(dashboards.is_empty(), |list| {
                        list.child(
                            div()
                                .px_2()
                                .py_1()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("None yet."),
                        )
                    })
                    .children(
                        dashboards
                            .into_iter()
                            .enumerate()
                            .map(|(index, dashboard)| {
                                let id = dashboard.id;
                                ListItem::new(("dashboard", index))
                                    .selected(selected == Some(id))
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_1p5()
                                            .items_center()
                                            .child(div().w(tokens::KIND_GUTTER).flex_none())
                                            .child(
                                                tokens::mono(cx)
                                                    .flex_none()
                                                    .w(tokens::KIND_WIDTH)
                                                    .text_size(tokens::KIND_TEXT)
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child("DASH"),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .child(dashboard.name.clone()),
                                            ),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.selected_dashboard = Some(id);
                                        cx.emit(CollectionsEvent::OpenDashboard(id));
                                        cx.notify();
                                    }))
                                    .context_menu(move |menu, _, _| {
                                        menu.menu("Delete dashboard", Box::new(DeleteDashboard(id)))
                                    })
                            }),
                    ),
            )
    }
}

impl Render for CollectionsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync(cx);

        let selected = self.selected;
        let collections = self.workspace.read(cx).collections().to_vec();
        let empty = self.workspace.read(cx).requests().is_empty()
            && collections.is_empty()
            && self.search.read(cx).value().trim().is_empty();

        // Everything the rows need, gathered before the tree renders. The
        // component builds its rows while this panel is still mid-render, so
        // reaching back into the panel from there is a borrow waiting to fail.
        let rows: HashMap<SharedString, RowData> = self.snapshot(cx);
        let rename_field = self.collection_name.clone();

        let tree = Tree::new(&self.tree, {
            let rows = rows.clone();
            let rename_field = rename_field.clone();
            let panel = cx.entity();
            let collections = collections.clone();
            move |index, entry, _selected, _window, cx| {
                let id = entry.item().id.clone();
                let Some((row, data)) = Row::parse(&id).zip(rows.get(&id)) else {
                    return ListItem::new(index);
                };
                let item = render_row(
                    index,
                    data,
                    entry.depth(),
                    entry.is_expanded(),
                    &rename_field,
                    cx,
                );
                let dragged = data.dragged(row);

                match row {
                    Row::Request(id) => item
                        .selected(selected == Some(id))
                        .on_click({
                            let panel = panel.clone();
                            move |_, _, cx| {
                                panel.update(cx, |panel, cx| {
                                    panel.selected = Some(id);
                                    cx.emit(CollectionsEvent::Open(id));
                                    cx.notify();
                                });
                            }
                        })
                        .on_drag(dragged, |dragged, _, _, cx| {
                            let label = dragged.name();
                            cx.new(|_| DragPreview { label })
                        }),
                    Row::Collection(id) => {
                        let collections = collections.clone();
                        item.drag_over::<Dragged>(move |style, dragged: &Dragged, _, cx| {
                            // Lit only for a drop it would accept, so an
                            // impossible move looks impossible before it is
                            // attempted.
                            let allowed = match dragged {
                                Dragged::Request { .. } => true,
                                Dragged::Collection { id: moving, .. } => {
                                    nesting::can_move(&collections, *moving, Some(id))
                                }
                            };
                            if allowed {
                                style.bg(cx.theme().sidebar_accent)
                            } else {
                                style
                            }
                        })
                        .on_drop({
                            let panel = panel.clone();
                            move |dragged: &Dragged, _, cx| {
                                let dragged = dragged.clone();
                                panel.update(cx, |panel, cx| {
                                    panel.accept_drop(&dragged, Some(id), cx)
                                });
                            }
                        })
                        .on_drag(dragged, |dragged, _, _, cx| {
                            let label = dragged.name();
                            cx.new(|_| DragPreview { label })
                        })
                    }
                }
            }
        })
        .context_menu({
            let collections = collections.clone();
            move |_index, entry, menu, _window, _cx| {
                let Some(row) = Row::parse(&entry.item().id) else {
                    return menu;
                };
                match row {
                    Row::Request(id) => menu
                        .menu("Open", Box::new(OpenRequest(id)))
                        .menu("Rename", Box::new(RenameRequest(id)))
                        .menu("Duplicate", Box::new(DuplicateRequest(id)))
                        .separator()
                        .menu("Delete", Box::new(DeleteRequest(id))),
                    Row::Collection(id) => {
                        let mut menu = menu;
                        // Offered only where it is possible: a menu item that
                        // explains itself by failing is worse than one that is
                        // not there.
                        if nesting::can_nest_inside(&collections, Some(id)) {
                            menu = menu.menu("New collection", Box::new(AddCollection(id)));
                        }
                        menu.menu("Rename", Box::new(RenameCollection(id)))
                            .separator()
                            .menu("Delete", Box::new(DeleteCollection(id)))
                    }
                }
            }
        });

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
                this.start_rename(Row::Collection(action.0), window, cx);
            }))
            .on_action(cx.listener(|this, action: &RenameRequest, window, cx| {
                this.start_rename(Row::Request(action.0), window, cx);
            }))
            .on_action(cx.listener(|this, action: &DeleteCollection, _, cx| {
                let id = action.0;
                this.expanded.remove(&id);
                this.workspace
                    .update(cx, |workspace, cx| workspace.delete_collection(id, cx))
                    .detach();
            }))
            // Flush against the panel rather than floating in it: a search row
            // in a file tree is part of the chrome, not a control sitting on top
            // of it. The hairline below is what separates it from the rows.
            .child(
                div()
                    .flex_shrink_0()
                    .px_2p5()
                    .py_1p5()
                    .border_b_1()
                    .border_color(cx.theme().sidebar_border)
                    .child(
                        Input::new(&self.search)
                            .xsmall()
                            .appearance(false)
                            .cleanable(true),
                    ),
            )
            .on_action(cx.listener(|_, action: &DeleteDashboard, _, cx| {
                cx.emit(CollectionsEvent::DeleteDashboard(action.0));
            }))
            .child(if empty {
                tokens::empty_state(
                    IconName::Inbox,
                    "No requests yet",
                    "Save a topic, service or action call to reuse it later.",
                    cx,
                )
                .into_any_element()
            } else {
                div()
                    .flex_1()
                    .min_h_0()
                    .px_1()
                    // The root drop target, so dragging something out of a
                    // collection is the same gesture as dragging it in.
                    .drag_over::<Dragged>(|style, _, _, cx| style.bg(cx.theme().sidebar_accent))
                    .on_drop(cx.listener(|this, dragged: &Dragged, _, cx| {
                        this.accept_drop(dragged, None, cx)
                    }))
                    .child(tree)
                    .into_any_element()
            })
            .child(self.dashboards(cx))
    }
}
