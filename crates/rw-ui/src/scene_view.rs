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
use rw_render::{Camera, Coloring, Content, Grid, Layer, Scene};

use crate::gpu::Gpu;
use crate::session::RobotWhisperer;
use crate::tokens;

/// Text and backdrop for the chips floating over a scene.
///
/// Fixed rather than themed: the viewport is dark whichever theme is on, so
/// these are chosen against it and not against the window.
const OVERLAY_TEXT: gpui::Hsla = gpui::Hsla {
    h: 0.,
    s: 0.,
    l: 0.82,
    a: 1.,
};
const OVERLAY_BACKDROP: gpui::Hsla = gpui::Hsla {
    h: 0.,
    s: 0.,
    l: 0.,
    a: 0.35,
};

/// How far the camera turns per pixel dragged. A full window's width is a bit
/// more than half a turn, which is what feels like "grabbing" the scene.
const ORBIT_PER_PIXEL: f32 = 0.008;
/// How much one wheel click closes the distance.
const ZOOM_PER_CLICK: f32 = 0.12;

/// How the scene's corners are rounded.
///
/// The picture is painted by `Window::paint_image` rather than laid out as an
/// element, and paint takes its own corner radii: `overflow_hidden` on an
/// ancestor masks to a rectangle, so a square image inside a rounded card fills
/// the corners the card cut away. The radius therefore has to be told, and it
/// has to be the radius of whatever the scene is sitting in — one radius inside
/// a slightly larger one leaves a sliver of card showing at each corner, which
/// looks like a rendering fault rather than a design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rounding {
    /// Inset in a card, with padding around it: the small radius.
    #[default]
    Inset,
    /// Filling a card edge to edge: the card's own radius.
    FillsACard,
}

impl Rounding {
    fn radius(self, cx: &App) -> Pixels {
        match self {
            Rounding::Inset => cx.theme().radius,
            Rounding::FillsACard => cx.theme().radius_lg,
        }
    }
}

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
    /// How points are coloured. Kept here rather than read back off the scene
    /// so it survives every layer in it being replaced, which happens on each
    /// message.
    coloring: Coloring,
    dragging: Option<Point<Pixels>>,
    rounding: Rounding,
}

impl SceneView {
    /// The same pane, rounded to fill a card edge to edge rather than to sit
    /// inside one with padding around it.
    pub fn filling_a_card(cx: &mut App) -> Entity<Self> {
        let view = Self::view(cx);
        view.update(cx, |view, _| view.rounding = Rounding::FillsACard);
        view
    }

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
                rounding: Rounding::default(),
                coloring: Coloring::default(),
                dragging: None,
            }
        })
    }

    /// Replaces everything the pane draws.
    ///
    /// The whole list at once rather than one layer at a time: the caller knows
    /// which layers exist and which of them resolved this frame, and a partial
    /// update would leave a layer whose transform has just gone stale still
    /// drawn where it last was.
    pub fn set_layers(&mut self, mut layers: Vec<Layer>, cx: &mut Context<Self>) {
        // The colouring is the user's choice, so it survives the new data — as
        // long as the new data still offers it.
        for layer in &mut layers {
            if let Content::Points(points) = &mut layer.content {
                points.coloring = if points.available().contains(&self.coloring) {
                    self.coloring
                } else {
                    Coloring::default()
                };
            }
        }
        self.scene.layers = layers;
        if !self.framed
            && let Some((min, max)) = bounds_of(&self.scene)
        {
            self.frame(min, max, cx);
        }
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
        self.coloring
    }

    /// Every colouring some cloud in the scene can offer, in the order shown.
    pub fn available(&self) -> Vec<Coloring> {
        let mut available = Vec::new();
        for layer in &self.scene.layers {
            if let Content::Points(points) = &layer.content {
                for coloring in points.available() {
                    if !available.contains(&coloring) {
                        available.push(coloring);
                    }
                }
            }
        }
        available
    }

    pub fn set_coloring(&mut self, coloring: Coloring, cx: &mut Context<Self>) {
        self.coloring = coloring;
        for layer in &mut self.scene.layers {
            if let Content::Points(points) = &mut layer.content
                && points.available().contains(&coloring)
            {
                points.coloring = coloring;
            }
        }
        self.dirty = true;
        cx.notify();
    }

    /// Points the camera back at whatever the pane is showing.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.scene.camera = Camera::default();
        if let Some((min, max)) = bounds_of(&self.scene) {
            // Through `frame`, so the ground grid is resized too: a grid left at
            // its ten-metre default with the camera a metre away puts lines
            // behind the near plane, which rasterise as wedges across the pane.
            self.frame(min, max, cx);
        }
        self.dirty = true;
        cx.notify();
    }

    pub fn point_count(&self) -> usize {
        self.scene
            .layers
            .iter()
            .filter_map(|layer| match &layer.content {
                Content::Points(points) => Some(points.positions.len()),
                _ => None,
            })
            .sum()
    }

    /// Whether there is anything at all to look at.
    pub fn is_empty(&self) -> bool {
        self.scene
            .layers
            .iter()
            .all(|layer| !layer.visible || layer.content.is_empty())
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

    /// The chips floating in the corner of the picture.
    ///
    /// Colouring, when the data offers a choice, and a way back to the default
    /// view. No count and no "drag to orbit" hint — a line of instructions
    /// teaches once and then costs part of the picture forever.
    fn overlay(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let active = self.coloring;
        // One entry is not a choice, so it is not offered.
        let available = self.available();
        let choices = if available.len() > 1 {
            available
        } else {
            Vec::new()
        };
        // Styled for the scene's own dark ground rather than the theme's, since
        // a viewport is dark under every theme and muted grey on it is
        // unreadable.
        let chip = |label: &'static str, on: bool, cx: &App| {
            div()
                .id(label)
                .px_1p5()
                .rounded(cx.theme().radius)
                .text_xs()
                .cursor_pointer()
                .when(on, |this| {
                    this.bg(cx.theme().accent)
                        .text_color(cx.theme().accent_foreground)
                })
                .when(!on, |this| this.text_color(OVERLAY_TEXT))
                .child(label)
        };

        h_flex()
            .gap_1()
            .items_center()
            .px_1()
            .py_0p5()
            .rounded(cx.theme().radius)
            .bg(OVERLAY_BACKDROP)
            .children(
                choices
                    .into_iter()
                    .map(|coloring| {
                        chip(coloring.label(), coloring == active, cx).on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |view, _, _, cx| view.set_coloring(coloring, cx)),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
            .child(chip("Reset", false, cx).on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|view, _, _, cx| view.reset(cx)),
            ))
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
                .paint_image(
                    bounds,
                    bounds,
                    Corners::all(self.rounding.radius(cx)),
                    image,
                    0,
                    false,
                )
                .ok();
        }
    }
}

