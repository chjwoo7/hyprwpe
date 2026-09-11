//! Keyframe animation for scene object properties.
//!
//! A Wallpaper Engine scene object property can be a plain number (`"alpha": 1`)
//! or an animated one:
//!
//! ```json
//! "alpha": {
//!   "value": 1.0,
//!   "animation": {
//!     "c0": [ { "frame": 0, "value": 0.0 }, { "frame": 18, "value": 1.0 } ],
//!     "options": { "fps": 30, "length": 18, "mode": "single", "startpaused": true }
//!   }
//! }
//! ```
//!
//! `c0`, `c1`, `c2` are one track per component (a scalar uses only `c0`; a
//! vector uses three), each a list of keyframes with integer frame numbers.
//! `options.mode` is `single` (play once, hold the last value), `loop` (wrap) or
//! `mirror` (ping-pong). Timing is `frame / fps`; a track is sampled with linear
//! interpolation between the surrounding keyframes.
//!
//! This module is pure arithmetic with no renderer dependency, so the sampling
//! rules are unit-tested and shared by the GPU renderer and the harness.

use serde::Deserialize;

/// How a track repeats once it passes its last keyframe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnimMode {
    /// Play once and hold the final value.
    #[default]
    Single,
    /// Restart from the beginning.
    Loop,
    /// Play forward then backward.
    Mirror,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Keyframe {
    #[serde(default)]
    pub frame: u32,
    #[serde(default)]
    pub value: f32,
}

/// One component's keyframes. Serialised as a bare array (`"c0": [ {...} ]`),
/// hence `transparent`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(transparent)]
pub struct Track(pub Vec<Keyframe>);

impl Track {
    /// Sample the track at `frame`, linearly interpolating between keys.
    ///
    /// Before the first key it holds the first value and after the last it
    /// holds the last, which is what the `single` mode's "hold" needs; the other
    /// modes remap `frame` before it reaches here.
    pub fn sample(&self, frame: f32) -> Option<f32> {
        let keys = &self.0;
        let first = keys.first()?;
        if frame <= first.frame as f32 {
            return Some(first.value);
        }
        let mut prev = first;
        for k in keys.iter().skip(1) {
            if frame <= k.frame as f32 {
                let span = (k.frame - prev.frame) as f32;
                if span <= 0.0 {
                    return Some(k.value);
                }
                let t = (frame - prev.frame as f32) / span;
                return Some(prev.value + (k.value - prev.value) * t);
            }
            prev = k;
        }
        Some(prev.value)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct AnimOptions {
    #[serde(default = "default_fps")]
    pub fps: f32,
    /// Total length in frames; 0 means "use the last keyframe".
    #[serde(default)]
    pub length: u32,
    #[serde(default)]
    pub mode: AnimMode,
    #[serde(default)]
    pub startpaused: bool,
    #[serde(default)]
    pub wraploop: Option<bool>,
}

fn default_fps() -> f32 {
    30.0
}

impl Default for AnimOptions {
    fn default() -> Self {
        AnimOptions {
            fps: default_fps(),
            length: 0,
            mode: AnimMode::Single,
            startpaused: false,
            wraploop: None,
        }
    }
}

/// A property's keyframe animation, one track per component.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Animation {
    #[serde(default)]
    pub c0: Option<Track>,
    #[serde(default)]
    pub c1: Option<Track>,
    #[serde(default)]
    pub c2: Option<Track>,
    #[serde(default)]
    pub options: AnimOptions,
}

impl Animation {
    /// Effective length in frames: the declared `options.length`, else the
    /// furthest key across every track.
    pub fn length(&self) -> f32 {
        if self.options.length > 0 {
            return self.options.length as f32;
        }
        let mut max = 0u32;
        for t in [&self.c0, &self.c1, &self.c2].into_iter().flatten() {
            if let Some(k) = t.0.last() {
                max = max.max(k.frame);
            }
        }
        max as f32
    }

    /// Map a source frame onto the track's own timeline, honouring the repeat
    /// mode. `single` clamps, `loop` wraps, `mirror` reflects.
    fn map_frame(&self, frame: f32, length: f32) -> f32 {
        if length <= 0.0 {
            return frame;
        }
        match self.options.mode {
            AnimMode::Single => frame.min(length),
            AnimMode::Loop => {
                let m = frame % length;
                if m < 0.0 {
                    m + length
                } else {
                    m
                }
            }
            AnimMode::Mirror => {
                let period = length * 2.0;
                let mut m = frame % period;
                if m < 0.0 {
                    m += period;
                }
                if m > length {
                    period - m
                } else {
                    m
                }
            }
        }
    }

    /// Sample a scalar property (only track `c0`) at `time` seconds.
    pub fn sample(&self, time: f32) -> Option<f32> {
        self.sample_track(self.c0.as_ref()?, time)
    }

    /// Sample a vector property from `c0`/`c1`/`c2` at `time` seconds. Missing
    /// components fall back to the first track's value.
    pub fn sample_vec3(&self, time: f32) -> Option<[f32; 3]> {
        let x = self.sample(time)?;
        let y = match &self.c1 {
            Some(t) => self.sample_track(t, time)?,
            None => x,
        };
        let z = match &self.c2 {
            Some(t) => self.sample_track(t, time)?,
            None => x,
        };
        Some([x, y, z])
    }

