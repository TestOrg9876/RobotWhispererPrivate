//! A pane that draws a 3D scene, and the mouse handling that goes with it.
//!
//! GPUI cannot be handed a live GPU surface portably, so each frame is rendered
//! offscreen by `rw-render`, read back, and painted as an image. That has one
//! sharp edge: `RenderImage::new` allocates a fresh id, so a new image every
//! frame means a new sprite-atlas entry every frame. Every swap here is paired
//! with `Window::drop_image`, which is the only thing standing between this
//! pane and an atlas that grows at frame rate.

use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Bounds, Context, Corners, Entity, InteractiveElement as _, IntoElement,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point, Render,
    RenderImage, ScrollWheelEvent, Styled as _, Window, canvas, div,
};
use gpui_component::{ActiveTheme as _, IconName, h_flex, v_flex};
use image_crate::{Frame, RgbaImage};
use rw_render::{Camera, Coloring, Grid, Points, Scene, Solid};

use crate::gpu::Gpu;
use crate::session::RobotWhisperer;
use crate::tokens;

/// How far the camera turns per pixel dragged. A full window's width is a bit
/// more than half a turn, which is what feels like "grabbing" the scene.
const ORBIT_PER_PIXEL: f32 = 0.008;
/// How much one wheel click closes the distance.
const ZOOM_PER_CLICK: f32 = 0.12;

pub struct SceneView {
    gpu: Entity<Gpu>,
    scene: Scene,
    /// The image currently in the sprite atlas, kept so it can be released when
    /// the next frame replaces it.
    shown: Option<Arc<RenderImage>>,
    /// The size the current image was rendered at, in device pixels.
    rendered: (u32, u32),
    /// Set whenever the scene changes, so an idle pane costs nothing.
    dirty: bool,
    /// Whether the camera has been pointed at the data yet. Done once, on the
    /// first cloud: re-framing on every message would fight the user's drag.
    framed: bool,
    dragging: Option<Point<Pixels>>,
}

impl SceneView {
    pub fn view(cx: &mut App) -> Entity<Self> {
        let gpu = RobotWhisperer::global(cx).gpu.clone();
        cx.new(|cx| {
            cx.observe(&gpu, |_, _, cx| cx.notify()).detach();
            Self {
                gpu,
                scene: Scene::default(),
                shown: None,
                rendered: (0, 0),
                dirty: true,
                framed: false,
                dragging: None,
            }
        })
    }

    /// Hands the pane a new cloud. Frames the camera on the first one.
    pub fn show(&mut self, points: Points, cx: &mut Context<Self>) {
        // The colouring is the user's choice, so it survives the new data — as
        // long as the new data still offers it.
        let wanted = self.scene.points.coloring;
        self.scene.points = points;
        if self.scene.points.available().contains(&wanted) {
            self.scene.points.coloring = wanted;
        }
        if !self.framed
            && let Some((min, max)) = bounds_of(&self.scene.points)
        {
            self.frame(min, max, cx);
        }
        self.dirty = true;
        cx.notify();
    }

    /// Replaces the lit surfaces the pane draws.
    pub fn set_solids(&mut self, solids: Vec<Solid>, cx: &mut Context<Self>) {
        self.scene.solids = solids;
        self.dirty = true;
        cx.notify();
    }

    /// Points the camera at a box, and sizes the ground grid to match.
    ///
    /// Used when a robot finishes loading or a cloud first arrives; re-framing
    /// on every message would take the view away from the user.
    pub fn frame(&mut self, min: [f32; 3], max: [f32; 3], cx: &mut Context<Self>) {
        self.scene.camera.frame(min, max);
        let widest = (0..3)
            .map(|axis| max[axis] - min[axis])
            .fold(0f32, f32::max);
        self.scene.grid = Some(Grid::for_size(widest));
        self.framed = true;
        self.dirty = true;
        cx.notify();
    }

    /// Releases geometry the pane will not draw again.
    pub fn forget(&self, keys: &[u64], cx: &App) {
        if let Some(renderer) = self.gpu.read(cx).renderer() {
            renderer.forget(keys);
        }
    }

    pub fn coloring(&self) -> Coloring {
        self.scene.points.coloring
    }

    pub fn available(&self) -> Vec<Coloring> {
        self.scene.points.available()
    }

    pub fn set_coloring(&mut self, coloring: Coloring, cx: &mut Context<Self>) {
        self.scene.points.coloring = coloring;
        self.dirty = true;
        cx.notify();
    }

