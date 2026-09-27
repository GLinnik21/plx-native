//! Picture geometry for overlays, independent of the subtitle's authoring resolution.
//! The ACB sourceInfo supplies the decoded raster and pixel aspect ratio; the dev
//! TV's 1440x1080 capture proves that its full-panel window pillarboxes that picture.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Viewport {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Aspect(u32, u32);

impl Aspect {
    pub(crate) fn from_raster(width: i32, height: i32) -> Option<Self> {
        (width > 0 && height > 0).then_some(Self(width as u32, height as u32))
    }

    pub(crate) fn pack(self) -> u64 {
        (u64::from(self.0) << 32) | u64::from(self.1)
    }

    pub(crate) fn unpack(value: u64) -> Option<Self> {
        let aspect = Self((value >> 32) as u32, value as u32);
        (aspect.0 != 0 && aspect.1 != 0).then_some(aspect)
    }

    pub(crate) fn fit(self, width: i32, height: i32) -> Viewport {
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

/// Parse only the sourceInfo video object, never a similarly named nested field.
/// Observed on the TV as `video:{width,height,pixelAspectRatio:{width,height}}`.
/// This runs only on metadata callbacks, not on the frame/position callback path.
pub(super) fn source_aspect(bytes: &[u8]) -> Option<Aspect> {
    let root: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let video = root.get("video")?;
    let positive = |v: &serde_json::Value| v.as_u64().filter(|&n| n > 0 && n <= 65_536);
    let w = positive(video.get("width")?)?;
    let h = positive(video.get("height")?)?;
    let (pw, ph) = match video.get("pixelAspectRatio") {
        Some(par) => (positive(par.get("width")?)?, positive(par.get("height")?)?),
        None => (1, 1),
    };
    let (num, den) = (w * pw, h * ph);
    let (mut a, mut b) = (num, den);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    Some(Aspect(
        u32::try_from(num / a).ok()?,
        u32::try_from(den / a).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn missing_or_invalid_geometry_does_not_publish_an_aspect() {
        for value in [
            br#"{}"#.as_slice(),
            br#"{"video":{"width":0,"height":1080}}"#,
            br#"{"video":{"width":1920,"height":1080,"pixelAspectRatio":{"width":0,"height":1}}}"#,
            br#"{"video":{"width":-1,"height":1080}}"#,
            br#"{"video":{"width":4294967295,"height":1080}}"#,
        ] {
            assert_eq!(source_aspect(value), None);
        }
        assert_eq!(Aspect::unpack(0), None);
        let aspect = source_aspect(br#"{"video":{"width":1440,"height":1080}}"#).unwrap();
        assert_eq!(Aspect::unpack(aspect.pack()), Some(aspect));
    }
}
