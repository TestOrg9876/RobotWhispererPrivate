//! What one pane is showing.
//!
//! A scene is plain data: a camera, a ground grid, and a list of layers. Each
//! layer carries the matrix that places its content in the scene's fixed frame,
//! and nothing is turned into vertices until draw time — so a pane can hand in
//! a new cloud, or move a robot, without knowing that a GPU exists.

use crate::Vertex;
use crate::camera::{Camera, Mat4, multiply, transform_point};

/// The ground plane drawn under everything, for a sense of scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    /// Spacing between lines, in metres.
    pub step: f32,
    /// How many lines out from the centre, each way.
    pub extent: i32,
    pub color: [f32; 4],
    /// The two lines through the origin, drawn brighter.
    pub axis_color: [f32; 4],
}

impl Default for Grid {
    fn default() -> Self {
        Self {
            step: 1.,
            extent: 10,
            color: [0.30, 0.33, 0.38, 0.55],
            axis_color: [0.45, 0.50, 0.58, 0.9],
        }
    }
}

impl Grid {
    /// A grid sized for something this big across.
    ///
    /// A one-metre grid under a ten-centimetre hand is a single square, and a
    /// ten-metre one is a thicket of lines behind it — the spacing has to follow
    /// the subject. The step is snapped to a 1-2-5 progression so the numbers
    /// stay ones a person would have chosen.
    pub fn for_size(size: f32) -> Self {
        let target = (size.max(0.001) / 8.).max(0.001);
        let magnitude = 10f32.powf(target.log10().floor());
        let step = match target / magnitude {
            ratio if ratio < 1.5 => magnitude,
            ratio if ratio < 3.5 => 2. * magnitude,
            ratio if ratio < 7.5 => 5. * magnitude,
            _ => 10. * magnitude,
        };
        Self {
            step,
            // Enough to reach comfortably past the subject without becoming a
            // haze at the horizon.
            extent: ((size / step).ceil() as i32).clamp(4, 20),
            ..Self::default()
        }
    }
}

/// How points are coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Coloring {
    /// Height, run through a blue-to-red ramp. Always available.
    #[default]
    Axis,
    /// The message's own `intensity` channel.
    Intensity,
    /// The message's own packed `rgb` channel.
    Rgb,
}

impl Coloring {
    pub fn label(self) -> &'static str {
        match self {
            Self::Axis => "Height",
            Self::Intensity => "Intensity",
            Self::Rgb => "Colour",
        }
    }
}

/// A cloud of points, with whatever the message offered to colour them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Points {
    pub positions: Vec<[f32; 3]>,
    pub rgb: Option<Vec<[u8; 3]>>,
    pub intensity: Option<Vec<f32>>,
    pub coloring: Coloring,
}

impl Points {
    /// Which colourings this cloud can actually offer, in the order shown.
    pub fn available(&self) -> Vec<Coloring> {
        let mut available = vec![Coloring::Axis];
        if self.intensity.is_some() {
            available.push(Coloring::Intensity);
        }
        if self.rgb.is_some() {
            available.push(Coloring::Rgb);
        }
        available
    }

    /// The colour of one point, given the span its scalar channel covers.
    fn color(&self, index: usize, span: (f32, f32)) -> [f32; 4] {
        match self.coloring {
            Coloring::Rgb => match self.rgb.as_ref().and_then(|rgb| rgb.get(index)) {
                Some([r, g, b]) => [*r as f32 / 255., *g as f32 / 255., *b as f32 / 255., 1.],
                None => [1., 1., 1., 1.],
            },
            Coloring::Intensity => {
                let value = self
                    .intensity
                    .as_ref()
                    .and_then(|values| values.get(index))
                    .copied()
                    .unwrap_or(0.);
                ramp(normalize(value, span))
            }
            Coloring::Axis => {
                let height = self.positions.get(index).map_or(0., |point| point[2]);
                ramp(normalize(height, span))
            }
        }
    }

    /// The span the active colouring is stretched across.
    fn span(&self) -> (f32, f32) {
        let values: Box<dyn Iterator<Item = f32> + '_> = match self.coloring {
            Coloring::Intensity => match &self.intensity {
                Some(values) => Box::new(values.iter().copied()),
                None => return (0., 1.),
            },
            _ => Box::new(self.positions.iter().map(|point| point[2])),
        };
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        for value in values.filter(|value| value.is_finite()) {
            min = min.min(value);
            max = max.max(value);
        }
        if min > max { (0., 1.) } else { (min, max) }
    }
}

