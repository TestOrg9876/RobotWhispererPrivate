//! The robot pane: a model from the catalog, and a slider per joint.
//!
//! Loading is slow enough to matter — one of the shipped hands is thirty
//! megabytes of mesh — so it happens on a background thread and the pane says
//! it is working. Once loaded, moving a joint re-solves the kinematics and
//! hands the renderer a new set of matrices; the geometry itself is uploaded
//! once and never sent again.

use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::{
    ActiveTheme as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use rw_assets::catalog::{Catalog, Entry, Loaded};
use rw_assets::kinematics::{self, Pose};
use rw_assets::math;
use rw_assets::mesh::Part;
use rw_render::{Content, Layer, MeshVertex, Solid};

use crate::scene_view::SceneView;
use crate::tokens;

/// One link's geometry, uploaded once and placed by a matrix every frame.
struct Piece {
    link: String,
    key: u64,
    vertices: Arc<Vec<MeshVertex>>,
}

/// A joint the user can move, and the slider that moves it.
struct Control {
    name: String,
    label: SharedString,
    /// Radians for a revolute joint, metres for a prismatic one.
    unit: &'static str,
    slider: Entity<SliderState>,
}

pub struct RobotPanel {
    focus_handle: FocusHandle,
    scene: Entity<SceneView>,
    /// `None` when the assets directory could not be found at all.
    catalog: Option<Catalog>,
    problem: Option<SharedString>,
    /// Which robot is showing, and what it is made of.
    showing: Option<String>,
    robot: Option<rw_assets::urdf::Robot>,
    /// The correction from the catalog that puts this model the right way up.
    correction: math::Mat4,
    pieces: Vec<Piece>,
    pose: Pose,
    controls: Vec<Control>,
    loading: Option<String>,
    /// The tab group this pane is in, so the shell can bring it to the front
    /// wherever the user has since dragged it.
    home: crate::docking::Home,
    /// Bumped per load so a robot swapped in never reuses a retired key.
    generation: u64,
    _load: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for RobotPanel {}

impl RobotPanel {
    pub fn view(cx: &mut App) -> Entity<Self> {
        let scene = SceneView::view(cx);
        let catalog = match Catalog::discover() {
            Ok(catalog) => Some(catalog),
            Err(error) => {
                tracing::warn!("no robot catalog: {error}");
                None
            }
        };
        let problem = catalog.is_none().then(|| {
            SharedString::from(
                "The robot models could not be found. Set RW_ASSETS to the assets directory.",
            )
        });

        cx.new(|cx| {
            let mut panel = Self {
                focus_handle: cx.focus_handle(),
                scene,
                catalog,
                problem,
                showing: None,
                robot: None,
                correction: math::IDENTITY,
                pieces: Vec::new(),
                pose: Pose::default(),
                controls: Vec::new(),
                loading: None,
                home: Default::default(),
                generation: 0,
                _load: None,
                _subscriptions: Vec::new(),
            };
            // Something to look at on first open rather than an empty stage.
            if let Some(first) = panel
                .catalog
                .as_ref()
                .and_then(|catalog| catalog.entries().first())
                .map(|entry| entry.id.clone())
            {
                panel.show(&first, cx);
            }
            panel
        })
    }

    /// The tab group this pane is in.
    pub fn home(&self) -> Option<gpui::WeakEntity<gpui_component::dock::TabPanel>> {
        self.home.tab_panel()
    }

    fn entries(&self) -> &[Entry] {
        self.catalog.as_ref().map(Catalog::entries).unwrap_or(&[])
    }

    /// Starts loading a robot. Returns immediately; the pane fills in later.
    fn show(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(catalog) = self.catalog.clone() else {
            return;
        };
        if self.loading.as_deref() == Some(id) {
            return;
        }
        self.loading = Some(id.to_string());
        self.problem = None;
        cx.notify();

        let id = id.to_string();
        self._load = Some(cx.spawn(async move |panel, cx| {
            // Reading and parsing thirty megabytes of mesh must not be done on
            // the thread that draws.
            let loaded = cx
                .background_spawn(
                    async move { catalog.load(&id).map_err(|error| error.to_string()) },
                )
                .await;
            panel
                .update(cx, |panel, cx| match loaded {
                    Ok(loaded) => panel.adopt(loaded, cx),
                    Err(reason) => {
                        panel.loading = None;
                        panel.problem = Some(reason.into());
                        cx.notify();
                    }
                })
                .ok();
        }));
    }

    /// Takes on a freshly loaded robot: builds its geometry, its sliders, and
    /// frames the camera on it.
    fn adopt(&mut self, loaded: Loaded, cx: &mut Context<Self>) {
        // The renderer caches geometry by key and cannot know when a robot is
        // put away, so the outgoing one is released here.
        let retired: Vec<u64> = self.pieces.iter().map(|piece| piece.key).collect();
        if !retired.is_empty() {
            self.scene.read(cx).forget(&retired, cx);
        }

        self.generation += 1;
        let generation = self.generation;
        self.pieces = loaded
            .meshes
            .iter()
            .enumerate()
            .map(|(index, (link, parts))| Piece {
                link: link.clone(),
                key: generation << 32 | index as u64,
                vertices: Arc::new(vertices(parts)),
            })
            .collect();

        self.controls = loaded
            .robot
            .movable()
            .map(|joint| {
                let (lower, upper) = joint
                    .limits
                    .unwrap_or((-std::f32::consts::PI, std::f32::consts::PI));
                let state = cx.new(|_| {
                    SliderState::new()
                        .min(lower.min(upper))
                        .max(upper.max(lower))
                        // Fine enough to pose an arm, coarse enough that a drag
                        // does not re-solve on every pixel.
                        .step((upper - lower).abs() / 200.)
                        .default_value(joint.rest())
                });
                let name = joint.name.clone();
                self._subscriptions.push(cx.subscribe(
                    &state,
                    move |panel, _, event: &SliderEvent, cx| {
                        let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
                        panel.pose.set(&name, value.start());
                        panel.repose(cx);
                    },
                ));
                Control {
                    name: joint.name.clone(),
                    label: joint.name.clone().into(),
                    unit: match joint.kind {
                        rw_assets::urdf::JointKind::Prismatic => "m",
                        _ => "rad",
                    },
                    slider: state,
                }
            })
            .collect();

        self.pose = Pose::rest(&loaded.robot);
        self.correction = loaded.entry.correction();
        self.showing = Some(loaded.entry.id.clone());
        self.loading = None;
        self.robot = Some(loaded.robot);
        self.repose(cx);
        // A new robot gets a fresh view: the angle that suited a hand is not
        // the angle that suits a two-metre arm.
        self.scene.update(cx, |scene, cx| scene.reset(cx));
        cx.notify();
    }

    /// Re-solves the kinematics and hands the renderer new matrices.
    fn repose(&mut self, cx: &mut Context<Self>) {
        let Some(robot) = &self.robot else { return };
        let placed = kinematics::solve(robot, &self.pose);
        let solids: Vec<Solid> = self
            .pieces
            .iter()
            .filter_map(|piece| {
                let link = placed.get(&piece.link)?;
                Some(Solid {
                    key: piece.key,
                    vertices: Arc::clone(&piece.vertices),
                    transform: math::multiply(self.correction, *link),
                })
            })
            .collect();
        self.scene.update(cx, |scene, cx| {
            scene.set_layers(vec![Layer::new(Content::Solids(solids))], cx)
        });
        cx.notify();
    }

    /// Puts every joint back where it rests.
    fn reset_pose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(robot) = &self.robot else { return };
        self.pose = Pose::rest(robot);
        for control in &self.controls {
            let value = self.pose.get(&control.name);
            control
                .slider
                .update(cx, |slider, cx| slider.set_value(value, window, cx));
        }
        self.repose(cx);
    }
}

/// Flattens a link's parts into the renderer's vertex format.
///
/// Indices are expanded rather than kept: the pipeline draws unindexed, and a
/// robot link is thousands of triangles rather than millions.
fn vertices(parts: &[Part]) -> Vec<MeshVertex> {
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

impl Focusable for RobotPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for RobotPanel {
    fn panel_name(&self) -> &'static str {
        "Robot"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        "Robot"
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

impl Render for RobotPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let showing = self.showing.clone();
        let loading = self.loading.clone();
        let entries: Vec<(String, SharedString)> = self
            .entries()
            .iter()
            .map(|entry| (entry.id.clone(), entry.name.clone().into()))
            .collect();

        v_flex()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .flex_shrink_0()
                    .h(px(tokens::CARD_HEADER_HEIGHT))
                    .items_center()
                    .gap_1()
                    .px_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(h_flex().gap_1().flex_1().min_w_0().flex_wrap().children(
                        entries.into_iter().map(|(id, name)| {
                            let selected = showing.as_deref() == Some(id.as_str());
                            let busy = loading.as_deref() == Some(id.as_str());
                            Button::new(SharedString::from(id.clone()))
                                .ghost()
                                .xsmall()
                                .label(name)
                                .selected(selected)
                                .loading(busy)
                                .on_click(cx.listener(move |this, _, _, cx| this.show(&id, cx)))
                        }),
                    ))
                    .child(
                        Button::new("reset-pose")
                            .ghost()
                            .xsmall()
                            .label("Reset pose")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.reset_pose(window, cx)),
                            ),
                    ),
            )
            .when_some(self.problem.clone(), |pane, problem| {
                pane.child(
                    div()
                        .p_3()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(problem),
                )
            })
            .child(
                // Both halves are cards on the panel's ground, the same
                // material a request's response sits on. Bare content beside a
                // hairline divider was what made this pane look unlike the rest
                // of the app.
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .p_2()
                    .gap_2()
                    .child(
                        tokens::card(cx)
                            .flex_1()
                            .min_w_0()
                            .child(self.scene.clone()),
                    )
                    .child(
                        tokens::card(cx)
                            .id("joints")
                            .flex_shrink_0()
                            .w(px(240.))
                            .min_h_0()
                            .p_3()
                            .gap_3()
                            .overflow_y_scroll()
                            .child(tokens::section_label("Joints", cx))
                            .children(self.controls.iter().map(|control| {
                                let value = self.pose.get(&control.name);
                                v_flex()
                                    .gap_1()
                                    .child(
                                        h_flex()
                                            .justify_between()
                                            .items_baseline()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().foreground)
                                                    .truncate()
                                                    .child(control.label.clone()),
                                            )
                                            .child(
                                                tokens::mono(cx)
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(format!("{value:.3} {}", control.unit)),
                                            ),
                                    )
                                    .child(Slider::new(&control.slider))
                            }))
                            .when(self.controls.is_empty(), |pane| {
                                pane.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Nothing to move."),
                                )
                            }),
                    ),
            )
    }
}
