//! The world pane: one scene, many layers, one fixed frame.
//!
//! Replaces the robot pane and the cloud path in `views::visualize`. Those each
//! showed one thing in whatever frame it arrived in, which is the model this
//! whole phase exists to leave behind: a scan is in `laser`, a map is in `map`,
//! a robot is in `base_link`, and drawn without a transform tree they all sit on
//! top of one another at the origin looking plausible.
//!
//! So the pane has a **fixed frame** — the single most important control in
//! RViz — and every layer is resolved into it through the connection's
//! transform buffer. A layer that will not resolve is dimmed and says exactly
//! why, because a layer quietly drawn in the wrong place is worse than one
//! that is missing: nothing about it looks wrong.
//!
//! The discipline the rest of the app follows holds here: no settings tree, no
//! submenus, no chrome row repeating the tab title. The layer list is content —
//! it says what is in the world, which frame each thing came in, and what is
//! broken — and the handful of controls that matter sit on the flat menu the
//! dock already draws.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div, px,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::menu::DropdownMenu as _;
use gpui_component::{
    ActiveTheme as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use rw_assets::catalog::{Catalog, Loaded};
use rw_assets::kinematics::{self, Pose};
use rw_assets::math;
use rw_canonical::CanonicalValue;
use rw_render::{Content, Layer, MeshVertex, Solid};
use rw_tf::Buffer;

use crate::actions::{AddWorldRobot, RemoveWorldLayer, SetWorldFrame};
use crate::scene_view::SceneView;
use crate::session::{RobotWhisperer, Sessions};
use crate::workspace::Workspace;
use crate::{tokens, viz};

/// How wide the layer rail is.
///
/// The same 240 the robot pane spent on its joint list: enough for a topic name
/// and the frame it came in, and no more — the picture is what the pane is for.
const RAIL: f32 = 240.;

/// What a layer is showing.
enum Source {
    /// A live topic on a connection.
    Topic { connection: i64, topic: String },
    /// A robot from the catalog, hung off a frame of the tree.
    Robot {
        id: String,
        /// The frame the robot's root link is bolted to.
        ///
        /// Defaults to the description's own root link name. A description and
        /// a running system rarely agree on that — the description says
        /// `panda_link0` and the robot publishes `base_link` — so it is the one
        /// per-layer control that has to exist, and it is on the flat menu.
        anchor: String,
        loaded: Option<Box<Robot>>,
    },
}

/// A catalog robot, ready to draw.
struct Robot {
    /// One entry per link: its frame name, its geometry, and where the
    /// description puts it relative to the robot's root.
    links: Vec<Link>,
}

struct Link {
    frame: String,
    key: u64,
    vertices: Arc<Vec<MeshVertex>>,
    /// Where the description puts this link inside the robot's root frame.
    local: math::Mat4,
}

/// What has arrived on a layer's topic.
#[derive(Default)]
struct Incoming {
    value: Option<CanonicalValue>,
    schema: Option<String>,
    count: u64,
}

/// One thing in the world.
struct WorldLayer {
    source: Source,
    name: SharedString,
    /// Turned off by the user. A hidden layer keeps its place and its upload,
    /// so turning it back on is instant.
    shown: bool,
    incoming: Arc<Mutex<Incoming>>,
    subscription: Option<String>,
    /// The frame this layer's content arrived in, for its row.
    frame: Option<SharedString>,
    /// Why it is not being drawn. Refreshed on every rebuild.
    problem: Option<SharedString>,
    _work: Option<Task<()>>,
}

impl WorldLayer {
    fn topic(connection: i64, topic: String) -> Self {
        Self {
            name: SharedString::from(topic.clone()),
            source: Source::Topic { connection, topic },
            shown: true,
            incoming: Arc::new(Mutex::new(Incoming::default())),
            subscription: None,
            frame: None,
            problem: None,
            _work: None,
        }
    }

    fn robot(id: String, name: String) -> Self {
        Self {
            name: SharedString::from(name),
            source: Source::Robot {
                id,
                anchor: String::new(),
                loaded: None,
            },
            shown: true,
            incoming: Arc::new(Mutex::new(Incoming::default())),
            subscription: None,
            frame: None,
            problem: None,
            _work: None,
        }
    }

    fn connection(&self) -> Option<i64> {
        match &self.source {
            Source::Topic { connection, .. } => Some(*connection),
            Source::Robot { .. } => None,
        }
    }
}

pub struct WorldPanel {
    focus_handle: FocusHandle,
    scene: Entity<SceneView>,
    sessions: Entity<Sessions>,
    workspace: Entity<Workspace>,
    /// The frame everything is drawn in. Empty until something arrives to
    /// suggest one.
    fixed: String,
    layers: Vec<WorldLayer>,
    catalog: Option<Catalog>,
    problem: Option<SharedString>,
    home: crate::docking::Home,
    /// Bumped per robot load, so geometry swapped out never reuses a key.
    generation: u64,
    _repaint: Task<()>,
}

impl EventEmitter<PanelEvent> for WorldPanel {}

impl WorldPanel {
    pub fn view(cx: &mut App) -> Entity<Self> {
        let scene = SceneView::view(cx);
        let (workspace, sessions) = {
            let global = RobotWhisperer::global(cx);
            (global.workspace.clone(), global.sessions.clone())
        };
        let catalog = match Catalog::discover() {
            Ok(catalog) => Some(catalog),
            Err(error) => {
                tracing::warn!("no robot catalog: {error}");
                None
            }
        };

        cx.new(|cx| Self {
            focus_handle: cx.focus_handle(),
            scene,
            sessions,
            workspace,
            fixed: String::new(),
            layers: Vec::new(),
            catalog,
            problem: None,
            home: Default::default(),
            generation: 0,
            // Frames arrive off the UI thread, so the pane wakes itself to draw
            // them — the same beat a dashboard pane keeps.
            _repaint: cx.spawn(async move |panel, cx| {
                loop {
                    crate::tick::sleep(std::time::Duration::from_millis(100), cx).await;
                    if panel.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }),
        })
    }

    pub fn home(&self) -> Option<gpui::WeakEntity<gpui_component::dock::TabPanel>> {
        self.home.tab_panel()
    }

    /// Adds a topic to the world and subscribes it.
    pub fn add_topic(&mut self, connection: i64, topic: String, cx: &mut Context<Self>) {
        if self.layers.iter().any(|layer| {
            matches!(&layer.source, Source::Topic { connection: id, topic: name }
                if *id == connection && name == &topic)
        }) {
            return;
        }
        let mut layer = WorldLayer::topic(connection, topic.clone());
        let Some(session) = self.sessions.read(cx).session(connection) else {
            layer.problem = Some("That system is not connected.".into());
            self.layers.push(layer);
            cx.notify();
            return;
        };

        let pipeline = self.sessions.read(cx).pipeline();
        let incoming = Arc::clone(&layer.incoming);
        layer._work = Some(cx.spawn(async move |panel, cx| {
            let opened = pipeline
                .subscribe_topic(session, &topic, move |_handle, frame, _lossy| {
                    let Ok(mut incoming) = incoming.lock() else {
                        return;
                    };
                    incoming.schema = Some(frame.schema.name.clone());
                    incoming.value = Some(frame.value.clone());
                    incoming.count += 1;
                })
                .await;
            panel
                .update(cx, |panel, cx| {
                    let Some(layer) = panel.layers.iter_mut().find(|layer| {
                        matches!(&layer.source, Source::Topic { topic: name, .. } if name == &topic)
                    }) else {
                        return;
                    };
                    match opened {
                        Ok(opened) => layer.subscription = Some(opened.subscription_id),
                        Err(error) => layer.problem = Some(error.to_string().into()),
                    }
                    cx.notify();
                })
                .ok();
        }));
        self.layers.push(layer);
        cx.notify();
    }

    /// Adds a robot from the catalog and starts loading it.
    pub fn add_robot(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(catalog) = self.catalog.clone() else {
            self.problem = Some(
                "The robot models could not be found. Set RW_ASSETS to the assets directory."
                    .into(),
            );
            cx.notify();
            return;
        };
        if self.layers.iter().any(
            |layer| matches!(&layer.source, Source::Robot { id: existing, .. } if existing == id),
        ) {
            return;
        }
        let name = catalog
            .entries()
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.name.clone())
            .unwrap_or_else(|| id.to_string());

        let mut layer = WorldLayer::robot(id.to_string(), name);
        layer.problem = Some("Loading…".into());
        let wanted = id.to_string();
        layer._work = Some(cx.spawn(async move |panel, cx| {
            // Thirty megabytes of mesh must not be parsed on the thread that
            // draws.
            let loaded = cx
                .background_spawn({
                    let wanted = wanted.clone();
                    async move { catalog.load(&wanted).map_err(|error| error.to_string()) }
                })
                .await;
            panel
                .update(cx, |panel, cx| match loaded {
                    Ok(loaded) => panel.adopt(&wanted, loaded, cx),
                    Err(reason) => {
                        if let Some(layer) = panel.layer_mut(&wanted) {
                            layer.problem = Some(reason.into());
                        }
                        cx.notify();
                    }
                })
                .ok();
        }));
        self.layers.push(layer);
        cx.notify();
    }

    fn layer_mut(&mut self, robot: &str) -> Option<&mut WorldLayer> {
        self.layers
            .iter_mut()
            .find(|layer| matches!(&layer.source, Source::Robot { id, .. } if id == robot))
    }

    /// Takes on a freshly loaded robot.
    fn adopt(&mut self, id: &str, loaded: Loaded, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        // The description's own frames, posed at rest. Where the tree knows a
        // link's frame it wins — a running robot's joints are the truth and a
        // description's rest pose is only what is left when they are missing.
        let placed = kinematics::solve(&loaded.robot, &Pose::rest(&loaded.robot));
        let correction = loaded.entry.correction();
        let root = loaded
            .robot
            .root()
            .map(|link| link.name.clone())
            .unwrap_or_default();

        let links: Vec<Link> = loaded
            .meshes
            .iter()
            .enumerate()
            .filter_map(|(index, (link, parts))| {
                Some(Link {
                    frame: link.clone(),
                    key: generation << 32 | index as u64,
                    vertices: Arc::new(vertices(parts)),
                    local: math::multiply(correction, *placed.get(link)?),
                })
            })
            .collect();

        let Some(layer) = self.layer_mut(id) else {
            return;
        };
        layer.problem = None;
        if let Source::Robot { anchor, loaded, .. } = &mut layer.source {
            if anchor.is_empty() {
                *anchor = root;
            }
            *loaded = Some(Box::new(Robot { links }));
        }
        // A fresh robot gets a fresh view: the angle that suited a scan is not
        // the angle that suits a two-metre arm.
        self.scene.update(cx, |scene, cx| scene.reset(cx));
        cx.notify();
    }

    pub fn remove(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.layers.len() {
            return;
        }
        let layer = self.layers.remove(index);
        // The renderer's cache is keyed by ids the caller chose and cannot know
        // when geometry is finished with; without this it would only ever grow.
        if let Source::Robot {
            loaded: Some(robot),
            ..
        } = &layer.source
        {
            let keys: Vec<u64> = robot.links.iter().map(|link| link.key).collect();
            self.scene.read(cx).forget(&keys, cx);
        }
        if let Some(handle) = layer.subscription {
            let pipeline = self.sessions.read(cx).pipeline();
            cx.background_spawn(async move {
                pipeline.unsubscribe(&handle).await.ok();
            })
            .detach();
        }
        cx.notify();
    }

    pub fn toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(layer) = self.layers.get_mut(index) {
            layer.shown = !layer.shown;
            cx.notify();
        }
    }

    pub fn set_fixed(&mut self, frame: String, cx: &mut Context<Self>) {
        self.fixed = frame;
        // The scene has moved wholesale, so the camera goes back to something
        // that can see it — the fixed frame's origin is rarely where the last
        // one's was.
        self.scene.update(cx, |scene, cx| scene.reset(cx));
        cx.notify();
    }

    /// Hangs a robot layer off a different frame of the tree.
    pub fn set_anchor(&mut self, index: usize, frame: String, cx: &mut Context<Self>) {
        if let Some(WorldLayer {
            source: Source::Robot { anchor, .. },
            ..
        }) = self.layers.get_mut(index)
        {
            *anchor = frame;
            cx.notify();
        }
    }

    pub fn reset_view(&mut self, cx: &mut Context<Self>) {
        self.scene.update(cx, |scene, cx| scene.reset(cx));
    }

    /// The connection whose tree places a layer.
    ///
    /// A robot from the catalog belongs to no connection of its own — it is a
    /// file on disk — so it is placed by the tree of whatever system this world
    /// is already showing. Without that a catalog robot could never be attached
    /// to a running robot at all, which is the entire point of putting one in a
    /// world rather than in a viewer of its own.
    fn tree_for(&self, index: usize, cx: &App) -> Option<Buffer> {
        let connection = self
            .layers
            .get(index)
            .and_then(WorldLayer::connection)
            .or_else(|| self.layers.iter().find_map(WorldLayer::connection));
        self.tree(connection, cx)
    }

    /// The transform tree of a connection, copied out from under its lock.
    fn tree(&self, connection: Option<i64>, cx: &App) -> Option<Buffer> {
        let held = RobotWhisperer::global(cx).tf.read(cx).peek(connection?)?;
        let tree = held.lock().ok()?;
        Some(tree.clone())
    }

    /// Every frame the pane could be fixed to.
    ///
    /// The union of what the trees of the connections in this world know, plus
    /// the frames its layers have actually arrived in — so a topic in a frame
    /// nothing has published a transform for is still selectable, and choosing
    /// it is how you look at that one topic on its own.
    fn frames(&self, cx: &App) -> Vec<String> {
        let mut frames = BTreeSet::new();
        for connection in self
            .layers
            .iter()
            .filter_map(WorldLayer::connection)
            .collect::<BTreeSet<_>>()
        {
            if let Some(tree) = self.tree(Some(connection), cx) {
                frames.extend(tree.frames());
            }
        }
        for layer in &self.layers {
            if let Some(frame) = &layer.frame {
                frames.insert(frame.to_string());
            }
            if let Source::Robot { anchor, .. } = &layer.source
                && !anchor.is_empty()
            {
                frames.insert(anchor.clone());
            }
        }
        frames.into_iter().collect()
    }

    /// Picks a fixed frame when there is not one yet.
    ///
    /// The root of the tree, which is what a person means by "the world" — and
    /// with no tree at all, whatever the first layer arrived in, so an offline
    /// robot is drawn rather than dimmed for want of a transform that could
    /// never come.
    fn settle_fixed(&mut self, cx: &App) {
        if !self.fixed.is_empty() {
            return;
        }
        let from_tree = self
            .layers
            .iter()
            .filter_map(WorldLayer::connection)
            .filter_map(|connection| self.tree(Some(connection), cx))
            .find_map(|tree| {
                tree.tree()
                    .into_iter()
                    .find(|node| node.parent.is_none())
                    .map(|node| node.frame)
            });
        if let Some(root) = from_tree {
            self.fixed = root;
            return;
        }
        self.fixed = self
            .layers
            .iter()
            .find_map(|layer| match (&layer.frame, &layer.source) {
                (Some(frame), _) => Some(frame.to_string()),
                (None, Source::Robot { anchor, .. }) if !anchor.is_empty() => Some(anchor.clone()),
                _ => None,
            })
            .unwrap_or_default();
    }

    /// Rebuilds the scene from every layer, resolved into the fixed frame.
    fn sync(&mut self, cx: &mut Context<Self>) {
        self.settle_fixed(cx);
        let fixed = self.fixed.clone();
        let mut drawn: Vec<Layer> = Vec::new();

        for index in 0..self.layers.len() {
            let tree = self.tree_for(index, cx);
            let (layers, frame, problem) = match &self.layers[index].source {
                Source::Topic { .. } => self.topic_layers(index, &fixed, tree.as_ref()),
                Source::Robot { .. } => self.robot_layers(index, &fixed, tree.as_ref()),
            };
            let shown = self.layers[index].shown;
            let layer = &mut self.layers[index];
            layer.frame = frame;
            layer.problem = problem;
            drawn.extend(layers.into_iter().map(|mut layer| {
                layer.visible = layer.visible && shown;
                layer
            }));
        }

        self.scene
            .update(cx, |scene, cx| scene.set_layers(drawn, cx));
    }

    /// A topic layer: decode by role, place by frame.
    #[allow(clippy::type_complexity)]
    fn topic_layers(
        &self,
        index: usize,
        fixed: &str,
        tree: Option<&Buffer>,
    ) -> (Vec<Layer>, Option<SharedString>, Option<SharedString>) {
        let (value, schema) = {
            let Ok(incoming) = self.layers[index].incoming.lock() else {
                return (Vec::new(), None, None);
            };
            (incoming.value.clone(), incoming.schema.clone())
        };
        let (Some(value), Some(schema)) = (value, schema) else {
            return (
                Vec::new(),
                None,
                self.layers[index]
                    .problem
                    .clone()
                    .or(Some("Waiting for the first message…".into())),
            );
        };
        let role = viz::role_for(&schema);
        let Some(pieces) = viz::draw(&role, &value) else {
            return (
                Vec::new(),
                None,
                Some(format!("`{schema}` has nothing to draw in 3D.").into()),
            );
        };

        let placed = viz::place(pieces, fixed, tree);
        let frame = placed
            .iter()
            .find_map(|placed| placed.frame.clone())
            .map(SharedString::from);
        let problem = placed
            .iter()
            .find_map(|placed| placed.problem.clone())
            .map(SharedString::from);
        (
            placed.into_iter().map(|placed| placed.layer).collect(),
            frame,
            problem,
        )
    }

    /// A robot layer: every link in its own frame where the tree has one, and
    /// otherwise posed by the description and hung off the robot's anchor.
    ///
    /// Both halves matter. A running robot publishes a transform per link and
    /// the tree is then the only honest answer — the description's rest pose is
    /// a guess about a machine that is moving. A catalog robot with no system
    /// behind it has only the description, and refusing to draw it would make
    /// the pane useless offline.
    #[allow(clippy::type_complexity)]
    fn robot_layers(
        &self,
        index: usize,
        fixed: &str,
        tree: Option<&Buffer>,
    ) -> (Vec<Layer>, Option<SharedString>, Option<SharedString>) {
        let Source::Robot { anchor, loaded, .. } = &self.layers[index].source else {
            return (Vec::new(), None, None);
        };
        let Some(robot) = loaded else {
            return (Vec::new(), None, self.layers[index].problem.clone());
        };

        // Where the robot's root sits in the fixed frame, if anything can say.
        let (base, mut problem) = if anchor == fixed || anchor.is_empty() {
            (Some(rw_render::IDENTITY), None)
        } else {
            match tree.map(|tree| tree.lookup(fixed, anchor, rw_tf::LATEST)) {
                Some(Ok(placed)) => (Some(placed.to_mat4()), None),
                Some(Err(error)) => (None, Some(SharedString::from(error.to_string()))),
                None => (
                    None,
                    Some(SharedString::from(format!(
                        "`{anchor}` cannot be placed in `{fixed}`: no transforms have \
                         arrived for this system yet"
                    ))),
                ),
            }
        };

        let mut solids = Vec::with_capacity(robot.links.len());
        let mut from_tree = 0usize;
        for link in &robot.links {
            let placed = tree
                .and_then(|tree| tree.lookup(fixed, &link.frame, rw_tf::LATEST).ok())
                .inspect(|_| from_tree += 1)
                .map(|placed| placed.to_mat4())
                .or_else(|| base.map(|base| math::multiply(base, link.local)));
            let Some(transform) = placed else { continue };
            solids.push(Solid {
                key: link.key,
                vertices: Arc::clone(&link.vertices),
                transform,
            });
        }

        // Every link came from the tree, so the anchor was never needed and its
        // failure is not worth reporting.
        if from_tree == robot.links.len() && !robot.links.is_empty() {
            problem = None;
        }

        let mut layer = Layer::new(Content::Solids(solids));
        if problem.is_some() {
            layer.visible = false;
        }
        (
            vec![layer],
            (!anchor.is_empty()).then(|| SharedString::from(anchor.clone())),
            problem,
        )
    }

    /// How many topics across every connected system have geometry in them.
    ///
    /// A count rather than the list: the `+` menu offers the searchable picker
    /// and the number is what tells a person whether it is worth opening.
    fn drawable_topics(&self, cx: &App) -> usize {
        let workspace = self.workspace.read(cx);
        let sessions = self.sessions.read(cx);
        workspace
            .connections()
            .iter()
            .filter_map(|connection| sessions.discovery(connection.id))
            .flat_map(|discovery| discovery.topics.iter())
            .filter(|topic| viz::is_drawable(&topic.schema_name))
            .count()
    }

    /// The fixed frame row, and one row per layer.
    fn rail(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let pane = cx.entity_id().as_u64();
        let frames = self.frames(cx);
        let fixed = self.fixed.clone();
        // Never the words "Fixed frame" again: the label beside it already
        // says that, and a button repeating its own label is a row that costs
        // height and carries nothing.
        let shown_fixed = if fixed.is_empty() {
            SharedString::from("None")
        } else {
            SharedString::from(fixed.clone())
        };
        // Built before the card, because each row needs the same `cx` the card
        // builder is holding.
        let rows: Vec<gpui::AnyElement> = (0..self.layers.len())
            .map(|index| self.row(index, cx))
            .collect();

        tokens::card(cx)
            .id("layers")
            .flex_shrink_0()
            .w(px(RAIL))
            .min_h_0()
            .p_3()
            .gap_2()
            .overflow_y_scroll()
            .child(
                // The most important control in the pane, and the only one that
                // earns a permanent place: everything below is drawn relative
                // to it, so which frame it is *is* information.
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_1()
                    .child(tokens::section_label("Fixed frame", cx))
                    .child(
                        Button::new("fixed-frame")
                            .ghost()
                            .xsmall()
                            .label(shown_fixed)
                            .dropdown_menu(move |mut menu, _window, _cx| {
                                if frames.is_empty() {
                                    return menu.menu(
                                        "No frames yet",
                                        Box::new(crate::actions::ManageConnections),
                                    );
                                }
                                for frame in frames.clone() {
                                    let chosen = frame == fixed;
                                    menu = menu.menu_with_check(
                                        SharedString::from(frame.clone()),
                                        chosen,
                                        Box::new(SetWorldFrame {
                                            pane,
                                            frame: frame.into(),
                                        }),
                                    );
                                }
                                menu
                            }),
                    )
                    .child(self.add_button(cx)),
            )
            .children(rows)
            .when(self.layers.is_empty(), |rail| {
                rail.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Nothing in the world yet. Add a topic or a robot with +."),
                )
            })
    }

    /// One layer: what it is, which frame it came in, and what is wrong with it.
    fn row(&self, index: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(layer) = self.layers.get(index) else {
            return div().into_any_element();
        };
        let dot = if layer.problem.is_some() {
            cx.theme().danger
        } else if layer.shown {
            cx.theme().primary
        } else {
            cx.theme().muted_foreground
        };
        // A layer that cannot be placed is dimmed rather than drawn: the reason
        // sits under its name, and the picture does not lie about where it is.
        let dimmed = layer.problem.is_some() || !layer.shown;

        v_flex()
            .id(("layer", index))
            .w_full()
            .gap_0p5()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_1p5()
                    .child(
                        div()
                            .id(("toggle", index))
                            .cursor_pointer()
                            .child(tokens::status_dot(dot))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.toggle(index, cx)
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .truncate()
                            .text_color(if dimmed {
                                cx.theme().muted_foreground
                            } else {
                                cx.theme().foreground
                            })
                            .child(layer.name.clone()),
                    )
                    .when_some(layer.frame.clone(), |row, frame| {
                        row.child(
                            tokens::mono(cx)
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(frame),
                        )
                    })
                    .child(
                        Button::new(("remove", index))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.remove(index, cx)
                            })),
                    ),
            )
            .when_some(layer.problem.clone(), |row, problem| {
                row.child(
                    div()
                        .w_full()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(problem),
                )
            })
            .into_any_element()
    }

    /// The one button that adds to the world: every drawable topic and every
    /// robot, flat.
    ///
    /// On the rail's own header row rather than the tab strip, because the
    /// dock's toolbar takes plain buttons and a `+` that cannot ask *what* to
    /// add is a button with no meaning. The row was already there to say which
    /// fixed frame is in force, so this costs no height at all.
    fn add_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let pane = cx.entity_id().as_u64();
        let drawable = self.drawable_topics(cx);
        let robots: Vec<(String, SharedString)> = self
            .catalog
            .as_ref()
            .map(Catalog::entries)
            .unwrap_or(&[])
            .iter()
            .map(|entry| (entry.id.clone(), SharedString::from(entry.name.clone())))
            .collect();

        Button::new("add-layer")
            .ghost()
            .xsmall()
            .icon(IconName::Plus)
            .tooltip("Add to the world")
            // Flat, as everything here is: a submenu per system would be two
            // clicks and a hunt for something that should be one click and a
            // read.
            .dropdown_menu(move |mut menu, _window, _cx| {
                if drawable == 0 && robots.is_empty() {
                    return menu.menu(
                        "Connect a system first",
                        Box::new(crate::actions::ManageConnections),
                    );
                }
                // One searchable entry for the topics rather than all of them:
                // a robot publishing three hundred is the case this has to
                // survive, and the palette already ranks and takes the
                // keyboard. The robots are a handful and stay listed.
                if drawable > 0 {
                    menu = menu.menu(
                        SharedString::from(format!("Add a topic…  ({drawable} drawable)")),
                        Box::new(crate::actions::PickWorldTopic { pane }),
                    );
                }
                if !robots.is_empty() {
                    menu = menu.separator();
                    for (id, name) in robots.clone() {
                        menu = menu.menu(
                            name,
                            Box::new(AddWorldRobot {
                                pane,
                                robot: id.into(),
                            }),
                        );
                    }
                }
                menu
            })
    }
}

