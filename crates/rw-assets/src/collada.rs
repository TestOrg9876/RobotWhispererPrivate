//! Reading a COLLADA (`.dae`) mesh.
//!
//! Only the path a robot description actually uses: geometry, per-material
//! diffuse colour, and the scene node that says what units and axis convention
//! the file was authored in. Animation, skinning, cameras and lights are read
//! past.
//!
//! The scene node matters more than it looks. Every mesh shipped with these
//! robots was exported from Blender in millimetres with a Y-up basis, and the
//! correction is a matrix on the node rather than something baked into the
//! vertices — ignoring it puts a thousand-metre arm on its side.

use std::collections::HashMap;

use crate::math::{self, Mat4};
use crate::mesh::{Mesh, Part, face_normal};

#[derive(Debug, thiserror::Error)]
pub enum ColladaError {
    #[error("not valid XML: {0}")]
    Xml(#[from] roxmltree::Error),
    #[error("no <COLLADA> element")]
    NotCollada,
}

/// Reads a COLLADA document into triangles, in metres and in the file's own
/// frame after its scene transforms have been applied.
pub fn parse(source: &str) -> Result<Mesh, ColladaError> {
    let document = roxmltree::Document::parse(source)?;
    let root = document.root_element();
    if root.tag_name().name() != "COLLADA" {
        return Err(ColladaError::NotCollada);
    }

    let effects = effects(root);
    let materials = materials(root, &effects);
    let geometries = geometries(root);

    let mut mesh = Mesh::default();
    // The scene is what places geometry; a file with none still has geometry
    // worth drawing, so it falls back to instancing everything once.
    let scene = root
        .descendants()
        .find(|node| node.has_tag_name("visual_scene"));
    match scene {
        Some(scene) => {
            for node in scene.children().filter(|node| node.is_element()) {
                walk(node, math::IDENTITY, &geometries, &materials, &mut mesh);
            }
        }
        None => {
            for parts in geometries.values() {
                mesh.parts.extend(parts.iter().cloned());
            }
        }
    }

    // An exporter that writes no normals is not unusual, and flat shading beats
    // a black model.
    for part in &mut mesh.parts {
        if part.normals.len() != part.positions.len() {
            part.normals = flat_normals(part);
        }
    }
    Ok(mesh)
}

/// Walks the scene tree, accumulating node transforms.
fn walk(
    node: roxmltree::Node,
    parent: Mat4,
    geometries: &HashMap<String, Vec<Part>>,
    materials: &HashMap<String, (String, Option<[f32; 4]>)>,
    mesh: &mut Mesh,
) {
    if !node.has_tag_name("node") {
        return;
    }
    let world = math::multiply(parent, node_transform(node));

    for instance in node
        .children()
        .filter(|child| child.has_tag_name("instance_geometry"))
    {
        let Some(parts) = instance
            .attribute("url")
            .and_then(|url| geometries.get(url.trim_start_matches('#')))
        else {
            continue;
        };
        // `bind_material` renames the file's material symbols per instance, so
        // the same geometry can be drawn in two colours.
        let bindings = material_bindings(instance);
        for part in parts {
            let bound = part
                .material
                .as_ref()
                .and_then(|symbol| bindings.get(symbol))
                .and_then(|target| materials.get(target));
            let mut part = part.clone().transformed(world);
            if let Some((name, color)) = bound {
                part.material = Some(name.clone());
                part.color = *color;
            }
            mesh.parts.push(part);
        }
    }

    for child in node.children().filter(|child| child.is_element()) {
        walk(child, world, geometries, materials, mesh);
    }
}

/// A node's local transform.
///
/// `<matrix>` is row-major in COLLADA and column-major here, hence the
/// transpose. `translate`, `rotate` and `scale` may appear instead, and compose
/// in document order.
fn node_transform(node: roxmltree::Node) -> Mat4 {
    let mut transform = math::IDENTITY;
    for child in node.children().filter(|child| child.is_element()) {
        let numbers = floats(child.text().unwrap_or_default());
        let step = match child.tag_name().name() {
            "matrix" if numbers.len() == 16 => {
                let mut matrix = math::IDENTITY;
                for row in 0..4 {
                    for column in 0..4 {
                        matrix[column][row] = numbers[row * 4 + column];
                    }
                }
                matrix
            }
            "translate" if numbers.len() == 3 => {
                math::translation([numbers[0], numbers[1], numbers[2]])
            }
            "rotate" if numbers.len() == 4 => math::from_axis_angle(
                [numbers[0], numbers[1], numbers[2]],
                numbers[3].to_radians(),
            ),
            "scale" if numbers.len() == 3 => math::scale([numbers[0], numbers[1], numbers[2]]),
            _ => continue,
        };
        transform = math::multiply(transform, step);
    }
    transform
}

/// `symbol` → material id, from an instance's `bind_material`.
fn material_bindings(instance: roxmltree::Node) -> HashMap<String, String> {
    instance
        .descendants()
        .filter(|node| node.has_tag_name("instance_material"))
        .filter_map(|node| {
            Some((
                node.attribute("symbol")?.to_string(),
                node.attribute("target")?
                    .trim_start_matches('#')
                    .to_string(),
            ))
        })
        .collect()
}

/// Effect id → diffuse colour.
fn effects(root: roxmltree::Node) -> HashMap<String, Option<[f32; 4]>> {
    root.descendants()
        .filter(|node| node.has_tag_name("effect"))
        .filter_map(|effect| Some((effect.attribute("id")?.to_string(), diffuse(effect))))
        .collect()
}

fn diffuse(effect: roxmltree::Node) -> Option<[f32; 4]> {
    let diffuse = effect
        .descendants()
        .find(|node| node.has_tag_name("diffuse"))?;
    let color = diffuse.children().find(|node| node.has_tag_name("color"))?;
    let numbers = floats(color.text().unwrap_or_default());
    (numbers.len() >= 3).then(|| {
        [
            numbers[0],
            numbers[1],
            numbers[2],
            numbers.get(3).copied().unwrap_or(1.),
        ]
    })
}

/// Material id → (name, colour).
fn materials(
    root: roxmltree::Node,
    effects: &HashMap<String, Option<[f32; 4]>>,
) -> HashMap<String, (String, Option<[f32; 4]>)> {
    root.descendants()
        .filter(|node| node.has_tag_name("material"))
        .filter_map(|material| {
            let id = material.attribute("id")?.to_string();
            let name = material.attribute("name").unwrap_or(&id).to_string();
            let color = material
                .children()
                .find(|node| node.has_tag_name("instance_effect"))
                .and_then(|node| node.attribute("url"))
                .and_then(|url| effects.get(url.trim_start_matches('#')))
                .copied()
                .flatten();
            Some((id, (name, color)))
        })
        .collect()
}

/// Geometry id → its triangle runs, one per material symbol.
fn geometries(root: roxmltree::Node) -> HashMap<String, Vec<Part>> {
    root.descendants()
        .filter(|node| node.has_tag_name("geometry"))
        .filter_map(|geometry| {
            let id = geometry.attribute("id")?.to_string();
            let mesh = geometry.children().find(|node| node.has_tag_name("mesh"))?;
            Some((id, primitives(mesh)))
        })
        .collect()
}

fn primitives(mesh: roxmltree::Node) -> Vec<Part> {
    // `source` holds the raw arrays; `vertices` is one level of indirection
    // pointing VERTEX at a POSITION source.
    let sources: HashMap<&str, Vec<f32>> = mesh
        .children()
        .filter(|node| node.has_tag_name("source"))
        .filter_map(|source| {
            let array = source
                .children()
                .find(|node| node.has_tag_name("float_array"))?;
            Some((source.attribute("id")?, floats(array.text()?)))
        })
        .collect();

    let vertices: HashMap<&str, &str> = mesh
        .children()
        .filter(|node| node.has_tag_name("vertices"))
        .filter_map(|node| {
            let position = node
                .children()
                .find(|input| input.attribute("semantic") == Some("POSITION"))?;
            Some((
                node.attribute("id")?,
                position.attribute("source")?.trim_start_matches('#'),
            ))
        })
        .collect();

    mesh.children()
        .filter(|node| {
            matches!(
                node.tag_name().name(),
                "triangles" | "polylist" | "polygons"
            )
        })
        .filter_map(|node| part(node, &sources, &vertices))
        .collect()
}

fn part(
    node: roxmltree::Node,
    sources: &HashMap<&str, Vec<f32>>,
    vertices: &HashMap<&str, &str>,
) -> Option<Part> {
    let inputs: Vec<(&str, usize, &str)> = node
        .children()
        .filter(|child| child.has_tag_name("input"))
        .filter_map(|input| {
            Some((
                input.attribute("semantic")?,
                input.attribute("offset")?.parse().ok()?,
                input.attribute("source")?.trim_start_matches('#'),
            ))
        })
        .collect();
    // How many indices make up one vertex reference.
    let stride = inputs.iter().map(|(_, offset, _)| offset + 1).max()?;

    let lookup = |semantic: &str| -> Option<&Vec<f32>> {
        let (_, _, source) = inputs.iter().find(|(name, _, _)| *name == semantic)?;
        // `vertices` is COLLADA's one level of indirection: a VERTEX input
        // points at a `<vertices>` element, which points at the real source.
        let source = vertices.get(source).copied().unwrap_or(source);
        sources.get(source)
    };
    let offset_of = |semantic: &str| -> Option<usize> {
        inputs
            .iter()
            .find(|(name, _, _)| *name == semantic)
            .map(|(_, offset, _)| *offset)
    };

    let positions = lookup("VERTEX")?;
    let position_offset = offset_of("VERTEX")?;
    let normals = lookup("NORMAL");
    let normal_offset = offset_of("NORMAL");

    // `<polygons>` splits its indices across several `<p>` elements; the others
    // use one. Concatenating covers both.
    let indices: Vec<usize> = node
        .children()
        .filter(|child| child.has_tag_name("p"))
        .flat_map(|p| {
            p.text()
                .unwrap_or_default()
                .split_whitespace()
                .filter_map(|value| value.parse::<usize>().ok())
                .collect::<Vec<_>>()
        })
        .collect();

    // A polylist may hold quads and larger faces, which are fanned into
    // triangles. `triangles` has no vcount and is already threes.
    let vcount: Vec<usize> = node
        .children()
        .find(|child| child.has_tag_name("vcount"))
        .map(|node| {
            node.text()
                .unwrap_or_default()
                .split_whitespace()
                .filter_map(|value| value.parse().ok())
                .collect()
        })
        .unwrap_or_else(|| vec![3; indices.len() / stride / 3]);

    let mut part = Part {
        material: node.attribute("material").map(str::to_string),
        ..Part::default()
    };

    let mut cursor = 0;
    for corners in vcount {
        if corners < 3 || (cursor + corners) * stride > indices.len() {
            break;
        }
        let vertex = |corner: usize| -> Option<([f32; 3], [f32; 3])> {
            let base = (cursor + corner) * stride;
            let position = triple(positions, *indices.get(base + position_offset)?)?;
            let normal = match (normals, normal_offset) {
                (Some(normals), Some(offset)) => {
                    triple(normals, *indices.get(base + offset)?).unwrap_or([0., 0., 0.])
                }
                _ => [0., 0., 0.],
            };
            Some((position, normal))
        };
        // A fan: correct for a convex polygon, which is what an exporter emits.
        for corner in 1..corners - 1 {
            let Some(corners) = (|| Some([vertex(0)?, vertex(corner)?, vertex(corner + 1)?]))()
            else {
                continue;
            };
            for (position, normal) in corners {
                part.indices.push(part.positions.len() as u32);
                part.positions.push(position);
                part.normals.push(normal);
            }
        }
        cursor += corners;
    }

    (!part.indices.is_empty()).then_some(part)
}

fn flat_normals(part: &Part) -> Vec<[f32; 3]> {
    let mut normals = vec![[0., 0., 1.]; part.positions.len()];
    for triangle in part.indices.chunks_exact(3) {
        let corners: Vec<[f32; 3]> = triangle
            .iter()
            .filter_map(|index| part.positions.get(*index as usize).copied())
            .collect();
        let [a, b, c] = corners[..] else { continue };
        let normal = face_normal(a, b, c);
        for index in triangle {
            if let Some(slot) = normals.get_mut(*index as usize) {
                *slot = normal;
            }
        }
    }
    normals
}

fn triple(values: &[f32], index: usize) -> Option<[f32; 3]> {
    let base = index.checked_mul(3)?;
    Some([
        *values.get(base)?,
        *values.get(base + 1)?,
        *values.get(base + 2)?,
    ])
}

fn floats(text: &str) -> Vec<f32> {
    text.split_whitespace()
        .filter_map(|value| value.parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One triangle, in a scene node that scales it by ten.
    const SIMPLE: &str = r##"
      <COLLADA version="1.4.1">
        <library_effects>
          <effect id="red-effect">
            <profile_COMMON><technique sid="common"><phong>
              <diffuse><color sid="diffuse">1 0 0 1</color></diffuse>
            </phong></technique></profile_COMMON>
          </effect>
        </library_effects>
        <library_materials>
          <material id="red-material" name="Red"><instance_effect url="#red-effect"/></material>
        </library_materials>
        <library_geometries>
          <geometry id="tri-mesh">
            <mesh>
              <source id="tri-positions">
                <float_array count="9">0 0 0  1 0 0  0 1 0</float_array>
              </source>
              <source id="tri-normals">
                <float_array count="3">0 0 1</float_array>
              </source>
              <vertices id="tri-vertices">
                <input semantic="POSITION" source="#tri-positions"/>
              </vertices>
              <polylist material="red-material" count="1">
                <input semantic="VERTEX" source="#tri-vertices" offset="0"/>
                <input semantic="NORMAL" source="#tri-normals" offset="1"/>
                <vcount>3</vcount>
                <p>0 0 1 0 2 0</p>
              </polylist>
            </mesh>
          </geometry>
        </library_geometries>
        <library_visual_scenes>
          <visual_scene id="Scene">
            <node id="n" type="NODE">
              <matrix sid="transform">10 0 0 0  0 10 0 0  0 0 10 0  0 0 0 1</matrix>
              <instance_geometry url="#tri-mesh">
                <bind_material><technique_common>
                  <instance_material symbol="red-material" target="#red-material"/>
                </technique_common></bind_material>
              </instance_geometry>
            </node>
          </visual_scene>
        </library_visual_scenes>
      </COLLADA>
    "##;

    #[test]
    fn a_triangle_comes_through_with_its_material() {
        let mesh = parse(SIMPLE).expect("parses");
        assert_eq!(mesh.triangle_count(), 1);
        let part = &mesh.parts[0];
        assert_eq!(part.material.as_deref(), Some("Red"));
        assert_eq!(part.color, Some([1., 0., 0., 1.]));
    }

    #[test]
    fn the_scene_node_transform_is_baked_into_the_vertices() {
        let mesh = parse(SIMPLE).expect("parses");
        // Without the node matrix this would be [1, 0, 0]: the whole reason
        // these meshes are not a thousand times too big.
        assert_eq!(mesh.parts[0].positions[1], [10., 0., 0.]);
    }

    #[test]
    fn a_collada_matrix_is_read_row_major() {
        let source = SIMPLE.replace(
            "10 0 0 0  0 10 0 0  0 0 10 0  0 0 0 1",
            "1 0 0 7  0 1 0 0  0 0 1 0  0 0 0 1",
        );
        let mesh = parse(&source).expect("parses");
        // The 7 is a translation in x. Read column-major it would be a shear.
        assert_eq!(mesh.parts[0].positions[0], [7., 0., 0.]);
    }

    #[test]
    fn normals_use_their_own_offset_not_the_positions() {
        let mesh = parse(SIMPLE).expect("parses");
        assert_eq!(mesh.parts[0].normals, vec![[0., 0., 1.]; 3]);
    }

    #[test]
    fn a_polygon_with_more_than_three_corners_is_fanned_into_triangles() {
        let source = SIMPLE
            .replace(
                r##"<float_array count="9">0 0 0  1 0 0  0 1 0</float_array>"##,
                r##"<float_array count="12">0 0 0  1 0 0  1 1 0  0 1 0</float_array>"##,
            )
            .replace("<vcount>3</vcount>", "<vcount>4</vcount>")
            .replace("<p>0 0 1 0 2 0</p>", "<p>0 0 1 0 2 0 3 0</p>");
        let mesh = parse(&source).expect("parses");
        assert_eq!(mesh.triangle_count(), 2, "a quad is two triangles");
    }

    #[test]
    fn a_file_without_normals_gets_flat_ones_rather_than_a_black_model() {
        let source = SIMPLE
            .replace(
                r##"<input semantic="NORMAL" source="#tri-normals" offset="1"/>"##,
                "",
            )
            .replace("<p>0 0 1 0 2 0</p>", "<p>0 1 2</p>");
        let mesh = parse(&source).expect("parses");
        assert_eq!(mesh.parts[0].normals.len(), 3);
        assert!((mesh.parts[0].normals[0][2] - 1.).abs() < 1e-5);
    }

    #[test]
    fn geometry_with_no_scene_is_still_drawn() {
        let source = SIMPLE
            .split("<library_visual_scenes>")
            .next()
            .expect("has a head")
            .to_string()
            + "</COLLADA>";
        let mesh = parse(&source).expect("parses");
        assert_eq!(mesh.triangle_count(), 1);
        assert_eq!(
            mesh.parts[0].positions[1],
            [1., 0., 0.],
            "untransformed, because nothing placed it"
        );
    }

    #[test]
    fn something_that_is_not_collada_is_refused() {
        assert!(matches!(
            parse("<robot name='a'/>"),
            Err(ColladaError::NotCollada)
        ));
        assert!(matches!(parse("<<<"), Err(ColladaError::Xml(_))));
    }

    #[test]
    fn the_real_ur10e_base_mesh_loads_at_a_believable_size() {
        let mesh = parse(include_str!("../../../assets/ur10e/meshes/visual/base.dae"))
            .expect("the shipped mesh parses");
        assert!(
            mesh.triangle_count() > 1000,
            "got {}",
            mesh.triangle_count()
        );

        let (min, max) = mesh.bounds().expect("has bounds");
        let extent = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
        // A UR10e base is roughly 190 mm across and 120 mm tall. In metres,
        // every side is well under one — which is only true if the scene node's
        // millimetre scale was applied.
        assert!(
            extent.iter().all(|side| (0.02..1.0).contains(side)),
            "the base came out {extent:?} metres across"
        );
        assert!(
            mesh.parts.iter().any(|part| part.color.is_some()),
            "the shipped mesh names its materials"
        );
    }

    #[test]
    fn every_shipped_ur10e_mesh_loads() {
        for (name, source) in [
            (
                "base",
                include_str!("../../../assets/ur10e/meshes/visual/base.dae"),
            ),
            (
                "shoulder",
                include_str!("../../../assets/ur10e/meshes/visual/shoulder.dae"),
            ),
            (
                "upperarm",
                include_str!("../../../assets/ur10e/meshes/visual/upperarm.dae"),
            ),
            (
                "forearm",
                include_str!("../../../assets/ur10e/meshes/visual/forearm.dae"),
            ),
            (
                "wrist1",
                include_str!("../../../assets/ur10e/meshes/visual/wrist1.dae"),
            ),
            (
                "wrist2",
                include_str!("../../../assets/ur10e/meshes/visual/wrist2.dae"),
            ),
            (
                "wrist3",
                include_str!("../../../assets/ur10e/meshes/visual/wrist3.dae"),
            ),
        ] {
            let mesh = parse(source).unwrap_or_else(|error| panic!("{name}: {error}"));
            assert!(!mesh.is_empty(), "{name} loaded no triangles");
            let (min, max) = mesh.bounds().expect("has bounds");
            for axis in 0..3 {
                assert!(
                    max[axis] - min[axis] < 2.,
                    "{name} is {} metres across axis {axis}",
                    max[axis] - min[axis]
                );
            }
        }
    }
}
