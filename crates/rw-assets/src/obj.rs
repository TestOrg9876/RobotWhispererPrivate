//! Reading a Wavefront `.obj` mesh.
//!
//! Simpler than COLLADA and correspondingly less to get wrong: vertices,
//! normals and faces, split into runs by `usemtl`. Colours come from the
//! companion `.mtl` when the caller can supply it — the URDF names only the
//! `.obj`, so resolving the sidecar is the caller's business.

use std::collections::HashMap;

use crate::mesh::{Mesh, Part, face_normal};

/// Reads an OBJ document. `materials` maps a material name to its colour, and
/// may be empty when there is no `.mtl` alongside.
pub fn parse(source: &str, materials: &HashMap<String, [f32; 4]>) -> Mesh {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    // One run per material, in first-use order, so a file that switches back
    // and forth does not end up with a part per switch.
    let mut runs: Vec<Part> = Vec::new();
    let mut current: Option<usize> = None;

    for line in source.lines() {
        let line = line.trim();
        let mut words = line.split_whitespace();
        match words.next() {
            Some("v") => {
                if let Some(vertex) = triple(&mut words) {
                    positions.push(vertex);
                }
            }
            Some("vn") => {
                if let Some(normal) = triple(&mut words) {
                    normals.push(normal);
                }
            }
            Some("usemtl") => {
                let name = words.next().unwrap_or_default().to_string();
                current = Some(
                    match runs
                        .iter()
                        .position(|run| run.material.as_deref() == Some(name.as_str()))
                    {
                        Some(existing) => existing,
                        None => {
                            runs.push(Part {
                                color: materials.get(&name).copied(),
                                material: Some(name),
                                ..Part::default()
                            });
                            runs.len() - 1
                        }
                    },
                );
            }
            Some("f") => {
                let corners: Vec<(usize, Option<usize>)> = words
                    .filter_map(|word| corner(word, positions.len(), normals.len()))
                    .collect();
                if corners.len() < 3 {
                    continue;
                }
                let run = match current {
                    Some(index) => index,
                    None => {
                        // A file with no `usemtl` still has geometry.
                        runs.push(Part::default());
                        current = Some(runs.len() - 1);
                        runs.len() - 1
                    }
                };
                // A fan, which is right for the convex faces exporters emit.
                for corner in 1..corners.len() - 1 {
                    for (position, normal) in [corners[0], corners[corner], corners[corner + 1]] {
                        let part = &mut runs[run];
                        part.indices.push(part.positions.len() as u32);
                        part.positions.push(positions[position]);
                        part.normals
                            .push(normal.map(|index| normals[index]).unwrap_or([0., 0., 0.]));
                    }
                }
            }
            _ => {}
        }
    }

    for part in &mut runs {
        if part.normals.contains(&[0., 0., 0.]) {
            fill_missing_normals(part);
        }
    }
    runs.retain(|part| !part.indices.is_empty());
    Mesh { parts: runs }
}

/// Reads a `.mtl`: material name to diffuse colour.
pub fn parse_materials(source: &str) -> HashMap<String, [f32; 4]> {
    let mut materials = HashMap::new();
    let mut current: Option<String> = None;
    for line in source.lines() {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("newmtl") => {
                current = words.next().map(str::to_string);
                if let Some(name) = &current {
                    materials.insert(name.clone(), [0.8, 0.8, 0.8, 1.]);
                }
            }
            Some("Kd") => {
                if let (Some(name), Some(rgb)) = (current.as_ref(), triple(&mut words)) {
                    let alpha = materials.get(name).map_or(1., |color| color[3]);
                    materials.insert(name.clone(), [rgb[0], rgb[1], rgb[2], alpha]);
                }
            }
            // `d` is opacity, `Tr` is transparency: the same number inverted,
            // and files use one or the other.
            Some(key @ ("d" | "Tr")) => {
                if let (Some(name), Some(value)) = (
                    current.as_ref(),
                    words.next().and_then(|v| v.parse::<f32>().ok()),
                ) {
                    let alpha = if key == "d" { value } else { 1. - value };
                    if let Some(color) = materials.get_mut(name) {
                        color[3] = alpha;
                    }
                }
            }
            _ => {}
        }
    }
    materials
}

/// One `f` corner: `v`, `v/vt`, `v//vn` or `v/vt/vn`, one-based, and negative
/// when counting back from the end of the file so far.
fn corner(word: &str, positions: usize, normals: usize) -> Option<(usize, Option<usize>)> {
    let mut parts = word.split('/');
    let position = index(parts.next()?, positions)?;
    let _texture = parts.next();
    let normal = parts.next().and_then(|raw| index(raw, normals));
    Some((position, normal))
}

fn index(raw: &str, count: usize) -> Option<usize> {
    let value: isize = raw.trim().parse().ok()?;
    let resolved = if value < 0 {
        count.checked_sub(value.unsigned_abs())?
    } else {
        usize::try_from(value).ok()?.checked_sub(1)?
    };
    (resolved < count).then_some(resolved)
}

/// Gives faces without their own normals a flat one, leaving the rest alone.
fn fill_missing_normals(part: &mut Part) {
    for triangle in part.indices.chunks_exact(3) {
        let corners: Vec<[f32; 3]> = triangle
            .iter()
            .filter_map(|index| part.positions.get(*index as usize).copied())
            .collect();
        let [a, b, c] = corners[..] else { continue };
        let normal = face_normal(a, b, c);
        for index in triangle {
            if let Some(slot) = part.normals.get_mut(*index as usize)
                && *slot == [0., 0., 0.]
            {
                *slot = normal;
            }
        }
    }
}

