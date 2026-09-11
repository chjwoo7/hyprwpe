//! Pure 2D geometry for the scene renderer.
//!
//! The scene model, validated against real packages (see
//! `docs/SCENE-COVERAGE.md`): a scene draws inside a **design canvas** named by
//! `general.orthogonalprojection {width, height}`, with `(0, 0)` at the
//! bottom-left and +Y up. An object's `origin` is the **centre of its quad** and
//! the quad spans `size * scale` design units. Objects may name a `parent`; a
//! child's transform is composed with its ancestors' (405 of 1450 objects in the
//! local corpus), so a child `origin` is relative to the parent, scaled and
//! rotated by it.
//!
//! Everything here is arithmetic with no GL or Wayland dependency, so the
//! placement rules — where off-by-one and aspect-ratio mistakes live — are
//! unit-testable and shared verbatim by the GPU renderer and the software
//! compositing harness.

use crate::scaling::Scaling;
use hyprwpe_core::animation::Animation;
use hyprwpe_core::scene::{Scene, SceneObject};
use std::collections::HashMap;

/// A 2D affine transform: `x' = a*x + c*y + e`, `y' = b*x + d*y + f`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Affine {
    pub const IDENTITY: Affine = Affine {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub fn translate(x: f32, y: f32) -> Self {
        Affine {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: x,
            f: y,
        }
    }

    pub fn scale(sx: f32, sy: f32) -> Self {
        Affine {
            a: sx,
            b: 0.0,
            c: 0.0,
            d: sy,
            e: 0.0,
            f: 0.0,
        }
    }

    pub fn rotate(rad: f32) -> Self {
        let (s, c) = (rad.sin(), rad.cos());
        Affine {
            a: c,
            b: s,
            c: -s,
            d: c,
            e: 0.0,
            f: 0.0,
        }
    }

    /// Composition: the result applies `rhs` first, then `self`.
    pub fn mul(&self, rhs: &Affine) -> Affine {
        Affine {
            a: self.a * rhs.a + self.c * rhs.b,
            b: self.b * rhs.a + self.d * rhs.b,
            c: self.a * rhs.c + self.c * rhs.d,
            d: self.b * rhs.c + self.d * rhs.d,
            e: self.a * rhs.e + self.c * rhs.f + self.e,
            f: self.b * rhs.e + self.d * rhs.f + self.f,
        }
    }

    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// The linear part scaled per axis — used to fold a half-extent into the
    /// transform so a `-1..1` quad becomes `size` units wide.
    pub fn scale_linear(&self, sx: f32, sy: f32) -> Affine {
        Affine {
            a: self.a * sx,
            b: self.b * sx,
            c: self.c * sy,
            d: self.d * sy,
            e: self.e,
            f: self.f,
        }
    }

    /// The inverse transform, or `None` when it is singular (a zero scale),
    /// which a caller must treat as "nothing to draw" rather than as a bug.
    pub fn invert(&self) -> Option<Affine> {
        let det = self.a * self.d - self.b * self.c;
        if !det.is_finite() || det.abs() < 1e-12 {
            return None;
        }
        let inv = 1.0 / det;
        let a = self.d * inv;
        let b = -self.b * inv;
        let c = -self.c * inv;
        let d = self.a * inv;
        Some(Affine {
            a,
            b,
            c,
            d,
            e: -(a * self.e + c * self.f),
            f: -(b * self.e + d * self.f),
        })
    }
}

/// The object's own transform: `translate(origin) * rotate(angles.z) * scale(scale)`.
pub fn local_transform(obj: &SceneObject) -> Affine {
    let origin = obj.origin();
    let scale = obj.scale();
    let angles = obj.angles();
    Affine::translate(origin[0], origin[1])
        .mul(&Affine::rotate(angles[2].to_radians()))
        .mul(&Affine::scale(scale[0], scale[1]))
}

/// The object's own transform with its animated properties applied at `t`
/// seconds. A property without an animation falls back to its base value.
pub fn animated_local(
    obj: &SceneObject,
    animations: &HashMap<String, Animation>,
    t: f32,
) -> Affine {
    let origin = anim_vec3(animations, "origin", t).unwrap_or_else(|| obj.origin());
    let scale = anim_vec3(animations, "scale", t).unwrap_or_else(|| obj.scale());
    let angles = anim_vec3(animations, "angles", t).unwrap_or_else(|| obj.angles());
    Affine::translate(origin[0], origin[1])
        .mul(&Affine::rotate(angles[2].to_radians()))
        .mul(&Affine::scale(scale[0], scale[1]))
}