    /// Points the camera back at whatever the pane is showing.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.scene.camera = Camera::default();
        if let Some((min, max)) =
            bounds_of(&self.scene.points).or_else(|| solid_bounds(&self.scene))
        {
            // Through `frame`, so the ground grid is resized too: a grid left at
            // its ten-metre default with the camera a metre away puts lines
            // behind the near plane, which rasterise as wedges across the pane.
            self.frame(min, max, cx);
        }
        self.dirty = true;
        cx.notify();
    }

    pub fn point_count(&self) -> usize {
        self.scene.points.positions.len()
    }

    fn orbit(&mut self, delta: Point<Pixels>, cx: &mut Context<Self>) {
        self.scene.camera.orbit(
            -f32::from(delta.x) * ORBIT_PER_PIXEL,
            f32::from(delta.y) * ORBIT_PER_PIXEL,
        );
        self.dirty = true;
        cx.notify();
    }

    fn zoom(&mut self, clicks: f32, cx: &mut Context<Self>) {
        self.scene
            .camera
            .zoom((1. - clicks * ZOOM_PER_CLICK).clamp(0.2, 5.));
        self.dirty = true;
        cx.notify();
    }

    /// Draws the scene into the atlas and paints it, if anything has changed.
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let scale = window.scale_factor();
        let size = (
            (f32::from(bounds.size.width) * scale).round().max(0.) as u32,
            (f32::from(bounds.size.height) * scale).round().max(0.) as u32,
        );

        // The handle is taken out first: `read` borrows the app context, and the
        // rest of this borrows `self` mutably.
        let renderer = self.gpu.read(cx).renderer();
        if (self.dirty || size != self.rendered)
            && let Some(renderer) = renderer
            && let Some(frame) = renderer.render(&self.scene, size.0, size.1)
        {
            // GPUI's atlas is BGRA; the renderer produces the RGBA every other
            // consumer would expect, so the swap happens here.
            let mut pixels = frame.rgba;
            for pixel in pixels.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            if let Some(image) = RgbaImage::from_raw(frame.width, frame.height, pixels) {
                if let Some(previous) = self.shown.take() {
                    window.drop_image(previous).ok();
                }
                self.shown = Some(Arc::new(RenderImage::new(vec![Frame::new(image)])));
                self.rendered = (frame.width, frame.height);
                self.dirty = false;
            }
        }

        if let Some(image) = self.shown.clone() {
            window
                .paint_image(bounds, bounds, Corners::default(), image, 0, false)
                .ok();
        }
    }
}

/// The box the pane's lit surfaces occupy, in world space.
fn solid_bounds(scene: &Scene) -> Option<([f32; 3], [f32; 3])> {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    let mut any = false;
    for solid in &scene.solids {
        for vertex in solid.vertices.iter() {
            any = true;
            let point = rw_render::transform_point(solid.transform, vertex.position);
            for axis in 0..3 {
                min[axis] = min[axis].min(point[axis]);
                max[axis] = max[axis].max(point[axis]);
            }
        }
    }
    any.then_some((min, max))
}

fn bounds_of(points: &Points) -> Option<([f32; 3], [f32; 3])> {
    let first = *points.positions.first()?;
    let mut min = first;
    let mut max = first;
    for point in &points.positions {
        for axis in 0..3 {
            min[axis] = min[axis].min(point[axis]);
            max[axis] = max[axis].max(point[axis]);
        }
    }
    Some((min, max))
}

impl Render for SceneView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let gpu = self.gpu.read(cx);
        if gpu.renderer().is_none() {
            let (title, detail) = match gpu.error() {
                Some(reason) => ("No 3D on this machine", reason.to_string()),
                None => (
                    "Starting the renderer",
                    "Opening the graphics device.".to_string(),
                ),
            };
            return v_flex()
                .size_full()
                .child(tokens::empty_state(IconName::Globe, title, detail, cx))
                .into_any_element();
        }

        let view = cx.entity().downgrade();
        v_flex()
            .id("scene")
            .size_full()
            .min_h_0()
            .relative()
            .overflow_hidden()
            .rounded(cx.theme().radius)
            .cursor_crosshair()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, _| {
                    this.dragging = Some(event.position);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                let Some(last) = this.dragging else { return };
                this.dragging = Some(event.position);
                this.orbit(event.position - last, cx);
            }))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.dragging = None),
            )
            // Also on the way out: a drag released past the pane's edge would
            // otherwise leave the camera stuck to the pointer.
            .on_mouse_up_out(
                gpui::MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.dragging = None),
            )
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                let delta = event.delta.pixel_delta(window.line_height()).y;
                this.zoom(f32::from(delta) / 40., cx);
            }))
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        view.update(cx, |view, cx| view.paint(bounds, window, cx))
                            .ok();
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }
}

/// The strip of controls a host pane puts beside a scene.
pub fn controls(view: &Entity<SceneView>, cx: &App) -> impl IntoElement + use<> {
    let scene = view.read(cx);
    let active = scene.coloring();
    let available = scene.available();
    let count = scene.point_count();

    h_flex()
        .gap_2()
        .items_center()
        // A pane showing a robot has nothing to colour and no points to count,
        // so it gets the view controls and nothing else.
        .when(count > 0, |row| {
            row.child(
                h_flex()
                    .gap_1()
                    .children(available.into_iter().map(|coloring| {
                        let view = view.clone();
                        div()
                            .id(coloring.label())
                            .px_2()
                            .py_0p5()
                            .rounded(cx.theme().radius)
                            .text_xs()
                            .cursor_pointer()
                            .when(coloring == active, |this| {
                                this.bg(cx.theme().accent)
                                    .text_color(cx.theme().accent_foreground)
                            })
                            .when(coloring != active, |this| {
                                this.text_color(cx.theme().muted_foreground)
                            })
                            .child(coloring.label())
                            .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
                                view.update(cx, |view, cx| view.set_coloring(coloring, cx));
                            })
                    })),
            )
            .child(tokens::meta("Points", count.to_string(), cx))
        })
        .child({
            let view = view.clone();
            div()
                .id("reset-view")
                .px_2()
                .py_0p5()
                .rounded(cx.theme().radius)
                .text_xs()
                .cursor_pointer()
                .text_color(cx.theme().muted_foreground)
                .child("Reset view")
                .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
                    view.update(cx, |view, cx| view.reset(cx));
                })
        })
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("drag to orbit · scroll to zoom"),
        )
}
