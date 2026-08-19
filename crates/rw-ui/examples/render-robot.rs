//! Renders a robot straight to a PNG, without a window.
//!
//! When something looks wrong on screen this says whether the fault is in the
//! renderer or in the pane around it.

use std::sync::Arc;

use rw_assets::catalog::Catalog;
use rw_assets::kinematics::{self, Pose};
use rw_assets::math;
use rw_render::{Camera, Grid, MeshVertex, Renderer, Scene, Solid};

fn main() {
    let mut args = std::env::args().skip(1);
    let id = args.next().unwrap_or_else(|| "ur10e".into());
    let with_grid = args.next().as_deref() != Some("nogrid");

    let catalog = Catalog::open("assets").expect("assets");
    // Optionally show another robot first and put it away, which is what the
    // pane does when the user switches.
    let before = std::env::var("RW_PROBE_FIRST").ok();
    let loaded = catalog.load(&id).expect("robot");
    let placed = kinematics::solve(&loaded.robot, &Pose::rest(&loaded.robot));
    let correction = loaded.entry.correction();

    let mut solids = Vec::new();
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for (index, (link, parts)) in loaded.meshes.iter().enumerate() {
        let Some(world) = placed.get(link) else {
            continue;
        };
        let transform = math::multiply(correction, *world);
        let mut vertices = Vec::new();
        for part in parts {
            let color = part.color.unwrap_or([0.72, 0.73, 0.76, 1.]);
            for i in &part.indices {
                let i = *i as usize;
                let (Some(p), Some(n)) = (part.positions.get(i), part.normals.get(i)) else {
                    continue;
                };
                vertices.push(MeshVertex::new(*p, *n, color));
                let q = math::transform_point(transform, *p);
                for a in 0..3 {
                    min[a] = min[a].min(q[a]);
                    max[a] = max[a].max(q[a]);
                }
            }
        }
        solids.push(Solid {
            key: index as u64,
            vertices: Arc::new(vertices),
            transform,
        });
    }

    let mut camera = Camera::default();
    camera.frame(min, max);
    let widest = (0..3).map(|a| max[a] - min[a]).fold(0f32, f32::max);
    println!("{id}: {} links, widest {widest:.3} m", solids.len());

    let renderer = pollster::block_on(Renderer::new()).expect("device");

    if let Some(first) = before {
        let first = catalog.load(&first).expect("first robot");
        let placed = kinematics::solve(&first.robot, &Pose::rest(&first.robot));
        let correction = first.entry.correction();
        let mut warmup = Vec::new();
        for (index, (link, parts)) in first.meshes.iter().enumerate() {
            let Some(world) = placed.get(link) else {
                continue;
            };
            let mut vertices = Vec::new();
            for part in parts {
                let color = part.color.unwrap_or([0.72, 0.73, 0.76, 1.]);
                for i in &part.indices {
                    let i = *i as usize;
                    let (Some(p), Some(n)) = (part.positions.get(i), part.normals.get(i)) else {
                        continue;
                    };
                    vertices.push(MeshVertex::new(*p, *n, color));
                }
            }
            warmup.push(Solid {
                key: (1u64 << 32) | index as u64,
                vertices: Arc::new(vertices),
                transform: math::multiply(correction, *world),
            });
        }
        let keys: Vec<u64> = warmup.iter().map(|solid| solid.key).collect();
        let warm = Scene {
            solids: warmup,
            ..Scene::default()
        };
        renderer.render(&warm, 900, 700).expect("renders");
        renderer.forget(&keys);
        println!("showed {} first, then put it away", keys.len());
    }
    let scene = Scene {
        camera,
        solids,
        grid: with_grid.then(|| Grid::for_size(widest)),
        ..Scene::default()
    };
    let frame = renderer.render(&scene, 900, 700).expect("renders");
    let image =
        image_crate::RgbaImage::from_raw(frame.width, frame.height, frame.rgba).expect("image");
    let path = format!("target/{id}-probe.png");
    image.save(&path).expect("saves");
    println!("wrote {path}");
}
