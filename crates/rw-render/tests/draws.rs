//! The one thing the unit tests cannot check: that a GPU actually draws this.
//!
//! Skipped rather than failed when there is no adapter, so a machine without
//! one still gets the rest of the suite. Everything up to the draw call is
//! covered by the unit tests in `camera` and `scene`.

use std::sync::Arc;

use rw_render::{
    Camera, Coloring, Content, Grid, Layer, MeshVertex, Points, Renderer, Scene, Solid,
};

fn renderer() -> Option<Renderer> {
    match pollster::block_on(Renderer::new()) {
        Ok(renderer) => {
            eprintln!("rendering on {}", renderer.adapter);
            Some(renderer)
        }
        Err(reason) => {
            eprintln!("skipped: {reason}");
            None
        }
    }
}

/// How many distinct colours the image holds, which is the cheapest way to tell
/// "something was drawn" from "the clear colour was read back".
fn colours(rgba: &[u8]) -> usize {
    let mut seen: Vec<[u8; 4]> = Vec::new();
    for pixel in rgba.chunks_exact(4) {
        let pixel: [u8; 4] = pixel.try_into().expect("four bytes");
        if !seen.contains(&pixel) {
            seen.push(pixel);
            if seen.len() > 64 {
                break;
            }
        }
    }
    seen.len()
}

#[test]
fn an_empty_scene_reads_back_as_its_background() {
    let Some(renderer) = renderer() else { return };
    let scene = Scene {
        grid: None,
        ..Scene::default()
    };
    let frame = renderer.render(&scene, 64, 48).expect("renders");
    assert_eq!(frame.width, 64);
    assert_eq!(frame.height, 48);
    assert_eq!(frame.rgba.len(), 64 * 48 * 4);
    assert_eq!(colours(&frame.rgba), 1, "nothing was drawn, so one colour");
    assert_eq!(frame.rgba[3], 255, "opaque");
}

#[test]
fn a_grid_puts_lines_on_the_screen() {
    let Some(renderer) = renderer() else { return };
    let scene = Scene {
        grid: Some(Grid::default()),
        ..Scene::default()
    };
    let frame = renderer.render(&scene, 320, 240).expect("renders");
    assert!(colours(&frame.rgba) > 1, "the grid did not draw");
}

#[test]
fn points_are_drawn_in_the_colour_the_cloud_asked_for() {
    let Some(renderer) = renderer() else { return };
    let mut camera = Camera::default();
    camera.frame([-1., -1., -1.], [1., 1., 1.]);
    let scene = Scene {
        camera,
        grid: None,
        // Large enough that a point cannot fall between sample positions.
        point_size: 40.,
        layers: vec![Layer::new(Content::Points(Points {
            positions: vec![[0., 0., 0.]],
            rgb: Some(vec![[255, 0, 0]]),
            coloring: Coloring::Rgb,
            ..Points::default()
        }))],
        ..Scene::default()
    };
    let frame = renderer.render(&scene, 200, 200).expect("renders");

    // The point is at the target, so it lands in the middle of the pane.
    let centre = ((100 * 200 + 100) * 4) as usize;
    let pixel = &frame.rgba[centre..centre + 4];
    assert!(
        pixel[0] > 200 && pixel[1] < 60 && pixel[2] < 60,
        "expected red at the centre, got {pixel:?}"
    );
}

#[test]
fn a_pane_with_no_area_renders_nothing_rather_than_failing() {
    let Some(renderer) = renderer() else { return };
    assert!(renderer.render(&Scene::default(), 0, 100).is_none());
    assert!(renderer.render(&Scene::default(), 100, 0).is_none());
}

#[test]
fn a_width_that_is_not_a_multiple_of_the_copy_alignment_still_unpads_correctly() {
    let Some(renderer) = renderer() else { return };
    // 61 × 4 = 244 bytes a row, padded to 256: every row is offset by 12 bytes
    // in the readback buffer, which is exactly what a naive copy gets wrong.
    let scene = Scene {
        grid: None,
        background: [1., 0., 0.],
        ..Scene::default()
    };
    let frame = renderer.render(&scene, 61, 7).expect("renders");
    assert_eq!(frame.rgba.len(), 61 * 7 * 4);
    for (index, pixel) in frame.rgba.chunks_exact(4).enumerate() {
        assert_eq!(
            pixel[3], 255,
            "pixel {index} was not opaque, so the rows are misaligned"
        );
    }
}

