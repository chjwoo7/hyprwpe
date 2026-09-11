//! CPU skinning for puppet models.
//!
//! A puppet's mesh is authored in a rest pose and bound to a skeleton. To draw
//! it animated we need, per bone, a matrix that carries the rest pose to the
//! animated pose; then each vertex is the weighted blend of its influences
//! through those matrices (linear blend skinning).
//!
//! Everything here was pinned from the bytes (see `docs/FORMATS.md`):
//!
//! * a bone's stored matrix is its **local** transform, relative to its parent.
//!   Composing it along the parent chain gives the rest world matrix — verified
//!   geometrically: composed, each vertex's dominant bone lands about 3x closer
//!   to it than if the stored matrices were read as absolute.
//! * an animation keyframe is the bone's local transform at that instant, in the
//!   same space, and **frame 0 equals the bind pose**. So the rest pose yields
//!   the identity skin and the mesh is undeformed at t=0.
//!
//! With those two facts the skinning matrix is the textbook
//! `animated_world * inverse(rest_world)`, and no per-model tuning is needed.

use crate::scene_layer::Mat4;
use hyprwpe_core::mdlv::{Mesh, PuppetAnimation, PuppetModel};

/// Rest-pose transforms and the bone hierarchy of a model.
///
/// Building this is the only costly part (an inverse per bone); it depends only
/// on the model, so keep one per loaded puppet and reuse it for every frame.
pub struct Rig {
    /// Rest-pose local matrix per bone, in bone order.
    rest_local: Vec<Mat4>,
    /// Rest-pose world matrix per bone, in bone order.
    rest_world: Vec<Mat4>,
    parents: Vec<i32>,
}

impl Rig {
    pub fn new(model: &PuppetModel) -> Self {
        let parents: Vec<i32> = model.bones.iter().map(|b| b.parent).collect();
        let rest_local: Vec<Mat4> = model
            .bones
            .iter()
            .map(|b| Mat4::from_row_vector(&b.bind))
            .collect();
        let rest_world = compose_world(&parents, &rest_local);
        Rig {
            rest_local,
            rest_world,
            parents,
        }
    }

    pub fn bone_count(&self) -> usize {
        self.rest_world.len()
    }

    /// The bind (rest) world matrix of every bone — the skeleton as authored.
    pub fn rest_world(&self) -> &[Mat4] {
        &self.rest_world
    }

    /// Skinning matrices for one instant. `anim` selects an animation; `None`
    /// (or an out-of-range index) gives the identity pose, so the mesh renders as
    /// authored.
    ///
    /// A bone the animation does not drive keeps its rest transform, which makes
    /// the pose correct even when a track count is short of the bone count.
    pub fn pose(&self, anim: Option<&PuppetAnimation>, time: f32) -> Vec<Mat4> {
        let Some(a) = anim else {
            return vec![Mat4::identity(); self.rest_world.len()];
        };
        let sampled = a.sample(time);
        let local: Vec<Mat4> = self
            .rest_local
            .iter()
            .enumerate()
            .map(|(i, rest)| match sampled.get(i) {
                Some(k) => Mat4::from_trs(k.position, k.rotation, k.scale),
                None => *rest,
            })
            .collect();

        let animated_world = compose_world(&self.parents, &local);
        // skin = animated_world * inverse(rest_world): carry the rest pose back to
        // the bone's local frame, then out through the animated frame.
        self.rest_world
            .iter()
            .zip(animated_world.iter())
            .map(|(rest, anim)| anim.mul(&rest.inverse_affine()))
            .collect()
    }
}

// ---------------------------------------------------------------- composition

