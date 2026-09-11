//! Reader for Wallpaper Engine `MDLV` puppet models.
//!
//! A scene layer whose `models/*.json` names a `"puppet"` is a deforming mesh
//! rather than a quad. This parses that mesh, its skeleton, attachments and
//! animation. See `docs/FORMATS.md` and `docs/WE-EDITOR-REFERENCE.md` for what
//! each part means in editor terms.
//!
//! **Version driven, never wallpaper driven.** The format exists in several
//! revisions (the local corpus holds `MDLV0013/14/16/17/23`) and each reference
//! gives every length from the file itself:
//!
//! * sections are found by scanning for the `MD??NNNN` tags and validated by the
//!   small header that follows each one; a section's end comes from the *next
//!   section offset the file stores itself*;
//! * element counts come from the file's own count fields;
//! * per-element byte sizes come from the file's own size fields, so a bone
//!   matrix or a keyframe may change size without touching this code;
//! * the mesh block stores its own vertex and index byte lengths.
//!
//! Nothing here indexes into a specific wallpaper's bytes, so a new format
//! revision needs no new branch unless it changes a field's *meaning*.

use anyhow::{bail, Result};

/// The 8-byte section tag, e.g. `MDLS0004`.
const TAG_LEN: usize = 8;
/// The material path always begins here, after the `MDLV` fixed prefix.
const MATERIAL_OFF: usize = 21;
/// How far past the material string to look for the mesh block.
const MESH_SEARCH_WINDOW: usize = 160;
const MIN_STRIDE: usize = 16;
const MAX_STRIDE: usize = 256;

/// A parsed puppet model.
#[derive(Debug, Clone)]
pub struct PuppetModel {
    /// Numeric revision from the `MDLV####` tag (e.g. 23 for `MDLV0023`).
    pub version: u32,
    pub material: String,
    pub mesh: Mesh,
    pub bones: Vec<Bone>,
    pub attachments: Vec<Attachment>,
    pub animations: Vec<PuppetAnimation>,
}

#[derive(Debug, Clone, Default)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    /// Bytes per vertex record.
    pub stride: usize,
    /// Raw per-vertex records, `stride / 4` floats each. Kept so per-vertex
    /// attributes whose slots are not yet pinned (skin weights) can be read
    /// without re-parsing.
    pub records: Vec<Vec<f32>>,
}

#[derive(Debug, Clone, Default)]
pub struct Bone {
    /// Index of the parent bone, or `-1` for a root.
    pub parent: i32,
    /// 4x4 bind matrix, row-major.
    pub bind: [[f32; 4]; 4],
    /// Free-form per-bone parameter string from the file.
    pub params: String,
}

#[derive(Debug, Clone, Default)]
pub struct Attachment {
    /// Index of the bone this attachment hangs off (observed values are all
    /// below the bone count, but the meaning is inferred, not proven).
    pub kind: u16,
    pub name: String,
    /// 4x4 matrix, row-major.
    pub matrix: [[f32; 4]; 4],
}

