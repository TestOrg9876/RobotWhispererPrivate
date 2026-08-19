//! Where each link ends up, given what the joints are set to.
//!
//! A URDF is a tree of frames: every joint says where its child sits inside its
//! parent, and how that changes with the joint's value. Walking it once from
//! the root gives every link a pose in the robot's own frame.

use std::collections::HashMap;

use crate::math::{self, Mat4};
use crate::urdf::Robot;

/// What every joint is currently set to, by joint name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pose(HashMap<String, f32>);

impl Pose {
    /// Every joint at its resting value: the pose a robot is first drawn in.
    pub fn rest(robot: &Robot) -> Self {
        Self(
            robot
                .movable()
                .map(|joint| (joint.name.clone(), joint.rest()))
                .collect(),
        )
    }

    pub fn get(&self, joint: &str) -> f32 {
        self.0.get(joint).copied().unwrap_or(0.)
    }

    pub fn set(&mut self, joint: &str, value: f32) {
        self.0.insert(joint.to_string(), value);
    }
}

/// The world transform of every link the tree can reach.
///
/// Links a joint chain never arrives at are left out rather than defaulted to
/// the origin, which would pile every stray part on top of the base.
pub fn solve(robot: &Robot, pose: &Pose) -> HashMap<String, Mat4> {
    let mut children: HashMap<&str, Vec<&crate::urdf::Joint>> = HashMap::new();
    for joint in &robot.joints {
        children
            .entry(joint.parent.as_str())
            .or_default()
            .push(joint);
    }

    let mut placed = HashMap::new();
    let Some(root) = robot.root() else {
        return placed;
    };

    // Iterative rather than recursive: a description is user-supplied, and a
    // cycle in one must not take the whole app down with a stack overflow.
    let mut frontier = vec![(root.name.as_str(), math::IDENTITY)];
    while let Some((link, world)) = frontier.pop() {
        if placed.contains_key(link) {
            // Already reached, so this edge closes a cycle. The first arrival
            // wins; going round again would never end.
            continue;
        }
        placed.insert(link.to_string(), world);

        for joint in children.get(link).into_iter().flatten() {
            let local = math::multiply(joint.origin, joint.motion(pose.get(&joint.name)));
            frontier.push((joint.child.as_str(), math::multiply(world, local)));
        }
    }

    placed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::transform_point;
    use crate::urdf;
    use std::f32::consts::FRAC_PI_2;

    const CHAIN: &str = r#"
      <robot name="chain">
        <link name="base"/>
        <link name="arm"/>
        <link name="hand"/>
        <joint name="lift" type="prismatic">
          <origin xyz="0 0 1"/>
          <parent link="base"/><child link="arm"/>
          <axis xyz="0 0 1"/>
          <limit lower="0" upper="1"/>
        </joint>
        <joint name="turn" type="revolute">
          <origin xyz="1 0 0"/>
          <parent link="arm"/><child link="hand"/>
          <axis xyz="0 0 1"/>
          <limit lower="-3" upper="3"/>
        </joint>
      </robot>
    "#;

    fn chain() -> urdf::Robot {
        urdf::parse(CHAIN).expect("parses")
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-5)
    }

    #[test]
    fn the_root_sits_at_the_origin() {
        let robot = chain();
        let placed = solve(&robot, &Pose::rest(&robot));
        assert_eq!(transform_point(placed["base"], [0., 0., 0.]), [0., 0., 0.]);
    }

    #[test]
    fn joint_origins_stack_down_the_chain() {
        let robot = chain();
        let placed = solve(&robot, &Pose::rest(&robot));
        assert!(close(
            transform_point(placed["hand"], [0., 0., 0.]),
            [1., 0., 1.]
        ));
    }

    #[test]
    fn moving_a_joint_moves_everything_below_it() {
        let robot = chain();
        let mut pose = Pose::rest(&robot);
        pose.set("lift", 0.5);
        let placed = solve(&robot, &pose);
        assert!(close(
            transform_point(placed["hand"], [0., 0., 0.]),
            [1., 0., 1.5]
        ));
    }

    #[test]
    fn a_rotation_carries_the_links_below_it_round() {
        let robot = chain();
        let mut pose = Pose::rest(&robot);
        pose.set("turn", FRAC_PI_2);
        let placed = solve(&robot, &pose);
        // The hand's own origin does not move — the joint is at its frame — but
        // a point held out in front of it swings a quarter turn.
        assert!(close(
            transform_point(placed["hand"], [1., 0., 0.]),
            [1., 1., 1.]
        ));
    }

    #[test]
    fn a_joint_above_carries_the_rotation_of_the_one_below() {
        let robot = chain();
        let mut pose = Pose::rest(&robot);
        pose.set("lift", 0.25);
        pose.set("turn", FRAC_PI_2);
        let placed = solve(&robot, &pose);
        assert!(close(
            transform_point(placed["hand"], [1., 0., 0.]),
            [1., 1., 1.25]
        ));
    }

    #[test]
    fn every_link_of_the_real_ur10e_is_placed() {
        let robot = urdf::parse(include_str!("../../../assets/ur10e/ur10e.urdf")).expect("parses");
        let placed = solve(&robot, &Pose::rest(&robot));
        for link in &robot.links {
            assert!(
                placed.contains_key(&link.name),
                "{} was left out",
                link.name
            );
        }
    }

    #[test]
    fn turning_the_ur10e_shoulder_moves_its_wrist() {
        let robot = urdf::parse(include_str!("../../../assets/ur10e/ur10e.urdf")).expect("parses");
        let rest = solve(&robot, &Pose::rest(&robot));
        let mut pose = Pose::rest(&robot);
        let shoulder = robot.movable().next().expect("has a first joint");
        pose.set(&shoulder.name, 1.0);
        let turned = solve(&robot, &pose);

        let name = "wrist_3_link";
        let before = transform_point(rest[name], [0., 0., 0.]);
        let after = transform_point(turned[name], [0., 0., 0.]);
        assert!(
            before
                .iter()
                .zip(after)
                .any(|(before, after)| (before - after).abs() > 0.01),
            "the wrist did not move: {before:?} → {after:?}"
        );
    }

    #[test]
    fn a_cycle_in_a_description_ends_rather_than_spinning() {
        let robot = urdf::parse(
            r#"<robot name="loop">
                 <link name="a"/><link name="b"/><link name="c"/>
                 <joint name="j1" type="fixed"><parent link="a"/><child link="b"/></joint>
                 <joint name="j2" type="fixed"><parent link="b"/><child link="c"/></joint>
                 <joint name="j3" type="fixed"><parent link="c"/><child link="b"/></joint>
               </robot>"#,
        )
        .expect("parses");
        let placed = solve(&robot, &Pose::rest(&robot));
        assert_eq!(placed.len(), 3);
    }

    #[test]
    fn a_link_nothing_connects_to_is_left_out_rather_than_stacked_on_the_base() {
        let robot = urdf::parse(
            r#"<robot name="stray">
                 <link name="a"/><link name="b"/><link name="orphan"/>
                 <joint name="j" type="fixed"><parent link="a"/><child link="b"/></joint>
               </robot>"#,
        )
        .expect("parses");
        let placed = solve(&robot, &Pose::rest(&robot));
        assert!(placed.contains_key("a") && placed.contains_key("b"));
        assert!(!placed.contains_key("orphan"));
    }

    #[test]
    fn a_description_with_no_links_solves_to_nothing() {
        let robot = urdf::parse(r#"<robot name="empty"/>"#).expect("parses");
        assert!(solve(&robot, &Pose::rest(&robot)).is_empty());
    }

    #[test]
    fn the_resting_pose_respects_limits() {
        let robot = urdf::parse(
            r#"<robot name="r">
                 <link name="a"/><link name="b"/>
                 <joint name="j" type="revolute">
                   <parent link="a"/><child link="b"/>
                   <limit lower="1" upper="2"/>
                 </joint>
               </robot>"#,
        )
        .expect("parses");
        assert_eq!(Pose::rest(&robot).get("j"), 1.);
    }
}