/// Where a value sits in a span, as 0..1. A flat span is all one colour rather
/// than a division by zero.
fn normalize(value: f32, (min, max): (f32, f32)) -> f32 {
    if (max - min).abs() < f32::EPSILON {
        return 0.5;
    }
    ((value - min) / (max - min)).clamp(0., 1.)
}

/// Blue through cyan and yellow to red: the ramp every point cloud viewer uses,
/// and the one that stays readable when printed or seen by a colourblind eye,
/// because lightness rises with the value as well as hue.
fn ramp(t: f32) -> [f32; 4] {
    let stops = [
        [0.16, 0.32, 0.75],
        [0.13, 0.68, 0.78],
        [0.62, 0.84, 0.35],
        [0.98, 0.78, 0.20],
        [0.90, 0.24, 0.22],
    ];
    let scaled = (t.clamp(0., 1.) * (stops.len() - 1) as f32).min((stops.len() - 1) as f32);
    let low = scaled.floor() as usize;
    let high = (low + 1).min(stops.len() - 1);
    let mix = scaled - low as f32;
    [
        stops[low][0] + (stops[high][0] - stops[low][0]) * mix,
        stops[low][1] + (stops[high][1] - stops[low][1]) * mix,
        stops[low][2] + (stops[high][2] - stops[low][2]) * mix,
        1.,
    ]
}

/// A run of line segments in one colour.
///
/// `strip` is the difference between a path — where each point continues from
/// the last — and a marker's line list, where the points are read in pairs.
/// Both arrive often enough that guessing would be wrong half the time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LineSet {
    pub points: Vec<[f32; 3]>,
    pub color: [f32; 4],
    pub strip: bool,
}

/// A frame drawn as its three axes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Axis {
    /// Where the frame sits inside its layer.
    pub transform: Mat4,
    /// How long the arms are, in metres.
    pub length: f32,
}

impl Default for Axis {
    fn default() -> Self {
        Self {
            transform: crate::IDENTITY,
            length: 0.25,
        }
    }
}

/// Red x, green y, blue z: the colours every robotics tool uses, and the reason
/// nobody has to be told which arm is which.
pub const AXIS_COLORS: [[f32; 4]; 3] = [
    [0.95, 0.27, 0.23, 1.],
    [0.32, 0.80, 0.35, 1.],
    [0.29, 0.53, 0.96, 1.],
];

/// What a layer is made of.
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    Points(Points),
    /// Lit surfaces: robot links, and anything else with a skin.
    Solids(Vec<crate::Solid>),
    Lines(Vec<LineSet>),
    /// Frames, as triads.
    Axes(Vec<Axis>),
}

impl Content {
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Points(points) => points.positions.is_empty(),
            Self::Solids(solids) => solids.is_empty(),
            Self::Lines(lines) => lines.iter().all(|set| set.points.len() < 2),
            Self::Axes(axes) => axes.is_empty(),
        }
    }
}

/// One thing in the world, and the transform that puts it where it belongs.
///
/// The transform is applied at draw time rather than baked into the content,
/// which is what lets a moving robot cost a matrix a frame instead of a
/// re-upload of its meshes — the geometry cache in `lib.rs`, keyed by
/// `Solid.key`, already relies on exactly that.
#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    /// Where this layer's content sits in the scene's fixed frame.
    pub transform: Mat4,
    pub content: Content,
    /// Drawn at all. A layer switched off keeps its place in the list, and its
    /// geometry stays uploaded, so turning it back on is instant.
    pub visible: bool,
}

impl Layer {
    pub fn new(content: Content) -> Self {
        Self {
            transform: crate::IDENTITY,
            content,
            visible: true,
        }
    }

    pub fn at(mut self, transform: Mat4) -> Self {
        self.transform = transform;
        self
    }
}