/// The box everything in the scene occupies, in the fixed frame.
///
/// Every layer's own transform is folded in, which is what makes framing
/// correct once TF is in play: a robot ten metres from a cloud has to be inside
/// the camera's box, not beside it.
fn bounds_of(scene: &Scene) -> Option<([f32; 3], [f32; 3])> {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    let mut any = false;
    let mut stretch = |point: [f32; 3]| {
        any = true;
        for axis in 0..3 {
            min[axis] = min[axis].min(point[axis]);
            max[axis] = max[axis].max(point[axis]);
        }
    };

    for layer in &scene.layers {
        if !layer.visible {
            continue;
        }
        let place = |point: [f32; 3]| rw_render::transform_point(layer.transform, point);
        match &layer.content {
            Content::Points(points) => {
                for point in &points.positions {
                    stretch(place(*point));
                }
            }
            Content::Solids(solids) => {
                for solid in solids {
                    for vertex in solid.vertices.iter() {
                        stretch(place(rw_render::transform_point(
                            solid.transform,
                            vertex.position,
                        )));
                    }
                }
            }
            Content::Lines(sets) => {
                for set in sets {
                    for point in &set.points {
                        stretch(place(*point));
                    }
                }
            }
            Content::Axes(axes) => {
                for axis in axes {
                    stretch(place(rw_render::transform_point(axis.transform, [0.; 3])));
                }
            }
        }
    }
    any.then_some((min, max))
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
            .rounded(self.rounding.radius(cx))
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
            // Over the picture rather than in a strip above it. A row of chips
            // costs the same height whether or not anyone is looking at them,
            // and every 3D viewer worth using floats its controls.
            .child(div().absolute().top_1().left_1().child(self.overlay(cx)))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rw_render::Points;

    fn cloud(positions: Vec<[f32; 3]>) -> Content {
        Content::Points(Points {
            positions,
            ..Points::default()
        })
    }

    #[test]
    fn a_cloud_with_one_colouring_offers_no_choice() {
        // The chips are only worth their pixels when there is something to
        // choose between; height is always available, so one entry is not a
        // choice.
        let points = Points {
            positions: vec![[0., 0., 0.]],
            ..Points::default()
        };
        assert_eq!(points.available().len(), 1);
    }

    #[test]
    fn framing_covers_every_layer_where_its_transform_puts_it() {
        // The regression this guards: before layers, a robot ten metres away
        // from a cloud framed on whichever of the two the pane happened to
        // hold, and the other was off screen.
        let mut here = Layer::new(cloud(vec![[0., 0., 0.]]));
        here.transform = rw_render::IDENTITY;
        let mut far = Layer::new(cloud(vec![[0., 0., 0.]]));
        far.transform[3] = [10., 0., 0., 1.];

        let scene = Scene {
            layers: vec![here, far],
            ..Scene::default()
        };
        assert_eq!(bounds_of(&scene), Some(([0., 0., 0.], [10., 0., 0.])));
    }

    #[test]
    fn a_hidden_layer_does_not_drag_the_camera_out_to_meet_it() {
        let mut hidden = Layer::new(cloud(vec![[1000., 0., 0.]]));
        hidden.visible = false;
        let scene = Scene {
            layers: vec![Layer::new(cloud(vec![[1., 1., 1.]])), hidden],
            ..Scene::default()
        };
        assert_eq!(bounds_of(&scene), Some(([1., 1., 1.], [1., 1., 1.])));
    }

    #[test]
    fn an_empty_scene_has_no_bounds_rather_than_a_box_at_infinity() {
        assert_eq!(bounds_of(&Scene::default()), None);
    }
}
