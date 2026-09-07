//! How a wallpaper is fitted to an output.
//!
//! Kept separate from any Wayland code so the arithmetic — which is where
//! off-by-one and aspect-ratio mistakes live — can be tested on its own.

/// Where to draw the source image within the surface, in surface pixels.
///
/// The rectangle may extend beyond the surface (`Fill` crops) or sit inside it
/// (`Fit` letterboxes); the caller clips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub x: i64,
    pub y: i64,
    pub width: u32,
    pub height: u32,
}

impl Placement {
    /// Whether the placement leaves any surface pixel uncovered, in which case
    /// the caller must paint a background first.
    pub fn leaves_gaps(&self, surface_w: u32, surface_h: u32) -> bool {
        self.x > 0
            || self.y > 0
            || (self.x + self.width as i64) < surface_w as i64
            || (self.y + self.height as i64) < surface_h as i64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scaling {
    /// Cover the output, cropping the overflow. The usual choice.
    #[default]
    Fill,
    /// Fit entirely inside the output, letterboxing the remainder.
    Fit,
    /// Ignore aspect ratio and match the output exactly.
    Stretch,
    /// Draw at native size, centred; crops or letterboxes as needed.
    Center,
}

impl Scaling {
    pub fn as_str(self) -> &'static str {
        match self {
            Scaling::Fill => "fill",
            Scaling::Fit => "fit",
            Scaling::Stretch => "stretch",
            Scaling::Center => "center",
        }
    }
}

/// Place a `src_w` x `src_h` image on a `dst_w` x `dst_h` surface.
///
/// A degenerate source (either dimension zero) yields an empty placement rather
/// than a division by zero; callers treat that as nothing to draw.
pub fn place(mode: Scaling, src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Placement {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return Placement {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        };
    }

    let (w, h) = match mode {
        Scaling::Stretch => (dst_w, dst_h),
        Scaling::Center => (src_w, src_h),
        Scaling::Fill | Scaling::Fit => {
            // Compare aspect ratios by cross-multiplying to stay in integers.
            let src_wider = (src_w as u64) * (dst_h as u64) > (dst_w as u64) * (src_h as u64);
            let match_width = match mode {
                Scaling::Fill => !src_wider,
                _ => src_wider,
            };
            if match_width {
                let h = ((src_h as u64) * (dst_w as u64) / (src_w as u64)).max(1);
                (dst_w, h as u32)
            } else {
                let w = ((src_w as u64) * (dst_h as u64) / (src_h as u64)).max(1);
                (w as u32, dst_h)
            }
        }
    };

    Placement {
        x: (dst_w as i64 - w as i64) / 2,
        y: (dst_h as i64 - h as i64) / 2,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stretch_matches_surface_exactly() {
        let p = place(Scaling::Stretch, 100, 100, 800, 600);
        assert_eq!((p.x, p.y, p.width, p.height), (0, 0, 800, 600));
        assert!(!p.leaves_gaps(800, 600));
    }

    #[test]
    fn fill_covers_and_overflows_one_axis() {
        // A 16:9 image covering a 4:3 surface matches the height and overflows
        // the width; matching the width instead would leave bars top and bottom.
        let p = place(Scaling::Fill, 1920, 1080, 800, 600);
        assert_eq!(p.height, 600);
        assert!(p.width >= 800, "must cover: {p:?}");
        assert!(p.x <= 0);
        assert!(!p.leaves_gaps(800, 600));
    }

    #[test]
    fn fit_stays_inside_and_letterboxes() {
        let p = place(Scaling::Fit, 1920, 1080, 800, 600);
        assert_eq!(p.width, 800);
        assert!(p.height <= 600, "must fit: {p:?}");
        assert!(p.y >= 0);
        assert!(p.leaves_gaps(800, 600));
    }

    #[test]
    fn square_image_on_wide_surface() {
        let fill = place(Scaling::Fill, 500, 500, 1920, 1080);
        assert_eq!(fill.width, 1920);
        assert!(fill.height >= 1080);
        let fit = place(Scaling::Fit, 500, 500, 1920, 1080);
        assert_eq!(fit.height, 1080);
        assert!(fit.width <= 1920);
    }

    #[test]
    fn exact_aspect_match_needs_no_crop_or_bars() {
        for mode in [Scaling::Fill, Scaling::Fit] {
            let p = place(mode, 1920, 1080, 3840, 2160);
            assert_eq!(
                (p.x, p.y, p.width, p.height),
                (0, 0, 3840, 2160),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn center_keeps_native_size() {
        let p = place(Scaling::Center, 400, 300, 800, 600);
        assert_eq!((p.width, p.height), (400, 300));
        assert_eq!((p.x, p.y), (200, 150));
        assert!(p.leaves_gaps(800, 600));
    }

    #[test]
    fn center_larger_than_surface_crops_symmetrically() {
        let p = place(Scaling::Center, 1000, 800, 800, 600);
        assert_eq!((p.x, p.y), (-100, -100));
        assert!(!p.leaves_gaps(800, 600));
    }

    #[test]
    fn degenerate_sizes_do_not_panic() {
        for (sw, sh, dw, dh) in [(0, 10, 800, 600), (10, 0, 800, 600), (10, 10, 0, 0)] {
            let p = place(Scaling::Fill, sw, sh, dw, dh);
            assert_eq!((p.width, p.height), (0, 0));
        }
    }

    #[test]
    fn extreme_aspect_never_collapses_to_zero() {
        let p = place(Scaling::Fit, 10000, 1, 800, 600);
        assert!(
            p.height >= 1,
            "a one-pixel-tall result must still be drawable: {p:?}"
        );
    }
}