/// Everything one pane draws.
///
/// A list of layers rather than one of each thing: RViz's model is one world
/// with many displays in it, and a scene that held a single cloud and a single
/// set of solids could not express a scan beside a robot beside a path.
#[derive(Debug, Clone)]
pub struct Scene {
    pub camera: Camera,
    pub layers: Vec<Layer>,
    pub grid: Option<Grid>,
    pub background: [f32; 3],
    /// Point diameter, in pixels.
    pub point_size: f32,
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            camera: Camera::default(),
            layers: Vec::new(),
            grid: Some(Grid::default()),
            background: [0.055, 0.063, 0.078],
            point_size: 3.,
        }
    }
}

/// The vertices for one frame, split by the pipeline that draws them.
pub struct Vertices {
    pub points: Vec<Vertex>,
    pub lines: Vec<Vertex>,
}

/// The four corners of a point's quad, as two triangles.
const QUAD: [[f32; 2]; 6] = [
    [-1., -1.],
    [1., -1.],
    [1., 1.],
    [-1., -1.],
    [1., 1.],
    [-1., 1.],
];

impl Scene {
    /// The layers that will actually be drawn.
    fn drawn(&self) -> impl Iterator<Item = &Layer> {
        self.layers
            .iter()
            .filter(|layer| layer.visible && !layer.content.is_empty())
    }

    /// Every solid in the scene, with its layer's placement already folded in.
    ///
    /// Two matrices multiplied per link per frame, against megabytes of mesh
    /// that never move off the GPU — which is the whole point of keeping the
    /// layer's transform separate from its content.
    pub fn placed_solids(&self) -> Vec<(&crate::Solid, Mat4)> {
        self.drawn()
            .filter_map(|layer| match &layer.content {
                Content::Solids(solids) => Some((layer, solids)),
                _ => None,
            })
            .flat_map(|(layer, solids)| {
                solids
                    .iter()
                    .map(move |solid| (solid, multiply(layer.transform, solid.transform)))
            })
            .collect()
    }