#[test]
fn a_lit_surface_is_drawn_and_shaded() {
    let Some(renderer) = renderer() else { return };
    // A quad filling the view, facing the camera, in flat mid-grey. If the
    // lighting works the pixels come back grey and not the clear colour; if the
    // instance matrix works it is where it was put.
    let colour = [0.6, 0.6, 0.6, 1.];
    let normal = [1., 0., 0.];
    let corners = [
        [0., -1., -1.],
        [0., 1., -1.],
        [0., 1., 1.],
        [0., -1., -1.],
        [0., 1., 1.],
        [0., -1., 1.],
    ];
    let solid = Solid {
        key: 1,
        vertices: Arc::new(
            corners
                .iter()
                .map(|corner| MeshVertex::new(*corner, normal, colour))
                .collect(),
        ),
        transform: rw_render::IDENTITY,
    };

    let camera = Camera {
        target: [0., 0., 0.],
        distance: 3.,
        yaw: 0.,
        pitch: 0.,
        ..Camera::default()
    };
    let scene = Scene {
        camera,
        grid: None,
        layers: vec![Layer::new(Content::Solids(vec![solid]))],
        background: [0., 0., 0.],
        ..Scene::default()
    };
    let frame = renderer.render(&scene, 120, 120).expect("renders");

    let centre = ((60 * 120 + 60) * 4) as usize;
    let pixel = &frame.rgba[centre..centre + 4];
    assert!(
        pixel[0] > 30 && pixel[0] < 250,
        "expected a shaded grey at the centre, got {pixel:?}"
    );
    assert_eq!(pixel[0], pixel[1], "a grey surface stayed grey");
    assert_eq!(pixel[3], 255);
}

#[test]
fn an_instance_transform_moves_the_geometry_it_places() {
    let Some(renderer) = renderer() else { return };
    let vertices = Arc::new(vec![
        MeshVertex::new([0., -0.2, -0.2], [1., 0., 0.], [1., 1., 1., 1.]),
        MeshVertex::new([0., 0.2, -0.2], [1., 0., 0.], [1., 1., 1., 1.]),
        MeshVertex::new([0., 0., 0.2], [1., 0., 0.], [1., 1., 1., 1.]),
    ]);
    let camera = Camera {
        target: [0., 0., 0.],
        distance: 3.,
        yaw: 0.,
        pitch: 0.,
        ..Camera::default()
    };
    let scene = |transform| Scene {
        camera,
        grid: None,
        layers: vec![Layer::new(Content::Solids(vec![Solid {
            key: 2,
            vertices: vertices.clone(),
            transform,
        }]))],
        background: [0., 0., 0.],
        ..Scene::default()
    };

    let centred = renderer
        .render(&scene(rw_render::IDENTITY), 120, 120)
        .expect("renders");
    // Pushed a long way along the camera's left, so it leaves the middle.
    let mut moved_matrix = rw_render::IDENTITY;
    moved_matrix[3] = [0., 5., 0., 1.];
    let moved = renderer
        .render(&scene(moved_matrix), 120, 120)
        .expect("renders");

    let centre = ((60 * 120 + 60) * 4) as usize;
    assert!(centred.rgba[centre] > 30, "nothing was drawn in the middle");
    assert_eq!(
        moved.rgba[centre], 0,
        "the instance matrix did not move the triangle"
    );
}

#[test]
fn geometry_is_uploaded_once_and_can_be_forgotten() {
    let Some(renderer) = renderer() else { return };
    let solid = Solid {
        key: 99,
        vertices: Arc::new(vec![
            MeshVertex::new([0., 0., 0.], [0., 0., 1.], [1., 1., 1., 1.]),
            MeshVertex::new([1., 0., 0.], [0., 0., 1.], [1., 1., 1., 1.]),
            MeshVertex::new([0., 1., 0.], [0., 0., 1.], [1., 1., 1., 1.]),
        ]),
        transform: rw_render::IDENTITY,
    };
    let scene = Scene {
        grid: None,
        layers: vec![Layer::new(Content::Solids(vec![solid]))],
        ..Scene::default()
    };
    // Twice over the same key, then again after forgetting it: all three must
    // produce the same picture, which is what says the cache is transparent.
    let first = renderer.render(&scene, 64, 64).expect("renders");
    let second = renderer.render(&scene, 64, 64).expect("renders");
    renderer.forget(&[99]);
    let third = renderer.render(&scene, 64, 64).expect("renders");
    assert_eq!(first.rgba, second.rgba);
    assert_eq!(first.rgba, third.rgba);
}