/// Compose each bone's local matrix with its ancestors' to get world matrices.
///
/// Bones are usually stored parent-first, but nothing guarantees it, so this
/// resolves by parent index (memoized, cycle-guarded) rather than by order.
fn compose_world(parents: &[i32], local: &[Mat4]) -> Vec<Mat4> {
    let n = parents.len();
    let mut out: Vec<Option<Mat4>> = vec![None; n];

    fn resolve(
        i: usize,
        parents: &[i32],
        local: &[Mat4],
        out: &mut [Option<Mat4>],
        depth: u32,
    ) -> Mat4 {
        if let Some(m) = out[i] {
            return m;
        }
        let parent = parents[i];
        // A negative parent, an out-of-range index, a self-loop or a cycle stops
        // the walk at this bone's own local transform.
        let m = if parent < 0 || depth > parents.len() as u32 {
            local[i]
        } else {
            let p = parent as usize;
            if p >= parents.len() || p == i {
                local[i]
            } else {
                resolve(p, parents, local, out, depth + 1).mul(&local[i])
            }
        };
        out[i] = Some(m);
        m
    }

    for i in 0..n {
        resolve(i, parents, local, &mut out, 0);
    }
    out.into_iter()
        .map(|m| m.unwrap_or_else(Mat4::identity))
        .collect()
}