/// Flattens a link's parts into the renderer's vertex format.
fn vertices(parts: &[rw_assets::mesh::Part]) -> Vec<MeshVertex> {
    let mut vertices = Vec::new();
    for part in parts {
        // A description that names no colour gets the neutral grey most robot
        // viewers use, rather than black.
        let color = part.color.unwrap_or([0.72, 0.73, 0.76, 1.]);
        for index in &part.indices {
            let index = *index as usize;
            let (Some(position), Some(normal)) =
                (part.positions.get(index), part.normals.get(index))
            else {
                continue;
            };
            vertices.push(MeshVertex::new(*position, *normal, color));
        }
    }
    vertices
}

impl Focusable for WorldPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for WorldPanel {
    fn panel_name(&self) -> &'static str {
        "World"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        "World"
    }

    /// The fixed frame, beside the tab's title.
    ///
    /// Small and dim: the one thing about this pane worth knowing without
    /// looking at it, and the reason nothing in the body repeats the word
    /// "World".
    fn title_suffix(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        (!self.fixed.is_empty()).then(|| {
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(self.fixed.clone()))
                .into_any_element()
        })
    }

    /// The handful of controls that matter, flat.
    ///
    /// A robot layer's anchor frame is here because it is the one setting a
    /// description cannot supply: the file says `panda_link0` and the running
    /// robot publishes `base_link`, and without a way to say so the model hangs
    /// off nothing.
    fn dropdown_menu(
        &mut self,
        mut menu: gpui_component::menu::PopupMenu,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_component::menu::PopupMenu {
        let pane = cx.entity_id().as_u64();
        let frames = self.frames(cx);
        for (index, layer) in self.layers.iter().enumerate() {
            if let Source::Robot { anchor, .. } = &layer.source {
                for frame in &frames {
                    menu = menu.menu_with_check(
                        SharedString::from(format!("{}  on  {frame}", layer.name)),
                        frame == anchor,
                        Box::new(crate::actions::SetWorldAnchor {
                            pane,
                            layer: index as u64,
                            frame: frame.clone().into(),
                        }),
                    );
                }
                menu = menu.separator();
            }
        }
        for (index, layer) in self.layers.iter().enumerate() {
            menu = menu.menu(
                SharedString::from(format!("Remove {}", layer.name)),
                Box::new(RemoveWorldLayer {
                    pane,
                    layer: index as u64,
                }),
            );
        }
        if !self.layers.is_empty() {
            menu = menu.separator();
        }
        menu.menu(
            "Reset view",
            Box::new(crate::actions::ResetWorldView { pane }),
        )
    }

    fn on_added_to(
        &mut self,
        tab_panel: gpui::WeakEntity<gpui_component::dock::TabPanel>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.home.moved_to(tab_panel);
    }
}

impl Render for WorldPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync(cx);

        // No header strip: the tab above says "World" and its suffix says which
        // frame, so a row here would repeat both and spend height doing it.
        v_flex()
            .id("world")
            .size_full()
            .min_h_0()
            .track_focus(&self.focus_handle)
            .bg(cx.theme().background)
            .when_some(self.problem.clone(), |pane, problem| {
                pane.child(
                    div()
                        .px_3()
                        .pt_2()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(problem),
                )
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .p_2()
                    .gap_2()
                    .child(self.rail(cx))
                    .child(
                        tokens::card(cx)
                            .flex_1()
                            .min_w_0()
                            .child(self.scene.clone()),
                    ),
            )
    }
}
