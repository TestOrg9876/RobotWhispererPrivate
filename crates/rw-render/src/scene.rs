//! What one pane is showing.
//!
//! A scene is plain data: points, a ground grid and a camera. It is turned into
//! vertices only at draw time, so the pane can hand in a new cloud without
//! knowing that a GPU exists.

use crate::Vertex;
use crate::camera::Camera;

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

/// Everything one pane draws.
#[derive(Debug, Clone)]
pub struct Scene {
    pub camera: Camera,
    pub points: Points,
    /// Lit surfaces: robot links, and anything else with a skin.
    pub solids: Vec<crate::Solid>,
    pub grid: Option<Grid>,
    pub background: [f32; 3],
    /// Point diameter, in pixels.
    pub point_size: f32,
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            camera: Camera::default(),
            points: Points::default(),
            solids: Vec::new(),
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
    pub fn vertices(&self) -> Vertices {
        let span = self.points.span();
        let mut points = Vec::with_capacity(self.points.positions.len() * QUAD.len());
        for (index, position) in self.points.positions.iter().enumerate() {
            let color = self.points.color(index, span);
            for corner in QUAD {
                points.push(Vertex::new(*position, color, corner));
            }
        }

        let mut lines = Vec::new();
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

    #[test]
    fn every_point_becomes_one_quad() {
        let scene = Scene {
            points: cloud(vec![[0., 0., 0.], [1., 1., 1.]]),
            grid: None,
            ..Scene::default()
        };
        let vertices = scene.vertices();
        assert_eq!(vertices.points.len(), 2 * 6);
        assert!(vertices.lines.is_empty());
    }

    #[test]
    fn a_quad_covers_all_four_corners_around_its_point() {
        let scene = Scene {
            points: cloud(vec![[3., 4., 5.]]),
            grid: None,
            ..Scene::default()
        };
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
    fn the_grid_draws_both_directions_and_marks_the_axes() {
        let grid = Grid {
            step: 1.,
            extent: 2,
            ..Grid::default()
        };
        let scene = Scene {
            points: Points::default(),
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
}