/// Deform a mesh's rest positions by a pose.
///
/// Each vertex is the weighted sum of its influences through the per-bone
/// matrices. An unskinned mesh (no influences) passes through unchanged, as does
/// a vertex whose weights sum to zero — better a still vertex than a NaN one.
pub fn deform(mesh: &Mesh, pose: &[Mat4], out: &mut Vec<[f32; 2]>) {
    out.clear();
    out.reserve(mesh.positions.len());

    if !mesh.is_skinned() {
        out.extend(mesh.positions.iter().map(|p| [p[0], p[1]]));
        return;
    }

    for (i, p) in mesh.positions.iter().enumerate() {
        let mut acc = [0.0f32; 3];
        let mut total = 0.0f32;
        for inf in &mesh.skin[i] {
            if !inf.is_used() {
                continue;
            }
            let Some(m) = pose.get(inf.bone as usize) else {
                continue;
            };
            let t = m.transform_point3(*p);
            acc[0] += t[0] * inf.weight;
            acc[1] += t[1] * inf.weight;
            acc[2] += t[2] * inf.weight;
            total += inf.weight;
        }
        if total > 1e-6 {
            out.push([acc[0] / total, acc[1] / total]);
        } else {
            out.push([p[0], p[1]]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprwpe_core::mdlv::{Bone, Influence, Keyframe, Track};

    fn inf(bone: u32, weight: f32) -> [Influence; 4] {
        [
            Influence { bone, weight },
            Influence::default(),
            Influence::default(),
            Influence::default(),
        ]
    }

    /// A two-bone rig: bone 0 translated +10x, bone 1 +5y relative to it.
    fn two_bone_model() -> PuppetModel {
        let bind0 = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [10.0, 0.0, 0.0, 1.0],
        ];
        let bind1 = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 5.0, 0.0, 1.0],
        ];
        let mesh = Mesh {
            positions: vec![[15.0, 0.0, 0.0], [10.0, 5.0, 0.0]],
            uvs: vec![[0.0, 0.0], [1.0, 1.0]],
            indices: vec![0, 1, 0],
            stride: 80,
            skin: vec![inf(0, 1.0), inf(1, 1.0)],
        };
        PuppetModel {
            version: 23,
            material: String::new(),
            mesh,
            bones: vec![
                Bone {
                    parent: -1,
                    bind: bind0,
                    params: String::new(),
                },
                Bone {
                    parent: 0,
                    bind: bind1,
                    params: String::new(),
                },
            ],
            attachments: Vec::new(),
            animations: Vec::new(),
        }
    }

    fn key(pos: [f32; 3], rot: f32) -> Keyframe {
        Keyframe {
            position: pos,
            rotation: rot,
            scale: [1.0, 1.0, 1.0],
        }
    }

    fn anim(tracks: Vec<Track>) -> PuppetAnimation {
        PuppetAnimation {
            name: "a".into(),
            mode: "single".into(),
            fps: 1.0,
            length: 1,
            tracks,
        }
    }

    #[test]
    fn rest_pose_leaves_the_mesh_undeformed() {
        let model = two_bone_model();
        let rig = Rig::new(&model);
        let pose = rig.pose(None, 0.0);
        let mut out = Vec::new();
        deform(&model.mesh, &pose, &mut out);
        assert_eq!(out, vec![[15.0, 0.0], [10.0, 5.0]]);
    }

    #[test]
    fn frame_zero_of_an_animation_matches_the_bind_pose() {
        // Frame 0 keys equal the bind pose, so the skin is the identity.
        let mut model = two_bone_model();
        model.animations.push(anim(vec![
            Track {
                keys: vec![key([10.0, 0.0, 0.0], 0.0), key([10.0, 0.0, 0.0], 0.0)],
            },
            Track {
                keys: vec![key([0.0, 5.0, 0.0], 0.0), key([0.0, 5.0, 0.0], 0.0)],
            },
        ]));
        let rig = Rig::new(&model);
        let pose = rig.pose(model.animations.first(), 0.0);
        let mut out = Vec::new();
        deform(&model.mesh, &pose, &mut out);
        assert_eq!(out, vec![[15.0, 0.0], [10.0, 5.0]]);
    }

    #[test]
    fn rotating_the_root_bone_carries_its_vertices_and_children() {
        // At frame 1 bone 0 rotates 90 degrees about its own origin; bone 1 rides
        // along. The vertex on bone 0 must swing from +x to +y.
        let mut model = two_bone_model();
        model.animations.push(anim(vec![
            Track {
                keys: vec![
                    key([10.0, 0.0, 0.0], 0.0),
                    key([10.0, 0.0, 0.0], std::f32::consts::FRAC_PI_2),
                ],
            },
            Track {
                keys: vec![key([0.0, 5.0, 0.0], 0.0), key([0.0, 5.0, 0.0], 0.0)],
            },
        ]));
        let rig = Rig::new(&model);
        let pose = rig.pose(model.animations.first(), 1.0);
        let mut out = Vec::new();
        deform(&model.mesh, &pose, &mut out);
        // The bone origin rests at (10, 0). A 90-degree turn carries the vertex
        // sitting 5 units along +x of it, (15, 0), round to (10, 5).
        let d0 = ((out[0][0] - 10.0).powi(2) + (out[0][1] - 5.0).powi(2)).sqrt();
        assert!(d0 < 1e-3, "vertex on bone 0 = {:?}", out[0]);
        // The child bone's vertex is dragged by the rotated parent.
        let moved = ((out[1][0] - 10.0).powi(2) + (out[1][1] - 5.0).powi(2)).sqrt();
        assert!(
            moved > 1.0,
            "child vertex should be dragged, got {:?}",
            out[1]
        );
    }

    #[test]
    fn an_unskinned_mesh_passes_through() {
        let mut model = two_bone_model();
        model.mesh.skin.clear();
        let pose = vec![Mat4::identity(); 2];
        let mut out = Vec::new();
        deform(&model.mesh, &pose, &mut out);
        assert_eq!(out, vec![[15.0, 0.0], [10.0, 5.0]]);
    }

    #[test]
    fn matrix_helpers_are_consistent() {
        // The stored row-vector matrix (translation in the last row) equals the
        // TRS form for a -90-degree rotation about +z translated to (7, 3).
        let stored = [
            [0.0, -1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [7.0, 3.0, 0.0, 1.0],
        ];
        let m = Mat4::from_row_vector(&stored);
        let via_trs = Mat4::from_trs(
            [7.0, 3.0, 0.0],
            -std::f32::consts::FRAC_PI_2,
            [1.0, 1.0, 1.0],
        );
        for (a, b) in m.0.iter().zip(via_trs.0.iter()) {
            assert!((a - b).abs() < 1e-5, "{:?} vs {:?}", m.0, via_trs.0);
        }
        // inverse(m) * m == identity.
        let p = m.mul(&m.inverse_affine());
        let id = Mat4::identity();
        for (a, b) in p.0.iter().zip(id.0.iter()) {
            assert!((a - b).abs() < 1e-4, "{:?}", p.0);
        }
        // A degenerate (singular) matrix falls back to identity, not NaN.
        let bad = Mat4::from_trs([0.0, 0.0, 0.0], 0.0, [0.0, 0.0, 0.0]);
        assert_eq!(bad.inverse_affine().0, Mat4::identity().0);
    }
}
