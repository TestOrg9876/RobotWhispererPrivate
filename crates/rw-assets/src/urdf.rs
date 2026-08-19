//! Reading a URDF: the links a robot is made of, and the joints between them.
//!
//! Only what a viewer needs is kept — the kinematic tree and the visual
//! geometry. Inertia, collision shapes, transmissions and gazebo tags are
//! read past, because nothing here simulates anything.

use std::collections::HashMap;

use crate::math::{self, Mat4};

/// What a joint is allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JointKind {
    /// Turns about its axis, between limits.
    Revolute,
    /// Turns about its axis, without limits.
    Continuous,
    /// Slides along its axis, between limits.
    Prismatic,
    /// Does not move. Most joints in a description are these.
    Fixed,
}

impl JointKind {
    fn parse(name: &str) -> Self {
        match name {
            "revolute" => Self::Revolute,
            "continuous" => Self::Continuous,
            "prismatic" => Self::Prismatic,
            // `floating` and `planar` are vanishingly rare and have no single
            // value to drive, so they hold still rather than being guessed at.
            _ => Self::Fixed,
        }
    }

    /// Whether this joint has a value a person can move.
    pub fn is_movable(self) -> bool {
        !matches!(self, Self::Fixed)
    }
}

/// One joint of the kinematic tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Joint {
    pub name: String,
    pub kind: JointKind,
    pub parent: String,
    pub child: String,
    /// Where the child frame sits in the parent's, before the joint moves.
    pub origin: Mat4,
    pub axis: [f32; 3],
    /// The travel limits, in radians or metres. `None` for a continuous joint.
    pub limits: Option<(f32, f32)>,
}

impl Joint {
    /// A sensible resting value: zero if the joint can reach it, the nearest
    /// limit otherwise — a joint parked outside its own range looks broken.
    pub fn rest(&self) -> f32 {
        match self.limits {
            Some((lower, upper)) => 0f32.clamp(lower.min(upper), upper.max(lower)),
            None => 0.,
        }
    }

    /// The transform this joint contributes at the given value.
    pub fn motion(&self, value: f32) -> Mat4 {
        match self.kind {
            JointKind::Revolute | JointKind::Continuous => math::from_axis_angle(self.axis, value),
            JointKind::Prismatic => math::translation([
                self.axis[0] * value,
                self.axis[1] * value,
                self.axis[2] * value,
            ]),
            JointKind::Fixed => math::IDENTITY,
        }
    }
}

/// A shape hung off a link.
#[derive(Debug, Clone, PartialEq)]
pub enum Geometry {
    /// A mesh file, named as the URDF names it — resolving `package://` is the
    /// catalog's job, not the parser's.
    Mesh {
        filename: String,
        scale: [f32; 3],
    },
    Box {
        size: [f32; 3],
    },
    Cylinder {
        radius: f32,
        length: f32,
    },
    Sphere {
        radius: f32,
    },
}

/// One drawable piece of a link.
#[derive(Debug, Clone, PartialEq)]
pub struct Visual {
    /// Where the shape sits in the link's frame.
    pub origin: Mat4,
    pub geometry: Geometry,
    /// The colour the description asked for, if it named one.
    pub color: Option<[f32; 4]>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Link {
    pub name: String,
    pub visuals: Vec<Visual>,
}

/// A parsed robot description.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Robot {
    pub name: String,
    pub links: Vec<Link>,
    pub joints: Vec<Joint>,
}

#[derive(Debug, thiserror::Error)]
pub enum UrdfError {
    #[error("not valid XML: {0}")]
    Xml(#[from] roxmltree::Error),
    #[error("no <robot> element")]
    NoRobot,
}

impl Robot {
    /// The link nothing is a child of: where the tree starts.
    ///
    /// A description with several would be malformed, so the first in document
    /// order wins rather than the parse failing — a robot drawn from one of its
    /// roots beats no robot at all.
    pub fn root(&self) -> Option<&Link> {
        let children: Vec<&str> = self
            .joints
            .iter()
            .map(|joint| joint.child.as_str())
            .collect();
        self.links
            .iter()
            .find(|link| !children.contains(&link.name.as_str()))
    }

    /// The joints a person can actually move, in description order.
    pub fn movable(&self) -> impl Iterator<Item = &Joint> {
        self.joints.iter().filter(|joint| joint.kind.is_movable())
    }