    pub fn vertices(&self) -> Vertices {
        let mut points = Vec::new();
        for layer in self.drawn() {
            let Content::Points(cloud) = &layer.content else {
                continue;
            };
            // Each cloud's colour ramp is stretched across its own range: two
            // lidars with different intensity scales must each stay readable,
            // and a shared span would flatten the quieter one to one colour.
            let span = cloud.span();
            points.reserve(cloud.positions.len() * QUAD.len());
            for (index, position) in cloud.positions.iter().enumerate() {
                let color = cloud.color(index, span);
                let placed = transform_point(layer.transform, *position);
                for corner in QUAD {
                    points.push(Vertex::new(placed, color, corner));
                }
            }
        }

        let mut lines = Vec::new();
        for layer in self.drawn() {
            match &layer.content {
                Content::Lines(sets) => {
                    for set in sets {
                        let placed: Vec<[f32; 3]> = set
                            .points
                            .iter()
                            .map(|point| transform_point(layer.transform, *point))
                            .collect();
                        if set.strip {
                            for pair in placed.windows(2) {
                                lines.push(Vertex::new(pair[0], set.color, [0.; 2]));
                                lines.push(Vertex::new(pair[1], set.color, [0.; 2]));
                            }
                        } else {
                            // Read in pairs, and an odd trailing point is
                            // dropped rather than joined to nothing.
                            for pair in placed.chunks_exact(2) {
                                lines.push(Vertex::new(pair[0], set.color, [0.; 2]));
                                lines.push(Vertex::new(pair[1], set.color, [0.; 2]));
                            }
                        }
                    }
                }
                Content::Axes(axes) => {
                    for axis in axes {
                        let placed = multiply(layer.transform, axis.transform);
                        let origin = transform_point(placed, [0.; 3]);
                        for (index, color) in AXIS_COLORS.iter().enumerate() {
                            let mut arm = [0f32; 3];
                            arm[index] = axis.length;
                            lines.push(Vertex::new(origin, *color, [0.; 2]));
                            lines.push(Vertex::new(transform_point(placed, arm), *color, [0.; 2]));
                        }
                    }
                }
                _ => {}
            }
        }

        if let Some(grid) = self.grid {
            let reach = grid.step * grid.extent as f32;
            for step in -grid.extent..=grid.extent {
                let offset = step as f32 * grid.step;
                let color = if step == 0 {
                    grid.axis_color
                } else {
                    grid.color
                };
                lines.push(Vertex::new([offset, -reach, 0.], color, [0.; 2]));
                lines.push(Vertex::new([offset, reach, 0.], color, [0.; 2]));
                lines.push(Vertex::new([-reach, offset, 0.], color, [0.; 2]));
                lines.push(Vertex::new([reach, offset, 0.], color, [0.; 2]));
            }
        }

        Vertices { points, lines }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cloud(positions: Vec<[f32; 3]>) -> Points {
        Points {
            positions,
            ..Points::default()
        }
    }

    /// A scene of one layer at the origin, with no grid under it.
    fn one(content: Content) -> Scene {
        Scene {
            layers: vec![Layer::new(content)],
            grid: None,
            ..Scene::default()
        }
    }

    #[test]
    fn every_point_becomes_one_quad() {
        let scene = one(Content::Points(cloud(vec![[0., 0., 0.], [1., 1., 1.]])));
        let vertices = scene.vertices();
        assert_eq!(vertices.points.len(), 2 * 6);
        assert!(vertices.lines.is_empty());
    }

    #[test]
    fn a_quad_covers_all_four_corners_around_its_point() {
        let scene = one(Content::Points(cloud(vec![[3., 4., 5.]])));
        let vertices = scene.vertices();
        assert!(
            vertices
                .points
                .iter()
                .all(|vertex| vertex.position == [3., 4., 5.]),
            "the quad is spread in the shader, not in the vertices"
        );
        for corner in [[-1., -1.], [1., -1.], [1., 1.], [-1., 1.]] {
            assert!(
                vertices.points.iter().any(|vertex| vertex.corner == corner),
                "corner {corner:?} is missing"
            );
        }
    }

    #[test]
    fn a_grid_follows_the_size_of_what_it_sits_under() {
        // A 10 cm hand and a 2 m arm must not get the same spacing.
        let hand = Grid::for_size(0.1);
        let arm = Grid::for_size(2.);
        assert!(hand.step < arm.step, "{} vs {}", hand.step, arm.step);
        for size in [0.05f32, 0.5, 2., 30.] {
            let grid = Grid::for_size(size);
            let reach = grid.step * grid.extent as f32;
            assert!(
                reach >= size * 0.9,
                "a grid for {size} m only reached {reach} m"
            );
            assert!(
                (4..=20).contains(&grid.extent),
                "{size} m gave {} lines",
                grid.extent
            );
        }
    }

    #[test]
    fn grid_spacing_snaps_to_numbers_a_person_would_choose() {
        for size in [0.08f32, 0.8, 8., 80.] {
            let step = Grid::for_size(size).step;
            let magnitude = 10f32.powf(step.log10().round());
            let ratio = step / magnitude;
            assert!(
                [0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0]
                    .iter()
                    .any(|nice| (ratio - nice).abs() < 1e-3),
                "{size} m gave a step of {step}"
            );
        }
    }

    #[test]
    fn a_degenerate_size_still_yields_a_usable_grid() {
        let grid = Grid::for_size(0.);
        assert!(grid.step > 0. && grid.step.is_finite());
        assert!(grid.extent >= 4);
    }

    #[test]
    fn the_grid_draws_both_directions_and_marks_the_axes() {
        let grid = Grid {
            step: 1.,
            extent: 2,
            ..Grid::default()
        };
        let scene = Scene {
            grid: Some(grid),
            ..Scene::default()
        };
        let lines = scene.vertices().lines;
        // Five lines each way, two vertices each.
        assert_eq!(lines.len(), 5 * 2 * 2);
        let axis_vertices = lines
            .iter()
            .filter(|vertex| vertex.color == grid.axis_color)
            .count();
        assert_eq!(axis_vertices, 4, "the two lines through the origin");
    }

    #[test]
    fn height_colouring_spans_the_cloud() {
        let points = cloud(vec![[0., 0., 0.], [0., 0., 10.]]);
        let span = points.span();
        assert_eq!(span, (0., 10.));
        let low = points.color(0, span);
        let high = points.color(1, span);
        assert_ne!(
            low, high,
            "the ends of the cloud must not be the same colour"
        );
    }

    #[test]
    fn a_flat_cloud_is_one_colour_rather_than_a_division_by_zero() {
        let points = cloud(vec![[0., 0., 2.], [1., 1., 2.]]);
        let span = points.span();
        let first = points.color(0, span);
        assert_eq!(first, points.color(1, span));
        assert!(first.iter().all(|channel| channel.is_finite()));
    }

    #[test]
    fn packed_colour_is_used_as_given() {
        let points = Points {
            positions: vec![[0., 0., 0.]],
            rgb: Some(vec![[255, 0, 128]]),
            coloring: Coloring::Rgb,
            ..Points::default()
        };
        let color = points.color(0, points.span());
        assert_eq!(color[0], 1.);
        assert_eq!(color[1], 0.);
        assert!((color[2] - 128. / 255.).abs() < 1e-6);
    }

    #[test]
    fn intensity_colouring_uses_its_own_span_not_the_heights() {
        let points = Points {
            positions: vec![[0., 0., 0.], [0., 0., 0.]],
            intensity: Some(vec![100., 200.]),
            coloring: Coloring::Intensity,
            ..Points::default()
        };
        assert_eq!(points.span(), (100., 200.));
        assert_ne!(
            points.color(0, points.span()),
            points.color(1, points.span())
        );
    }

    #[test]
    fn only_the_colourings_the_message_supports_are_offered() {
        assert_eq!(cloud(vec![]).available(), vec![Coloring::Axis]);
        let both = Points {
            intensity: Some(vec![1.]),
            rgb: Some(vec![[1, 2, 3]]),
            ..Points::default()
        };
        assert_eq!(
            both.available(),
            vec![Coloring::Axis, Coloring::Intensity, Coloring::Rgb]
        );
    }

    #[test]
    fn the_ramp_runs_from_one_end_to_the_other_without_leaving_the_range() {
        for step in 0..=20 {
            let color = ramp(step as f32 / 20.);
            assert!(
                color.iter().all(|channel| (0. ..=1.).contains(channel)),
                "{color:?} at step {step}"
            );
        }
        assert_ne!(ramp(0.), ramp(1.));
    }

    #[test]
    fn a_value_outside_the_span_is_clamped_rather_than_wrapped() {
        assert_eq!(normalize(-5., (0., 10.)), 0.);
        assert_eq!(normalize(50., (0., 10.)), 1.);
    }

    #[test]
    fn an_empty_scene_draws_nothing_but_its_grid() {
        let vertices = Scene::default().vertices();
        assert!(vertices.points.is_empty());
        assert!(!vertices.lines.is_empty());
    }

    /// Half a turn about z, then five metres along x — the sort of placement a
    /// transform lookup hands back.
    fn placed() -> Mat4 {
        [
            [0., 1., 0., 0.],
            [-1., 0., 0., 0.],
            [0., 0., 1., 0.],
            [5., 0., 0., 1.],
        ]
    }

    #[test]
    fn a_layers_transform_moves_its_points_without_touching_the_data() {
        let cloud = cloud(vec![[1., 0., 0.]]);
        let scene = Scene {
            layers: vec![Layer::new(Content::Points(cloud.clone())).at(placed())],
            grid: None,
            ..Scene::default()
        };
        let vertices = scene.vertices();
        assert!(
            vertices
                .points
                .iter()
                .all(|vertex| vertex.position == [5., 1., 0.]),
            "got {:?}",
            vertices.points.first().map(|vertex| vertex.position)
        );
        let Content::Points(kept) = &scene.layers[0].content else {
            panic!("the layer still holds points");
        };
        assert_eq!(
            kept.positions, cloud.positions,
            "the layer's own data is left in its own frame"
        );
    }

    #[test]
    fn two_layers_land_in_different_places_from_the_same_geometry() {
        // The whole point of TF, in one assertion: before this, both of these
        // sat on top of one another at the origin.
        let scene = Scene {
            layers: vec![
                Layer::new(Content::Points(cloud(vec![[0., 0., 0.]]))),
                Layer::new(Content::Points(cloud(vec![[0., 0., 0.]]))).at(placed()),
            ],
            grid: None,
            ..Scene::default()
        };
        let positions: Vec<[f32; 3]> = scene
            .vertices()
            .points
            .iter()
            .map(|vertex| vertex.position)
            .collect();
        assert!(positions.contains(&[0., 0., 0.]));
        assert!(positions.contains(&[5., 0., 0.]));
    }

    #[test]
    fn a_solids_layer_composes_its_placement_with_each_solids_own() {
        let solid = crate::Solid {
            key: 7,
            vertices: std::sync::Arc::new(vec![crate::MeshVertex::new(
                [0.; 3],
                [0., 0., 1.],
                [1.; 4],
            )]),
            transform: crate::camera::multiply(
                crate::IDENTITY,
                [
                    [1., 0., 0., 0.],
                    [0., 1., 0., 0.],
                    [0., 0., 1., 0.],
                    [0., 2., 0., 1.],
                ],
            ),
        };
        let scene = Scene {
            layers: vec![Layer::new(Content::Solids(vec![solid])).at(placed())],
            grid: None,
            ..Scene::default()
        };
        let placed_solids = scene.placed_solids();
        assert_eq!(placed_solids.len(), 1);
        let (solid, matrix) = placed_solids[0];
        assert_eq!(
            solid.key, 7,
            "the cache key is untouched, so nothing re-uploads"
        );
        // Two metres along the solid's own y, turned into the layer's frame,
        // then shifted five along x: (5, 0, 0) + (-2, 0, 0).
        assert_eq!(transform_point(matrix, [0.; 3]), [3., 0., 0.]);
    }

    #[test]
    fn a_hidden_or_empty_layer_draws_nothing() {
        let mut hidden = Layer::new(Content::Points(cloud(vec![[1., 2., 3.]])));
        hidden.visible = false;
        let scene = Scene {
            layers: vec![hidden, Layer::new(Content::Points(cloud(vec![])))],
            grid: None,
            ..Scene::default()
        };
        assert!(scene.vertices().points.is_empty());
        assert!(scene.placed_solids().is_empty());
    }

    #[test]
    fn a_line_strip_joins_its_points_and_a_line_list_reads_them_in_pairs() {
        let path = LineSet {
            points: vec![[0., 0., 0.], [1., 0., 0.], [2., 0., 0.]],
            color: [1., 1., 1., 1.],
            strip: true,
        };
        let list = LineSet {
            strip: false,
            ..path.clone()
        };
        // Three points: two joined segments as a strip, one pair as a list with
        // the odd point dropped rather than joined to nothing.
        assert_eq!(one(Content::Lines(vec![path])).vertices().lines.len(), 4);
        assert_eq!(one(Content::Lines(vec![list])).vertices().lines.len(), 2);
    }

    #[test]
    fn an_axis_is_three_coloured_arms_from_its_own_origin() {
        let scene = one(Content::Axes(vec![Axis {
            transform: placed(),
            length: 0.5,
        }]));
        let lines = scene.vertices().lines;
        assert_eq!(lines.len(), 6, "three arms, two vertices each");
        for (index, color) in AXIS_COLORS.iter().enumerate() {
            assert!(
                lines.iter().any(|vertex| vertex.color == *color),
                "axis {index} is missing its colour"
            );
        }
        // Every arm starts at the frame's own origin, wherever that landed.
        let origin = transform_point(placed(), [0.; 3]);
        assert_eq!(
            lines
                .iter()
                .filter(|vertex| vertex.position == origin)
                .count(),
            3
        );
        // And x reaches half a metre along the placed frame's x, which the half
        // turn has pointed down the world's y.
        assert!(
            lines.iter().any(|vertex| vertex.position == [5., 0.5, 0.]),
            "the x arm followed its frame's rotation"
        );
    }

    #[test]
    fn each_cloud_is_coloured_across_its_own_range() {
        // A layer of tall points and a layer of short ones: sharing a span
        // would flatten the short one to a single colour.
        let low = Points {
            positions: vec![[0., 0., 0.], [0., 0., 1.]],
            ..Points::default()
        };
        let high = Points {
            positions: vec![[0., 0., 100.], [0., 0., 200.]],
            ..Points::default()
        };
        let scene = Scene {
            layers: vec![
                Layer::new(Content::Points(low)),
                Layer::new(Content::Points(high)),
            ],
            grid: None,
            ..Scene::default()
        };
        let colors: Vec<[f32; 4]> = scene
            .vertices()
            .points
            .iter()
            .map(|vertex| vertex.color)
            .collect();
        assert_ne!(
            colors[0], colors[6],
            "the low layer spans its own two points"
        );
        assert_ne!(colors[12], colors[18], "and so does the high one");
    }
}