    fn sample_track(&self, track: &Track, time: f32) -> Option<f32> {
        // `startpaused` animations (a media-play icon, say) only advance once a
        // script triggers them. hyprwpe has no scene scripting yet, so holding
        // the first frame is the honest reading — advancing would show motion the
        // wallpaper never asks for on its own.
        let time = if self.options.startpaused { 0.0 } else { time };
        let frame = time * self.options.fps;
        let length = self.length();
        track.sample(self.map_frame(frame, length))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anim(json: &str) -> Animation {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn parses_a_real_alpha_animation() {
        // Shape taken from the corpus (Play/Pause icon fade in the Akali scene).
        let a = anim(
            r#"{
                "c0": [
                    {"frame": 0, "value": 0.0},
                    {"frame": 1, "value": 1.0},
                    {"frame": 2, "value": 1.0},
                    {"frame": 18, "value": 0.0}
                ],
                "options": {"fps": 30, "length": 18, "mode": "single", "startpaused": true}
            }"#,
        );
        assert_eq!(a.options.fps, 30.0);
        assert_eq!(a.options.length, 18);
        assert_eq!(a.options.mode, AnimMode::Single);
        assert!(a.options.startpaused);
        assert_eq!(a.length(), 18.0);
    }

    #[test]
    fn single_holds_after_the_last_key() {
        let a = anim(
            r#"{"c0":[{"frame":0,"value":0},{"frame":10,"value":1}],
                "options":{"fps":10,"length":10,"mode":"single"}}"#,
        );
        assert_eq!(a.sample(0.0), Some(0.0));
        assert!((a.sample(0.5).unwrap() - 0.5).abs() < 1e-6); // frame 5
        assert_eq!(a.sample(1.0), Some(1.0)); // frame 10
        assert_eq!(a.sample(5.0), Some(1.0)); // past the end: hold
    }

    #[test]
    fn loop_wraps_back_to_the_start() {
        let a = anim(
            r#"{"c0":[{"frame":0,"value":0},{"frame":10,"value":1}],
                "options":{"fps":10,"length":10,"mode":"loop"}}"#,
        );
        // frame 10 is one full cycle, so it wraps to frame 0.
        assert_eq!(a.sample(1.0), Some(0.0));
        assert!((a.sample(1.05).unwrap() - 0.05).abs() < 1e-6); // frame 10.5 -> 0.5
    }

    #[test]
    fn mirror_ping_pongs() {
        let a = anim(
            r#"{"c0":[{"frame":0,"value":0},{"frame":10,"value":1}],
                "options":{"fps":10,"length":10,"mode":"mirror"}}"#,
        );
        assert_eq!(a.sample(1.0), Some(1.0)); // frame 10: peak
        assert!((a.sample(1.5).unwrap() - 0.5).abs() < 1e-6); // frame 15 -> relfected to 5
        assert_eq!(a.sample(2.0), Some(0.0)); // frame 20 -> back to start
    }

    #[test]
    fn vector_animation_uses_three_tracks() {
        let a = anim(
            r#"{"c0":[{"frame":0,"value":0},{"frame":10,"value":10}],
                "c1":[{"frame":0,"value":100},{"frame":10,"value":200}],
                "c2":[{"frame":0,"value":0},{"frame":10,"value":0}],
                "options":{"fps":10,"length":10,"mode":"single"}}"#,
        );
        let v = a.sample_vec3(1.0).unwrap();
        assert_eq!(v, [10.0, 200.0, 0.0]);
    }

    #[test]
    fn a_single_key_is_constant() {
        let a = anim(r#"{"c0":[{"frame":7,"value":3.0}],"options":{"fps":30}}"#);
        assert_eq!(a.sample(0.0), Some(3.0));
        assert_eq!(a.sample(10.0), Some(3.0));
    }

    #[test]
    fn absent_length_uses_the_last_key() {
        let a = anim(r#"{"c0":[{"frame":0,"value":0},{"frame":42,"value":1}],"options":{"fps":30}}"#);
        assert_eq!(a.length(), 42.0);
        // loop without a declared length still wraps at 42 frames
        let a2: Animation = serde_json::from_str(
            r#"{"c0":[{"frame":0,"value":0},{"frame":42,"value":1}],
                "options":{"fps":30,"mode":"loop"}}"#,
        )
        .unwrap();
        let t = 42.0 / 30.0;
        assert_eq!(a2.sample(t), Some(0.0));
    }

    #[test]
    fn startpaused_holds_the_first_frame() {
        // Without scene scripting there is nothing to unpause such an
        // animation, so it must stay at its first frame rather than run free.
        let a = anim(
            r#"{"c0":[{"frame":0,"value":0},{"frame":10,"value":1}],
                "options":{"fps":10,"length":10,"mode":"loop","startpaused":true}}"#,
        );
        assert_eq!(a.sample(0.0), Some(0.0));
        assert_eq!(a.sample(5.0), Some(0.0));
    }

    #[test]
    fn empty_animation_samples_nothing() {
        let a = anim(r#"{"options":{"fps":30,"length":10}}"#);
        assert_eq!(a.sample(0.0), None);
        assert_eq!(a.sample_vec3(0.0), None);
    }
}