    pub fn link(&self, name: &str) -> Option<&Link> {
        self.links.iter().find(|link| link.name == name)
    }
}

/// Reads a URDF document.
pub fn parse(source: &str) -> Result<Robot, UrdfError> {
    let document = roxmltree::Document::parse(source)?;
    let root = document.root_element();
    if root.tag_name().name() != "robot" {
        return Err(UrdfError::NoRobot);
    }

    let mut robot = Robot {
        name: root.attribute("name").unwrap_or_default().to_string(),
        ..Robot::default()
    };

    // Materials may be declared once at the top and referred to by name from
    // any link, which is how most descriptions avoid repeating a colour.
    let mut palette: HashMap<String, [f32; 4]> = HashMap::new();
    for material in root.children().filter(|node| node.has_tag_name("material")) {
        if let (Some(name), Some(color)) = (material.attribute("name"), color_of(material)) {
            palette.insert(name.to_string(), color);
        }
    }

    for node in root.children().filter(|node| node.is_element()) {
        match node.tag_name().name() {
            "link" => robot.links.push(link(node, &palette)),
            "joint" => robot.joints.push(joint(node)),
            _ => {}
        }
    }

    Ok(robot)
}

fn link(node: roxmltree::Node, palette: &HashMap<String, [f32; 4]>) -> Link {
    Link {
        name: node.attribute("name").unwrap_or_default().to_string(),
        visuals: node
            .children()
            .filter(|child| child.has_tag_name("visual"))
            .filter_map(|child| visual(child, palette))
            .collect(),
    }
}

fn visual(node: roxmltree::Node, palette: &HashMap<String, [f32; 4]>) -> Option<Visual> {
    let geometry = node
        .children()
        .find(|child| child.has_tag_name("geometry"))
        .and_then(geometry)?;
    Some(Visual {
        origin: origin_of(node),
        geometry,
        color: node
            .children()
            .find(|child| child.has_tag_name("material"))
            .and_then(|material| {
                // An inline colour wins; otherwise the name refers to one
                // declared at the top of the document.
                color_of(material).or_else(|| {
                    palette
                        .get(material.attribute("name").unwrap_or_default())
                        .copied()
                })
            }),
    })
}

fn geometry(node: roxmltree::Node) -> Option<Geometry> {
    let shape = node.children().find(|child| child.is_element())?;
    Some(match shape.tag_name().name() {
        "mesh" => Geometry::Mesh {
            filename: shape.attribute("filename")?.to_string(),
            scale: triple(shape.attribute("scale")).unwrap_or([1., 1., 1.]),
        },
        "box" => Geometry::Box {
            size: triple(shape.attribute("size"))?,
        },
        "cylinder" => Geometry::Cylinder {
            radius: number(shape.attribute("radius"))?,
            length: number(shape.attribute("length"))?,
        },
        "sphere" => Geometry::Sphere {
            radius: number(shape.attribute("radius"))?,
        },
        _ => return None,
    })
}

fn joint(node: roxmltree::Node) -> Joint {
    let child_named = |tag: &str| node.children().find(|child| child.has_tag_name(tag));
    let kind = JointKind::parse(node.attribute("type").unwrap_or_default());
    let limits = child_named("limit").and_then(|limit| {
        Some((
            number(limit.attribute("lower"))?,
            number(limit.attribute("upper"))?,
        ))
    });
    Joint {
        name: node.attribute("name").unwrap_or_default().to_string(),
        kind,
        parent: child_named("parent")
            .and_then(|parent| parent.attribute("link"))
            .unwrap_or_default()
            .to_string(),
        child: child_named("child")
            .and_then(|child| child.attribute("link"))
            .unwrap_or_default()
            .to_string(),
        origin: origin_of(node),
        // The URDF default when a joint names no axis.
        axis: child_named("axis")
            .and_then(|axis| triple(axis.attribute("xyz")))
            .unwrap_or([1., 0., 0.]),
        // A continuous joint turns without end, so a limit on it is meaningless
        // even when a description carries one.
        limits: (kind != JointKind::Continuous).then_some(limits).flatten(),
    }
}

fn origin_of(node: roxmltree::Node) -> Mat4 {
    let Some(origin) = node.children().find(|child| child.has_tag_name("origin")) else {
        return math::IDENTITY;
    };
    math::from_origin(
        triple(origin.attribute("xyz")).unwrap_or([0.; 3]),
        triple(origin.attribute("rpy")).unwrap_or([0.; 3]),
    )
}

fn color_of(node: roxmltree::Node) -> Option<[f32; 4]> {
    let color = node.children().find(|child| child.has_tag_name("color"))?;
    let mut values = color.attribute("rgba")?.split_whitespace();
    let mut rgba = [1.; 4];
    for slot in rgba.iter_mut() {
        *slot = values.next()?.parse().ok()?;
    }
    Some(rgba)
}

fn triple(raw: Option<&str>) -> Option<[f32; 3]> {
    let mut values = raw?.split_whitespace();
    let mut triple = [0.; 3];
    for slot in triple.iter_mut() {
        *slot = values.next()?.parse().ok()?;
    }
    Some(triple)
}

fn number(raw: Option<&str>) -> Option<f32> {
    raw?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::transform_point;
    use std::f32::consts::FRAC_PI_2;

    const ARM: &str = r#"
      <robot name="arm">
        <material name="house_orange"><color rgba="0.9 0.4 0.1 1"/></material>
        <link name="base">
          <visual>
            <origin xyz="0 0 0.05" rpy="0 0 0"/>
            <geometry><cylinder radius="0.1" length="0.1"/></geometry>
            <material name="house_orange"/>
          </visual>
        </link>
        <link name="upper">
          <visual>
            <geometry><mesh filename="package://arm/meshes/upper.dae" scale="0.001 0.001 0.001"/></geometry>
            <material name="inline"><color rgba="0 0 1 0.5"/></material>
          </visual>
          <collision><geometry><box size="1 1 1"/></geometry></collision>
        </link>
        <link name="tool"/>
        <joint name="shoulder" type="revolute">
          <origin xyz="0 0 0.1" rpy="0 0 0"/>
          <parent link="base"/>
          <child link="upper"/>
          <axis xyz="0 0 1"/>
          <limit lower="-1.5" upper="1.5" effort="100" velocity="2"/>
        </joint>
        <joint name="wrist" type="fixed">
          <parent link="upper"/>
          <child link="tool"/>
        </joint>
      </robot>
    "#;

    #[test]
    fn a_description_yields_its_links_and_joints() {
        let robot = parse(ARM).expect("parses");
        assert_eq!(robot.name, "arm");
        assert_eq!(robot.links.len(), 3);
        assert_eq!(robot.joints.len(), 2);
    }

    #[test]
    fn the_root_is_the_link_no_joint_claims_as_a_child() {
        let robot = parse(ARM).expect("parses");
        assert_eq!(robot.root().map(|link| link.name.as_str()), Some("base"));
    }

    #[test]
    fn only_movable_joints_are_offered_to_be_driven() {
        let robot = parse(ARM).expect("parses");
        let movable: Vec<&str> = robot.movable().map(|joint| joint.name.as_str()).collect();
        assert_eq!(movable, ["shoulder"]);
    }

    #[test]
    fn a_joints_limits_and_axis_come_through() {
        let robot = parse(ARM).expect("parses");
        let shoulder = &robot.joints[0];
        assert_eq!(shoulder.kind, JointKind::Revolute);
        assert_eq!(shoulder.axis, [0., 0., 1.]);
        assert_eq!(shoulder.limits, Some((-1.5, 1.5)));
        assert_eq!(shoulder.parent, "base");
        assert_eq!(shoulder.child, "upper");
    }

    #[test]
    fn a_joint_origin_places_the_child_frame() {
        let robot = parse(ARM).expect("parses");
        let moved = transform_point(robot.joints[0].origin, [0., 0., 0.]);
        assert_eq!(moved, [0., 0., 0.1]);
    }

    #[test]
    fn a_revolute_joint_turns_about_its_axis_by_its_value() {
        let robot = parse(ARM).expect("parses");
        let turned = transform_point(robot.joints[0].motion(FRAC_PI_2), [1., 0., 0.]);
        assert!(turned[1] > 0.99, "got {turned:?}");
    }

    #[test]
    fn a_prismatic_joint_slides_along_its_axis() {
        let robot = parse(
            r#"<robot name="r">
                 <link name="a"/><link name="b"/>
                 <joint name="rail" type="prismatic">
                   <parent link="a"/><child link="b"/>
                   <axis xyz="0 1 0"/>
                   <limit lower="0" upper="0.5"/>
                 </joint>
               </robot>"#,
        )
        .expect("parses");
        let slid = transform_point(robot.joints[0].motion(0.25), [0., 0., 0.]);
        assert_eq!(slid, [0., 0.25, 0.]);
    }

    #[test]
    fn a_continuous_joint_has_no_limits_even_if_the_file_gives_it_some() {
        let robot = parse(
            r#"<robot name="r">
                 <link name="a"/><link name="b"/>
                 <joint name="spin" type="continuous">
                   <parent link="a"/><child link="b"/>
                   <limit lower="-1" upper="1"/>
                 </joint>
               </robot>"#,
        )
        .expect("parses");
        assert_eq!(robot.joints[0].kind, JointKind::Continuous);
        assert_eq!(robot.joints[0].limits, None);
    }

    #[test]
    fn a_joint_that_cannot_reach_zero_rests_at_its_nearest_limit() {
        let joint = Joint {
            name: "x".into(),
            kind: JointKind::Revolute,
            parent: String::new(),
            child: String::new(),
            origin: math::IDENTITY,
            axis: [0., 0., 1.],
            limits: Some((0.5, 2.0)),
        };
        assert_eq!(joint.rest(), 0.5);
    }

    #[test]
    fn mesh_filenames_are_left_as_written_for_the_catalog_to_resolve() {
        let robot = parse(ARM).expect("parses");
        let upper = robot.link("upper").expect("has an upper link");
        assert_eq!(
            upper.visuals[0].geometry,
            Geometry::Mesh {
                filename: "package://arm/meshes/upper.dae".into(),
                scale: [0.001, 0.001, 0.001],
            }
        );
    }

    #[test]
    fn collision_geometry_is_not_drawn() {
        let robot = parse(ARM).expect("parses");
        assert_eq!(
            robot.link("upper").expect("has it").visuals.len(),
            1,
            "the <collision> box must not become a second visual"
        );
    }

    #[test]
    fn a_named_material_is_looked_up_and_an_inline_one_wins() {
        let robot = parse(ARM).expect("parses");
        assert_eq!(
            robot.link("base").expect("has it").visuals[0].color,
            Some([0.9, 0.4, 0.1, 1.])
        );
        assert_eq!(
            robot.link("upper").expect("has it").visuals[0].color,
            Some([0., 0., 1., 0.5])
        );
    }

    #[test]
    fn a_link_with_no_visual_is_still_part_of_the_tree() {
        let robot = parse(ARM).expect("parses");
        let tool = robot.link("tool").expect("has a tool link");
        assert!(tool.visuals.is_empty());
    }

    #[test]
    fn a_joint_with_no_axis_takes_the_urdf_default() {
        let robot = parse(
            r#"<robot name="r">
                 <link name="a"/><link name="b"/>
                 <joint name="j" type="revolute"><parent link="a"/><child link="b"/></joint>
               </robot>"#,
        )
        .expect("parses");
        assert_eq!(robot.joints[0].axis, [1., 0., 0.]);
    }

    #[test]
    fn joint_types_with_no_single_value_hold_still_rather_than_being_guessed() {
        for kind in ["floating", "planar", "nonsense"] {
            assert_eq!(JointKind::parse(kind), JointKind::Fixed, "{kind}");
        }
    }

    #[test]
    fn something_that_is_not_a_robot_is_refused() {
        assert!(matches!(
            parse("<world><link name='a'/></world>"),
            Err(UrdfError::NoRobot)
        ));
        assert!(matches!(parse("not xml at all <<"), Err(UrdfError::Xml(_))));
    }

    #[test]
    fn the_real_ur10e_description_parses() {
        let source = include_str!("../../../assets/ur10e/ur10e.urdf");
        let robot = parse(source).expect("the shipped description parses");
        assert_eq!(robot.name, "ur10e");
        // Six revolute joints is what a UR10e has; the rest are fixed.
        assert_eq!(robot.movable().count(), 6);
        assert_eq!(robot.root().map(|link| link.name.as_str()), Some("world"));
        assert!(
            robot
                .links
                .iter()
                .flat_map(|link| &link.visuals)
                .any(|visual| matches!(visual.geometry, Geometry::Mesh { .. })),
            "the arm is drawn from meshes"
        );
    }
}