fn anim_vec3(animations: &HashMap<String, Animation>, key: &str, t: f32) -> Option<[f32; 3]> {
    animations.get(key).and_then(|a| a.sample_vec3(t))
}

/// The object's alpha with its animation applied at `t` seconds.
pub fn animated_alpha(obj: &SceneObject, animations: &HashMap<String, Animation>, t: f32) -> f32 {
    animations
        .get("alpha")
        .and_then(|a| a.sample(t))
        .unwrap_or_else(|| obj.alpha())
}

/// World transform of every object at `t` seconds, composing `parent` chains and
/// applying each object's property animations.
pub fn animated_world_transforms(
    objects: &[SceneObject],
    animations: &[HashMap<String, Animation>],
    t: f32,
) -> Vec<Affine> {
    let index = index_by_id(objects);
    let local = |i: usize| {
        let anims = animations.get(i);
        match anims {
            Some(a) => animated_local(&objects[i], a, t),
            None => local_transform(&objects[i]),
        }
    };
    compose(objects, &index, &local)
}

/// World transform of every object, composing `parent` chains.
///
/// A missing or self-referential parent falls back to the object's local
/// transform, and a cycle is broken rather than recursed into: a malformed
/// package must degrade to a wrong-looking layer, never to a hang.
pub fn world_transforms(objects: &[SceneObject]) -> Vec<Affine> {
    let index = index_by_id(objects);
    let local = |i: usize| local_transform(&objects[i]);
    compose(objects, &index, &local)
}

fn index_by_id(objects: &[SceneObject]) -> HashMap<u32, usize> {
    let mut index: HashMap<u32, usize> = HashMap::new();
    for (i, obj) in objects.iter().enumerate() {
        if let Some(id) = obj.id {
            index.entry(id).or_insert(i);
        }
    }
    index
}

fn compose(
    objects: &[SceneObject],
    index: &HashMap<u32, usize>,
    local: &dyn Fn(usize) -> Affine,
) -> Vec<Affine> {
    let mut out = vec![Affine::IDENTITY; objects.len()];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = world_of(i, objects, index, local, &mut Vec::new());
    }
    out
}

fn world_of(
    i: usize,
    objects: &[SceneObject],
    index: &HashMap<u32, usize>,
    local: &dyn Fn(usize) -> Affine,
    stack: &mut Vec<usize>,
) -> Affine {
    if stack.contains(&i) {
        return Affine::IDENTITY;
    }
    stack.push(i);
    let world = match objects[i].parent.and_then(|p| index.get(&p).copied()) {
        Some(pi) if pi != i => world_of(pi, objects, index, local, stack).mul(&local(i)),
        _ => local(i),
    };
    stack.pop();
    world
}

/// Where the design canvas lands on the output surface, in output pixels.
///
/// `sx`/`sy` are usually equal; only [`Scaling::Stretch`] splits them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CanvasMap {
    pub x: f32,
    pub y: f32,
    pub sx: f32,
    pub sy: f32,
}

impl CanvasMap {
    /// The rectangle the canvas occupies: `(left, right, bottom, top)`.
    pub fn bounds(&self, design: (f32, f32)) -> (f32, f32, f32, f32) {
        (
            self.x,
            self.x + design.0 * self.sx,
            self.y,
            self.y + design.1 * self.sy,
        )
    }
}

/// The design-space rectangle that the **output surface** covers, i.e. the
/// window the orthographic projection must use.
///
/// This is not the canvas' own extent: when a scaling mode makes the canvas
/// larger than the output (`Fill` on a mismatched aspect), the canvas overflows
/// and only its centre is visible. Projecting the canvas' *extent* instead would
/// leave the overflow margin unpainted — the canvas quad would stop short of the
/// viewport edges and the clear colour would show as bands around it. Inverting
/// the map for the two output corners gives the correct window; the canvas is
/// then cropped to it by the viewport, which is exactly what "cover" means.
pub fn view_window(map: &CanvasMap, out_w: f32, out_h: f32) -> (f32, f32, f32, f32) {
    if !map.sx.is_finite() || !map.sy.is_finite() || map.sx.abs() < 1e-9 || map.sy.abs() < 1e-9 {
        return (0.0, out_w, 0.0, out_h);
    }
    (
        (0.0 - map.x) / map.sx,
        (out_w - map.x) / map.sx,
        (0.0 - map.y) / map.sy,
        (out_h - map.y) / map.sy,
    )
}

