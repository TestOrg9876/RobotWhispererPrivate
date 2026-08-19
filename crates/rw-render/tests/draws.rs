//! The one thing the unit tests cannot check: that a GPU actually draws this.
//!
//! Skipped rather than failed when there is no adapter, so a machine without
//! one still gets the rest of the suite. Everything up to the draw call is
//! covered by the unit tests in `camera` and `scene`.

use rw_render::{Camera, Coloring, Grid, Points, Renderer, Scene};

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
        points: Points {
            positions: vec![[0., 0., 0.]],
            rgb: Some(vec![[255, 0, 0]]),
            coloring: Coloring::Rgb,
            ..Points::default()
        },
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