#[derive(Debug, Clone, Default)]
pub struct PuppetAnimation {
    pub name: String,
    /// `"loop"`, `"single"`, … as stored by the editor.
    pub mode: String,
    pub fps: f32,
    /// Length in frames; tracks carry `length + 1` keys.
    pub length: u32,
    /// One track per bone, in bone order.
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, Default)]
pub struct Track {
    pub keys: Vec<Keyframe>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Keyframe {
    pub position: [f32; 3],
    /// Rotation in radians (a 2D puppet stores a single scalar).
    pub rotation: f32,
    pub scale: [f32; 3],
}

impl PuppetAnimation {
    /// Sample every bone at `time` seconds, returning each bone's local
    /// translation/rotation/scale.
    pub fn sample(&self, time: f32) -> Vec<Keyframe> {
        let frame = (time * self.fps).max(0.0);
        let length = if self.length > 0 {
            self.length as f32
        } else {
            0.0
        };
        let frame = match self.mode.as_str() {
            "loop" if length > 0.0 => frame % length,
            _ => frame.min(length),
        };
        self.tracks.iter().map(|t| t.sample(frame)).collect()
    }
}

impl Track {
    fn sample(&self, frame: f32) -> Keyframe {
        if self.keys.is_empty() {
            return Keyframe::default();
        }
        // Keys are one per frame, so the surrounding pair is a direct index;
        // interpolate between them for a sub-frame time.
        let i = frame.floor().max(0.0) as usize;
        if i + 1 >= self.keys.len() {
            return *self.keys.last().expect("non-empty");
        }
        let t = frame - i as f32;
        let a = self.keys[i];
        let b = self.keys[i + 1];
        let lerp = |x: f32, y: f32| x + (y - x) * t;
        Keyframe {
            position: [
                lerp(a.position[0], b.position[0]),
                lerp(a.position[1], b.position[1]),
                lerp(a.position[2], b.position[2]),
            ],
            rotation: lerp(a.rotation, b.rotation),
            scale: [
                lerp(a.scale[0], b.scale[0]),
                lerp(a.scale[1], b.scale[1]),
                lerp(a.scale[2], b.scale[2]),
            ],
        }
    }
}

// ---------------------------------------------------------------- primitives

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

fn i32_at(d: &[u8], o: usize) -> Option<i32> {
    Some(i32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

fn f32_at(d: &[u8], o: usize) -> Option<f32> {
    Some(f32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

/// NUL-terminated string, not reading past `limit`.
fn zstring(d: &[u8], o: usize, limit: usize) -> Option<(String, usize)> {
    let end = d.get(o..limit.min(d.len()))?.iter().position(|&b| b == 0)? + o;
    let text = String::from_utf8_lossy(d.get(o..end)?).into_owned();
    Some((text, end + 1))
}

fn read_f32s(d: &[u8], o: usize, n: usize) -> Option<Vec<f32>> {
    let bytes = d.get(o..o + n * 4)?;
    Some(
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect(),
    )
}

fn mat4(d: &[u8], o: usize) -> Option<[[f32; 4]; 4]> {
    let v = read_f32s(d, o, 16)?;
    let mut m = [[0.0f32; 4]; 4];
    for r in 0..4 {
        for c in 0..4 {
            m[r][c] = v[r * 4 + c];
        }
    }
    Some(m)
}

// ------------------------------------------------------------------ sections

/// Is `t` a `MD??NNNN` section tag?
fn is_mdl_tag(t: &[u8]) -> bool {
    t.len() == TAG_LEN
        && t[0] == b'M'
        && t[1] == b'D'
        && t[2..4].iter().all(|c| c.is_ascii_uppercase())
        && t[4..8].iter().all(|c| c.is_ascii_digit())
}

#[derive(Debug, Clone, Copy)]
struct Section {
    offset: usize,
    /// Offset of the section that follows, as stored by the file itself.
    next: usize,
    /// Element count from the section header.
    count: u16,
}

fn section_at(d: &[u8], off: usize) -> Option<Section> {
    if !is_mdl_tag(d.get(off..off + TAG_LEN)?) {
        return None;
    }
    // The byte after the tag is a pad; a section whose pad is not zero is a
    // coincidental byte run, not a real header.
    if *d.get(off + TAG_LEN)? != 0 {
        return None;
    }
    let next = u32_at(d, off + 9)? as usize;
    if next <= off || next > d.len() {
        return None;
    }
    Some(Section {
        offset: off,
        next,
        count: u16_at(d, off + 13)?,
    })
}

/// Every distinct section in file order, found by scanning for validated tags.
///
/// Scanning (rather than assuming a fixed position for each) is what makes the
/// parse independent of the version: the file says where its sections are.
fn find_sections(d: &[u8]) -> Vec<(String, Section)> {
    let mut out: Vec<(String, Section)> = Vec::new();
    let mut i = 0;
    while i + TAG_LEN <= d.len() {
        match d[i..].windows(2).position(|w| w == b"MD") {
            Some(rel) => {
                let j = i + rel;
                if let Some(sec) = section_at(d, j) {
                    let tag = String::from_utf8_lossy(&d[j..j + TAG_LEN]).into_owned();
                    if !out.iter().any(|(t, _)| *t == tag) {
                        out.push((tag, sec));
                    }
                }
                i = j + 1;
            }
            None => break,
        }
    }
    out
}

// ---------------------------------------------------------------------- mesh

/// Fraction of sampled triangles sharing one orientation in the XY plane.
///
/// A correctly paired position (or UV) array produces consistently wound
/// triangles and scores near 1.0; an arbitrarily paired array scores near 0.5.
/// This is the self-validating test that locates the mesh without knowing the
/// version.
fn winding_consistency(verts: &[[f32; 2]], indices: &[u32]) -> f32 {
    let tris = indices.len() / 3;
    if tris == 0 {
        return 0.0;
    }
    let step = (tris / 600).max(1);
    let (mut pos, mut neg) = (0u32, 0u32);
    for t in (0..tris).step_by(step) {
        let (ia, ib, ic) = (indices[3 * t], indices[3 * t + 1], indices[3 * t + 2]);
        let (a, b, c) = match (
            verts.get(ia as usize),
            verts.get(ib as usize),
            verts.get(ic as usize),
        ) {
            (Some(a), Some(b), Some(c)) => (a, b, c),
            _ => continue,
        };
        let cross = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
        if cross > 0.0 {
            pos += 1;
        } else if cross < 0.0 {
            neg += 1;
        }
    }
    let total = pos + neg;
    if total == 0 {
        return 0.0;
    }
    pos.max(neg) as f32 / total as f32
}

/// Locate and parse the mesh block by validating self-consistent invariants.
///
/// The block is not a tagged section, so it is found by probing the bytes just
/// after the material string and keeping the first candidate whose vertex and
/// index arrays agree: the record stride divides the vertex byte length exactly,
/// and both the position triple and the trailing UV pair produce consistent
/// triangle winding.
fn parse_mesh(d: &[u8]) -> Option<Mesh> {
    let nul = d.get(MATERIAL_OFF..)?.iter().position(|&b| b == 0)? + MATERIAL_OFF;
    let hi = (nul + MESH_SEARCH_WINDOW).min(d.len().saturating_sub(12));

    let mut best: Option<(f32, Mesh)> = None;
    for vs in (nul + 1)..hi.max(nul + 1) {
        let Some(vb) = u32_at(d, vs.checked_sub(4)?) else {
            continue;
        };
        let vb = vb as usize;
        if vb == 0 || !vb.is_multiple_of(4) || vs + vb + 4 > d.len() {
            continue;
        }
        let io = vs + vb;
        let Some(ib) = u32_at(d, io).map(|v| v as usize) else {
            continue;
        };
        if ib == 0 || ib > 0x1000_0000 || io + 4 + ib > d.len() {
            continue;
        }

        for esize in [2usize, 4] {
            if ib % (3 * esize) != 0 {
                continue;
            }
            let count = ib / esize;
            if count < 3 {
                continue;
            }
            let indices: Vec<u32> = match (0..count)
                .map(|k| {
                    if esize == 2 {
                        u16_at(d, io + 4 + k * 2).map(u32::from)
                    } else {
                        u32_at(d, io + 4 + k * 4)
                    }
                })
                .collect::<Option<Vec<u32>>>()
            {
                Some(v) => v,
                None => continue,
            };
            let Some(n) = indices.iter().copied().max().map(|m| m as usize + 1) else {
                continue;
            };
            if n < 3 || !vb.is_multiple_of(n) {
                continue;
            }
            let stride = vb / n;
            if !stride.is_multiple_of(4) || !(MIN_STRIDE..=MAX_STRIDE).contains(&stride) {
                continue;
            }
            let nf = stride / 4;

            let mut positions = Vec::with_capacity(n);
            let mut uvs = Vec::with_capacity(n);
            let mut records = Vec::with_capacity(n);
            let mut ok = true;
            for i in 0..n {
                let Some(v) = read_f32s(d, vs + i * stride, nf) else {
                    ok = false;
                    break;
                };
                positions.push([v[0], v[1], v[2]]);
                uvs.push([v[nf - 2], v[nf - 1]]);
                records.push(v);
            }
            if !ok {
                continue;
            }

            let pos_xy: Vec<[f32; 2]> = positions.iter().map(|p| [p[0], p[1]]).collect();
            let pos_w = winding_consistency(&pos_xy, &indices);
            let uv_w = winding_consistency(&uvs, &indices);
            if pos_w < 0.95 || uv_w < 0.95 {
                continue;
            }
            let score = pos_w + uv_w;
            if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
                best = Some((
                    score,
                    Mesh {
                        positions,
                        uvs,
                        indices,
                        stride,
                        records,
                    },
                ));
            }
            break; // u16 preferred; only fall through to u32 if u16 did not fit
        }
    }
    best.map(|(_, m)| m)
}

// ----------------------------------------------------------------- skeleton

fn parse_skeleton(d: &[u8], sec: &Section) -> Vec<Bone> {
    let mut bones = Vec::new();
    let mut p = sec.offset + 17;
    for _ in 0..sec.count {
        // Per-bone lead (a fixed field group), then parent and block size.
        if p + 5 + 8 > sec.next {
            break;
        }
        p += 5;
        let Some(parent) = i32_at(d, p) else { break };
        let Some(size) = u32_at(d, p + 4).map(|v| v as usize) else {
            break;
        };
        p += 8;
        if size % 4 != 0 || p + size > sec.next {
            break;
        }
        let bind = mat4(d, p).unwrap_or_default();
        p += size;
        let params = match zstring(d, p, sec.next) {
            Some((s, np)) => {
                p = np;
                s
            }
            None => break,
        };
        bones.push(Bone {
            parent,
            bind,
            params,
        });
    }
    bones
}

// -------------------------------------------------------------- attachments

fn parse_attachments(d: &[u8], sec: &Section) -> Vec<Attachment> {
    let mut items = Vec::new();
    let mut p = sec.offset + 15;
    for _ in 0..sec.count {
        let Some(kind) = u16_at(d, p) else { break };
        p += 2;
        let Some((name, np)) = zstring(d, p, sec.next) else {
            break;
        };
        p = np;
        if p + 64 > sec.next {
            break;
        }
        let matrix = mat4(d, p).unwrap_or_default();
        p += 64;
        items.push(Attachment { kind, name, matrix });
    }
    items
}

// --------------------------------------------------------------- animations

/// Bytes per keyframe record: 9 floats (position, two unknowns, rotation, scale).
const KEYFRAME_BYTES: usize = 36;

fn parse_animations(d: &[u8], sec: &Section) -> Vec<PuppetAnimation> {
    // The per-animation trailer length is not stored, so recover it: the value
    // that makes the animation list end exactly on the section end. Parsing is
    // retried for each candidate; the checks inside reject wrong ones.
    for trailer in 0..256usize {
        if let Some(anims) = animations_with_trailer(d, sec, trailer) {
            return anims;
        }
    }
    Vec::new()
}

fn animations_with_trailer(
    d: &[u8],
    sec: &Section,
    trailer: usize,
) -> Option<Vec<PuppetAnimation>> {
    let end = sec.next;
    let mut anims = Vec::with_capacity(sec.count as usize);
    let mut p = sec.offset + 17;
    for _ in 0..sec.count {
        u32_at(d, p)?; // X: purpose unknown, unused
        u32_at(d, p + 4)?; // Y: always 0 in every file observed
        p += 8;
        let (name, np) = zstring(d, p, end)?;
        p = np;
        let (mode, np) = zstring(d, p, end)?;
        p = np;
        if p + 24 > end {
            return None;
        }
        let fps = f32_at(d, p)?;
        let length = u32_at(d, p + 4)?;
        let nbones = u32_at(d, p + 12)? as usize;
        p += 16;
        if nbones > 4096 {
            return None;
        }

        let mut tracks = Vec::with_capacity(nbones);
        for _ in 0..nbones {
            u32_at(d, p)?; // B: unused
            let size = u32_at(d, p + 4)? as usize;
            p += 8;
            if p + size > end {
                return None;
            }
            // Tracks are contiguous per frame; the observed correlation is
            // `length + 1` keys, but the size field is authoritative.
            let nkeys = if length > 0 && size.is_multiple_of(length as usize + 1) {
                length as usize + 1
            } else if size.is_multiple_of(KEYFRAME_BYTES) {
                size / KEYFRAME_BYTES
            } else {
                0
            };
            let mut keys = Vec::new();
            match size.checked_div(nkeys) {
                // A stride smaller than one keyframe cannot be right.
                Some(stride) if stride < KEYFRAME_BYTES => return None,
                Some(stride) => {
                    keys.reserve(nkeys);
                    for k in 0..nkeys {
                        let o = p + k * stride;
                        let v = read_f32s(d, o, 9)?;
                        keys.push(Keyframe {
                            position: [v[0], v[1], v[2]],
                            rotation: v[5],
                            scale: [v[6], v[7], v[8]],
                        });
                    }
                }
                // `nkeys == 0`: a track that carries no keys.
                None => {}
            }
            p += size;
            tracks.push(Track { keys });
        }

        p += trailer;
        if p > end {
            return None;
        }
        anims.push(PuppetAnimation {
            name,
            mode,
            fps,
            length,
            tracks,
        });
    }
    // The list must land exactly on the section end for the trailer to be right.
    if p != end {
        return None;
    }
    Some(anims)
}

// -------------------------------------------------------------------- public

/// Parse a puppet model from raw `.mdl` bytes.
pub fn parse(bytes: &[u8]) -> Result<PuppetModel> {
    let tag = bytes
        .get(0..TAG_LEN)
        .ok_or_else(|| anyhow::anyhow!("file too short to hold a tag"))?;
    let tag = String::from_utf8_lossy(tag).into_owned();
    if !tag.starts_with("MDLV") {
        bail!("not an MDLV puppet model (tag {tag:?})");
    }
    let version: u32 = tag
        .get(4..8)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("unreadable version in tag {tag:?}"))?;

    let material = zstring(bytes, MATERIAL_OFF, bytes.len())
        .map(|(s, _)| s)
        .unwrap_or_default();

    let sections = find_sections(bytes);
    let find = |prefix: &str| -> Option<Section> {
        sections
            .iter()
            .find(|(t, _)| t.starts_with(prefix))
            .map(|(_, s)| *s)
    };

    let mesh = parse_mesh(bytes).unwrap_or_default();
    let bones = find("MDLS")
        .map(|s| parse_skeleton(bytes, &s))
        .unwrap_or_default();
    let attachments = find("MDAT")
        .map(|s| parse_attachments(bytes, &s))
        .unwrap_or_default();
    let animations = find("MDLA")
        .map(|s| parse_animations(bytes, &s))
        .unwrap_or_default();

    Ok(PuppetModel {
        version,
        material,
        mesh,
        bones,
        attachments,
        animations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_puppet_files() {
        assert!(parse(b"not a model at all").is_err());
        assert!(parse(b"PKGV0001....").is_err());
    }

    #[test]
    fn tag_validation_is_strict() {
        assert!(is_mdl_tag(b"MDLS0004"));
        assert!(!is_mdl_tag(b"MDLS0004x"));
        assert!(!is_mdl_tag(b"mdLS0004"));
        assert!(!is_mdl_tag(b"MDLSabcd"));
    }

    #[test]
    fn section_header_is_read_from_the_file() {
        // tag(8) pad(1) next(4) count(2)
        let mut d = vec![0u8; 128];
        d[0..8].copy_from_slice(b"MDLS0004");
        d[8] = 0;
        d[9..13].copy_from_slice(&100u32.to_le_bytes());
        d[13..15].copy_from_slice(&7u16.to_le_bytes());
        let sec = section_at(&d, 0).expect("valid header");
        assert_eq!(sec.next, 100);
        assert_eq!(sec.count, 7);

        // A non-zero pad means this byte run is not a real section.
        d[8] = 1;
        assert!(section_at(&d, 0).is_none());
    }

    #[test]
    fn section_scan_finds_each_tag_once_in_order() {
        let mut d = vec![0u8; 300];
        for (off, tag) in [
            (0usize, b"MDLV0023"),
            (100, b"MDLS0004"),
            (200, b"MDLA0006"),
        ] {
            d[off..off + 8].copy_from_slice(tag);
            d[off + 8] = 0;
            let next = (off + 90) as u32;
            d[off + 9..off + 13].copy_from_slice(&next.to_le_bytes());
        }
        d[290..298].copy_from_slice(b"MDLS0004"); // duplicate: ignored
        let secs = find_sections(&d);
        let tags: Vec<&str> = secs.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(tags, vec!["MDLV0023", "MDLS0004", "MDLA0006"]);
    }

    #[test]
    fn keyframe_sampling_interpolates_and_loops() {
        let anim = PuppetAnimation {
            name: "a".into(),
            mode: "loop".into(),
            fps: 10.0,
            length: 3,
            tracks: vec![Track {
                keys: vec![
                    Keyframe {
                        position: [0.0, 0.0, 0.0],
                        rotation: 0.0,
                        scale: [1.0, 1.0, 1.0],
                    },
                    Keyframe {
                        position: [10.0, 0.0, 0.0],
                        rotation: 0.0,
                        scale: [1.0, 1.0, 1.0],
                    },
                    Keyframe {
                        position: [20.0, 0.0, 0.0],
                        rotation: 0.0,
                        scale: [1.0, 1.0, 1.0],
                    },
                    Keyframe {
                        position: [30.0, 0.0, 0.0],
                        rotation: 0.0,
                        scale: [1.0, 1.0, 1.0],
                    },
                ],
            }],
        };
        // t = 0.15s -> frame 1.5 -> halfway between keys 1 and 2.
        let s = anim.sample(0.15);
        assert!(
            (s[0].position[0] - 15.0).abs() < 1e-3,
            "{:?}",
            s[0].position
        );
        // frame 3 == length, loop wraps to 0.
        let s = anim.sample(0.3);
        assert!(s[0].position[0].abs() < 1e-3, "{:?}", s[0].position);
    }

    #[test]
    fn winding_test_separates_real_geometry_from_noise() {
        // A triangle wound consistently: score 1.0.
        let verts = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
        assert!((winding_consistency(&verts, &[0, 1, 2]) - 1.0).abs() < 1e-6);
        // An index that falls outside the vertex list is skipped, not panicking.
        assert_eq!(winding_consistency(&verts, &[0, 1, 99]), 0.0);
    }
}