/// Map a scene's design canvas onto an output surface.
///
/// Follows the same [`Scaling`] contract as image and video wallpapers, so a
/// scene honours the per-output mode the user picked: `Fill` covers (crop),
/// `Fit` letterboxes, `Stretch` matches both axes, `Center` is 1:1. `zoom`
/// (from `general.zoom`) multiplies the scale, zooming into the canvas centre.
///
/// A scene with no `orthogonalprojection` has no design canvas: design units
/// are output pixels, and the map is the identity.
pub fn canvas_map(
    design: Option<(f32, f32)>,
    zoom: f32,
    out_w: f32,
    out_h: f32,
    scaling: Scaling,
) -> CanvasMap {
    let identity = CanvasMap {
        x: 0.0,
        y: 0.0,
        sx: 1.0,
        sy: 1.0,
    };
    let Some((dw, dh)) = design else {
        return identity;
    };
    if !(dw > 0.0 && dh > 0.0 && out_w > 0.0 && out_h > 0.0) {
        return identity;
    }
    let zoom = if zoom.is_finite() && zoom > 0.0 {
        zoom
    } else {
        1.0
    };

    let (sx, sy) = match scaling {
        Scaling::Stretch => (out_w / dw, out_h / dh),
        Scaling::Center => (1.0, 1.0),
        Scaling::Fill => {
            let s = (out_w / dw).max(out_h / dh);
            (s, s)
        }
        Scaling::Fit => {
            let s = (out_w / dw).min(out_h / dh);
            (s, s)
        }
    };
    let sx = sx * zoom;
    let sy = sy * zoom;

    CanvasMap {
        x: (out_w - dw * sx) / 2.0,
        y: (out_h - dh * sy) / 2.0,
        sx,
        sy,
    }
}