fn triple<'a>(words: &mut impl Iterator<Item = &'a str>) -> Option<[f32; 3]> {
    let mut triple = [0.; 3];
    for slot in triple.iter_mut() {
        *slot = words.next()?.parse().ok()?;
    }
    Some(triple)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQUARE: &str = "\
# a comment
v 0 0 0
v 1 0 0
v 1 1 0
v 0 1 0
vn 0 0 1
usemtl paint
f 1//1 2//1 3//1 4//1
";

    fn no_materials() -> HashMap<String, [f32; 4]> {
        HashMap::new()
    }

    #[test]
    fn a_quad_face_is_fanned_into_two_triangles() {
        let mesh = parse(SQUARE, &no_materials());
        assert_eq!(mesh.triangle_count(), 2);
        assert_eq!(mesh.parts[0].material.as_deref(), Some("paint"));
    }

    #[test]
    fn indices_are_one_based() {
        let mesh = parse(SQUARE, &no_materials());
        assert_eq!(mesh.parts[0].positions[0], [0., 0., 0.]);
        assert_eq!(mesh.parts[0].positions[1], [1., 0., 0.]);
    }

    #[test]
    fn negative_indices_count_back_from_the_end() {
        let mesh = parse("v 0 0 0\nv 1 0 0\nv 0 1 0\nf -3 -2 -1\n", &no_materials());
        assert_eq!(mesh.triangle_count(), 1);
        assert_eq!(mesh.parts[0].positions[2], [0., 1., 0.]);
    }

    #[test]
    fn a_face_with_no_normal_index_gets_a_flat_one() {
        let mesh = parse("v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n", &no_materials());
        assert!((mesh.parts[0].normals[0][2] - 1.).abs() < 1e-6);
    }

    #[test]
    fn colours_come_from_the_material_library() {
        let mut materials = HashMap::new();
        materials.insert("paint".to_string(), [0.2, 0.4, 0.6, 1.]);
        let mesh = parse(SQUARE, &materials);
        assert_eq!(mesh.parts[0].color, Some([0.2, 0.4, 0.6, 1.]));
    }

    #[test]
    fn switching_back_to_an_earlier_material_reuses_its_run() {
        let mesh = parse(
            "v 0 0 0\nv 1 0 0\nv 0 1 0\nv 1 1 0\n\
             usemtl a\nf 1 2 3\nusemtl b\nf 2 3 4\nusemtl a\nf 1 2 4\n",
            &no_materials(),
        );
        assert_eq!(mesh.parts.len(), 2, "two materials, two runs");
        assert_eq!(mesh.parts[0].triangle_count(), 2);
    }

    #[test]
    fn geometry_before_any_usemtl_is_kept() {
        let mesh = parse("v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n", &no_materials());
        assert_eq!(mesh.triangle_count(), 1);
        assert_eq!(mesh.parts[0].material, None);
    }

    #[test]
    fn an_out_of_range_index_is_skipped_rather_than_panicking() {
        let mesh = parse("v 0 0 0\nf 1 2 3\nf 900 901 902\n", &no_materials());
        assert!(mesh.is_empty());
    }

    #[test]
    fn a_material_library_reads_colour_and_opacity() {
        let materials = parse_materials(
            "newmtl shell\nKd 0.1 0.2 0.3\nd 0.5\nnewmtl trim\nKd 1 1 1\nTr 0.25\n",
        );
        assert_eq!(materials["shell"], [0.1, 0.2, 0.3, 0.5]);
        assert_eq!(materials["trim"], [1., 1., 1., 0.75]);
    }

    #[test]
    fn an_empty_document_is_an_empty_mesh() {
        assert!(parse("", &no_materials()).is_empty());
    }

    #[test]
    fn the_real_iiwa_meshes_load() {
        for (name, source) in [
            (
                "link_0",
                include_str!("../../../assets/iiwa14/meshes/visual/link_0.obj"),
            ),
            (
                "link_2_orange",
                include_str!("../../../assets/iiwa14/meshes/visual/link_2_orange.obj"),
            ),
        ] {
            let mesh = parse(source, &no_materials());
            assert!(mesh.triangle_count() > 100, "{name} loaded almost nothing");
            let (min, max) = mesh.bounds().expect("has bounds");
            for axis in 0..3 {
                let side = max[axis] - min[axis];
                assert!(side < 1.5, "{name} is {side} metres across axis {axis}");
            }
            assert!(
                mesh.parts
                    .iter()
                    .flat_map(|part| &part.normals)
                    .all(|normal| normal.iter().all(|c| c.is_finite())),
                "{name} produced a normal that is not a number"
            );
        }
    }

    #[test]
    fn the_real_allegro_meshes_load() {
        let mesh = parse(
            include_str!("../../../assets/allegro_hand/meshes/base_link.obj"),
            &no_materials(),
        );
        assert!(mesh.triangle_count() > 100);
        // The hand's base is about 10 cm across.
        let (min, max) = mesh.bounds().expect("has bounds");
        assert!(max[0] - min[0] < 0.5);
    }
}
