//! Picture geometry for overlays, independent of the subtitle's authoring resolution.
//! The ACB sourceInfo supplies the decoded raster and pixel aspect ratio; the dev
//! TV's 1440x1080 capture proves that its full-panel window pillarboxes that picture.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Viewport {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aspect(u32, u32);

impl Aspect {
    pub fn from_raster(width: i32, height: i32) -> Option<Self> {
        (width > 0 && height > 0).then_some(Self(width as u32, height as u32))
    }

    pub fn pack(self) -> u64 {
        (u64::from(self.0) << 32) | u64::from(self.1)
    }

    pub fn unpack(value: u64) -> Option<Self> {
        let aspect = Self((value >> 32) as u32, value as u32);
        (aspect.0 != 0 && aspect.1 != 0).then_some(aspect)
    }

    pub fn fit(self, width: i32, height: i32) -> Viewport {
        let (w, h) = (i64::from(width.max(1)), i64::from(height.max(1)));
        let (num, den) = (i64::from(self.0), i64::from(self.1));
        let (w, h) = if w * den > h * num {
            (((h * num + den / 2) / den).clamp(1, w), h)
        } else {
            (w, ((w * den + num / 2) / num).clamp(1, h))
        };
        Viewport {
            x: (width - w as i32) / 2,
            y: (height - h as i32) / 2,
            width: w as i32,
            height: h as i32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parse lives with the callback that reads sourceInfo (`plx_play_feed::sf_event`); the
    /// reduced pair it returns is what the mailbox publishes and `video_viewport` unpacks.
    fn source_aspect(json: &[u8]) -> Option<Aspect> {
        let (num, den) = plx_play_feed::sf_event::source_aspect(json)?;
        Aspect::unpack((u64::from(num) << 32) | u64::from(den))
    }

    #[test]
    fn picture_fit_covers_pillarbox_letterbox_and_anamorphic_sources() {
        let fit = |json: &[u8]| source_aspect(json).unwrap().fit(1920, 1080);
        assert_eq!(fit(br#"{"video":{"width":1440,"height":1080,"pixelAspectRatio":{"width":1,"height":1}}}"#),
            Viewport { x: 240, y: 0, width: 1440, height: 1080 });
        assert_eq!(
            fit(br#"{"video":{"width":1920,"height":800}}"#),
            Viewport {
                x: 0,
                y: 140,
                width: 1920,
                height: 800
            }
        );
        assert_eq!(fit(br#"{"video":{"width":720,"height":576,"pixelAspectRatio":{"width":64,"height":45}}}"#),
            Viewport { x: 0, y: 0, width: 1920, height: 1080 });
        assert_eq!(
            Aspect::from_raster(3840, 2160).unwrap().fit(1920, 1080),
            Viewport {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080
            }
        );
    }

    #[test]
    fn the_published_pair_round_trips_through_the_aspect() {
        assert_eq!(Aspect::unpack(0), None);
        let aspect = source_aspect(br#"{"video":{"width":1440,"height":1080}}"#).unwrap();
        assert_eq!(Aspect::unpack(aspect.pack()), Some(aspect));
    }
}