/// The colour a scene asks to be painted behind its layers.
///
/// `general.clearenabled` false means "paint nothing"; for a wallpaper that
/// still means black, because undefined framebuffer contents read as
/// corruption. An absent `clearenabled` counts as enabled, matching the
/// packages in the local corpus that set it.
pub fn clear_color(scene: &Scene) -> [f32; 4] {
    let general = scene.general.as_ref();
    let enabled = general.and_then(|g| g.clearenabled).unwrap_or(true);
    match general.and_then(|g| g.clearcolor) {
        Some(c) if enabled => [c.x(), c.y(), c.z(), 1.0],
        _ => [0.0, 0.0, 0.0, 1.0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scene_objects(v: serde_json::Value) -> Vec<SceneObject> {
        let scene = Scene::from_json_str(&json!({ "objects": v }).to_string()).unwrap();
        scene.objects
    }

    #[test]
    fn affine_composition_orders_rhs_first() {
        // Scale a point by 2, then move it +10 in x.
        let m = Affine::translate(10.0, 0.0).mul(&Affine::scale(2.0, 2.0));
        assert_eq!(m.apply(3.0, 4.0), (16.0, 8.0));
    }

    #[test]
    fn local_transform_is_translate_rotate_scale() {
        let objs = scene_objects(json!([{
            "origin": "100 50 0",
            "scale": "2 3 1",
            "angles": "0 0 90"
        }]));
        let t = local_transform(&objs[0]);
        // A point 1 unit to the right, rotated 90 degrees, scaled x2 -> +y.
        let (x, y) = t.apply(1.0, 0.0);
        assert!((x - 100.0).abs() < 1e-4, "{x}");
        assert!((y - 52.0).abs() < 1e-4, "{y}");
        assert!((t.e - 100.0).abs() < 1e-4);
        assert!((t.f - 50.0).abs() < 1e-4);
    }

    #[test]
    fn child_position_is_relative_to_parent() {
        // Parent 599 in the Akali scene: origin (1935.7, 774.3), scale (2, 3.77).
        // Its child 遮罩云b1 is authored at (-24.7, 85.8) and must land near the
        // canvas centre, not at the bottom-left corner.
        let objs = scene_objects(json!([
            { "id": 599, "origin": "1935.675 774.258 0", "scale": "2 3.76589 2" },
            { "id": 927, "parent": 599, "origin": "-24.698 85.835 0", "scale": "0.81 0.86 1" }
        ]));
        let world = world_transforms(&objs);
        let (x, y) = world[1].apply(0.0, 0.0);
        assert!((x - 1886.28).abs() < 0.5, "x={x}");
        assert!((y - 1097.53).abs() < 0.5, "y={y}");
        // Scale composes too: 0.81 * 2 in x, 0.86 * 3.76589 in y.
        assert!((world[1].a - 1.62).abs() < 1e-3, "a={}", world[1].a);
        assert!((world[1].d - 3.2386).abs() < 1e-3, "d={}", world[1].d);
    }

    #[test]
    fn grandchild_composes_through_every_ancestor() {
        let objs = scene_objects(json!([
            { "id": 1, "origin": "100 100 0", "scale": "2 2 1" },
            { "id": 2, "parent": 1, "origin": "10 0 0", "scale": "3 3 1" },
            { "id": 3, "parent": 2, "origin": "1 0 0" }
        ]));
        let world = world_transforms(&objs);
        let (x, _) = world[2].apply(0.0, 0.0);
        // 100 + 2*10 + 2*3*1 = 126
        assert!((x - 126.0).abs() < 1e-3, "x={x}");
        assert!((world[2].a - 6.0).abs() < 1e-3, "a={}", world[2].a);
    }

    #[test]
    fn dangling_and_self_parents_do_not_recurse_forever() {
        let objs = scene_objects(json!([
            { "id": 1, "parent": 999, "origin": "5 5 0" },
            { "id": 2, "parent": 2, "origin": "7 7 0" }
        ]));
        let world = world_transforms(&objs);
        assert_eq!(world[0].apply(0.0, 0.0), (5.0, 5.0));
        assert_eq!(world[1].apply(0.0, 0.0), (7.0, 7.0));
    }

    #[test]
    fn invert_round_trips_and_rejects_singular_transforms() {
        let m = Affine::translate(100.0, -50.0)
            .mul(&Affine::rotate(0.7))
            .mul(&Affine::scale(3.0, 4.0));
        let inv = m.invert().expect("invertible");
        let (x, y) = m.apply(11.0, -7.0);
        let (bx, by) = inv.apply(x, y);
        assert!((bx - 11.0).abs() < 1e-3, "{bx}");
        assert!((by + 7.0).abs() < 1e-3, "{by}");
        assert!(Affine::scale(0.0, 1.0).invert().is_none());
    }

    #[test]
    fn clear_colour_follows_general() {
        let scene = Scene::from_json_str(
            &json!({ "general": { "clearenabled": true, "clearcolor": "0.7 0.7 0.7" } })
                .to_string(),
        )
        .unwrap();
        assert_eq!(clear_color(&scene), [0.7, 0.7, 0.7, 1.0]);

        // Explicitly disabled: never paint the declared colour.
        let scene = Scene::from_json_str(
            &json!({ "general": { "clearenabled": false, "clearcolor": "0.7 0.7 0.7" } })
                .to_string(),
        )
        .unwrap();
        assert_eq!(clear_color(&scene), [0.0, 0.0, 0.0, 1.0]);

        // Absent general entirely: black, never undefined.
        assert_eq!(clear_color(&Scene::default()), [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn view_window_covers_output_not_canvas_extent() {
        // The regression this pins: `Fill` scales a 16:9 canvas onto a 16:10
        // output, so the canvas overflows horizontally. The projection window
        // must be the visible middle of the canvas, not the whole canvas — a
        // whole-canvas window leaves bands of clear colour on the sides.
        let m = canvas_map(Some((3840.0, 2160.0)), 1.0, 4096.0, 2560.0, Scaling::Fill);
        assert!(m.sx > 1.0, "canvas must overflow: {m:?}");
        let (l, r, b, t) = view_window(&m, 4096.0, 2560.0);
        assert!(l > 0.0 && r < 3840.0, "window must crop inside the canvas: {l} {r}");
        // 2560/2160 = 1.1852 -> the visible design width is 4096/1.1852 = 3455.6
        assert!((r - l - 3455.6).abs() < 2.0, "visible width {} ", r - l);
        // The canvas fits vertically, so the window spans the full height.
        assert!(b.abs() < 1e-3 && (t - 2160.0).abs() < 1e-3, "b={b} t={t}");
    }

    #[test]
    fn view_window_equals_canvas_when_aspect_matches() {
        let m = canvas_map(Some((3840.0, 2160.0)), 1.0, 1920.0, 1080.0, Scaling::Fill);
        let (l, r, b, t) = view_window(&m, 1920.0, 1080.0);
        assert_eq!((l, r, b, t), (0.0, 3840.0, 0.0, 2160.0));
    }

    #[test]
    fn view_window_is_identity_without_a_canvas() {
        let m = canvas_map(None, 1.0, 1920.0, 1080.0, Scaling::Fill);
        assert_eq!(view_window(&m, 1920.0, 1080.0), (0.0, 1920.0, 0.0, 1080.0));
    }

    #[test]
    fn view_window_never_panics_on_degenerate_map() {
        let m = CanvasMap {
            x: 0.0,
            y: 0.0,
            sx: 0.0,
            sy: 0.0,
        };
        assert_eq!(view_window(&m, 800.0, 600.0), (0.0, 800.0, 0.0, 600.0));
    }

    #[test]
    fn no_design_canvas_is_identity() {
        let m = canvas_map(None, 1.0, 1920.0, 1080.0, Scaling::Fill);
        assert_eq!(m, CanvasMap { x: 0.0, y: 0.0, sx: 1.0, sy: 1.0 });
    }

    #[test]
    fn fill_covers_and_centres_the_overflow() {
        // A 16:9 canvas on a 4:3 output must crop the sides, never letterbox.
        let m = canvas_map(Some((3840.0, 2160.0)), 1.0, 800.0, 600.0, Scaling::Fill);
        assert!((m.sy - 600.0 / 2160.0).abs() < 1e-6);
        assert!((m.sx - 600.0 / 2160.0).abs() < 1e-6);
        let (l, r, b, t) = m.bounds((3840.0, 2160.0));
        assert!(l <= 0.0 && b <= 0.0, "{l} {b}");
        assert!(r >= 800.0 && t >= 600.0, "{r} {t}");
        assert!((l + r - 800.0).abs() < 1e-3, "centred: {l} {r}");
    }

    #[test]
    fn fit_letterboxes_inside() {
        // 3840x2160 fitted into 800x600 is 800x450: bars top and bottom.
        let m = canvas_map(Some((3840.0, 2160.0)), 1.0, 800.0, 600.0, Scaling::Fit);
        let (l, r, b, t) = m.bounds((3840.0, 2160.0));
        assert!(l >= 0.0 && b >= 0.0 && r <= 800.0 && t <= 600.0);
        assert!((l - 0.0).abs() < 1e-3, "l={l}");
        assert!((b - 75.0).abs() < 1e-3, "b={b}");
        assert!((t - 525.0).abs() < 1e-3, "t={t}");
    }

    #[test]
    fn stretch_matches_both_axes() {
        let m = canvas_map(Some((3840.0, 2160.0)), 1.0, 800.0, 600.0, Scaling::Stretch);
        assert_eq!((m.x, m.y, m.sx, m.sy), (0.0, 0.0, 800.0 / 3840.0, 600.0 / 2160.0));
    }

    #[test]
    fn center_is_native_size_and_centred() {
        let m = canvas_map(Some((3840.0, 2160.0)), 1.0, 800.0, 600.0, Scaling::Center);
        assert_eq!((m.sx, m.sy), (1.0, 1.0));
        assert_eq!((m.x, m.y), (-1520.0, -780.0));
    }

    #[test]
    fn exact_aspect_match_needs_no_crop_for_fill_or_fit() {
        for mode in [Scaling::Fill, Scaling::Fit] {
            let m = canvas_map(Some((3840.0, 2160.0)), 1.0, 1920.0, 1080.0, mode);
            assert_eq!((m.x, m.y, m.sx, m.sy), (0.0, 0.0, 0.5, 0.5), "{mode:?}");
        }
    }

    #[test]
    fn zoom_scales_about_the_canvas_centre() {
        let m = canvas_map(Some((3840.0, 2160.0)), 2.0, 1920.0, 1080.0, Scaling::Fill);
        assert_eq!((m.sx, m.sy), (1.0, 1.0));
        assert_eq!((m.x, m.y), (-960.0, -540.0));
    }

    #[test]
    fn degenerate_canvas_or_zoom_falls_back_to_identity() {
        assert_eq!(
            canvas_map(Some((0.0, 0.0)), 1.0, 100.0, 100.0, Scaling::Fill),
            CanvasMap { x: 0.0, y: 0.0, sx: 1.0, sy: 1.0 }
        );
        let m = canvas_map(Some((100.0, 100.0)), f32::NAN, 100.0, 100.0, Scaling::Fill);
        assert_eq!((m.sx, m.sy), (1.0, 1.0));
    }
}
