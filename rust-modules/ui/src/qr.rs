//! Offline QR links with a four-module quiet zone and integer-sized modules.
//! Encode on mount, upload once in `prepare`, then paint one texture per frame.
use super::{theme, Painter, Rect};
use plx_machine::machine::Measure;
use super::label::HAlign;
use super::text_view::TextView;

/// Caption-sized link text with a 32 px cap-top pitch. At the shared 24 px face this leaves
/// roughly 10–14 px of visible air between address lines, without a paragraph-sized gap.
const LINK_LINE_H: f32 = theme::size::CAPTION as f32 + theme::space::XS;

/// The QR allocation is unchanged; text rectangles describe their actual cap-band extents.
/// `height` is the complete composition measured down from the supplied frame's top.
#[derive(Clone, Copy, Debug)]
pub struct QrLinkLayout {
    pub code: Rect,
    pub caption: Rect,
    pub address: Rect,
    pub height: f32,
}

fn link_text<'a>(text: &'a str, measure: &'a dyn Measure) -> TextView<'a> {
    TextView::new(text, theme::size::CAPTION, theme::TEXT_READING)
        .with_measure(measure)
        .h(HAlign::Center)
        .leading(LINK_LINE_H)
        .break_long_words()
}

/// Explicit newlines belong to the address's presentation. TextView wraps each requested line
/// as necessary, while this block preserves those breaks without inserting paragraph spacing.
fn text_block_height(text: &str, width: f32, measure: &dyn Measure) -> f32 {
    if text.is_empty() { return 0.0; }
    let pitches: f32 = text.lines()
        .map(|line| link_text(line, measure).measure_h(width.max(1.0)))
        .sum();
    // TextView reserves a full pitch after its final line; inter-block gaps start at the ink.
    (pitches - LINK_LINE_H + measure.cap_h(theme::size::CAPTION)).max(0.0)
}

impl QrLinkLayout {
    pub fn new(frame: Rect, caption: &str, address: &str, measure: &dyn Measure) -> Self {
        let side = frame.w.min(frame.h * 0.60).max(0.0);
        let code = Rect::new(frame.cx() - side / 2.0, frame.y, side, side);
        let caption_h = text_block_height(caption, code.w, measure);
        let caption = Rect::new(code.x, code.y + code.h + theme::space::MD, code.w, caption_h);
        let gap = if caption_h > 0.0 { theme::space::MD } else { 0.0 };
        let address = Rect::new(
            code.x, caption.y + caption.h + gap, code.w,
            text_block_height(address, code.w, measure),
        );
        let bottom = if address.h > 0.0 { address.y + address.h }
            else if caption.h > 0.0 { caption.y + caption.h }
            else { code.y + code.h };
        Self { code, caption, address, height: bottom - frame.y }
    }
}

/// Passive caption/address composition for a QR link. The owner keeps its encoded QrCode and
/// passes `layout.code` to prepare/draw; this component owns the shared text rhythm and styling.
pub struct QrLink<'a> {
    caption: &'a str,
    address: &'a str,
}

impl<'a> QrLink<'a> {
    pub fn new(caption: &'a str, address: &'a str) -> Self {
        Self { caption, address }
    }

    pub fn layout(&self, frame: Rect, measure: &dyn Measure) -> QrLinkLayout {
        QrLinkLayout::new(frame, self.caption, self.address, measure)
    }

    /// Both runs are centred on the QR square, inside its allocated width.
    /// Measuring and painting share the same text view, explicit line breaks, and line pitch.
    pub fn draw(&self, painter: Painter, frame: Rect, measure: &dyn Measure) -> QrLinkLayout {
        let layout = self.layout(frame, measure);
        if layout.height <= 0.0 { return layout; }
        for (text, block) in [(self.caption, layout.caption), (self.address, layout.address)] {
            let mut y = block.y;
            for line in text.lines() {
                let view = link_text(line, measure);
                let width = block.w.max(1.0);
                let height = view.measure_h(width);
                view.draw(painter, Rect::new(block.x, y, width, height));
                y += height;
            }
        }
        layout
    }
}

pub struct QrCode {
    code: qrcodegen::QrCode,
    texture: u32,
    pixels: i32,
}

impl QrCode {
    pub fn new(text: &str) -> Result<Self, qrcodegen::DataTooLong> {
        qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium)
            .map(|code| Self { code, texture: 0, pixels: 0 })
    }

    /// Main-thread GL preparation, like the sign-in screen's downloaded QR image.
    pub fn prepare(&mut self, frame: Rect) {
        let modules = self.code.size() + 8;
        let scale = (frame.w.min(frame.h) / modules as f32).floor() as i32;
        let side = modules * scale;
        if scale < 1 || (self.texture != 0 && self.pixels == side) { return; }
        let mut rgba = vec![255u8; side as usize * side as usize * 4];
        for y in 0..side {
            for x in 0..side {
                // Encoder reads outside the matrix as light, preserving the quiet zone.
                if self.code.get_module(x / scale - 4, y / scale - 4) {
                    let offset = (y as usize * side as usize + x as usize) * 4;
                    rgba[offset..offset + 3].fill(0);
                }
            }
        }
        plx_gfx::gfx::delete_tex(self.texture);
        self.texture = plx_gfx::img::img_upload_rgba(rgba.as_ptr(), side, side);
        self.pixels = side;
    }

    pub fn draw(&self, painter: Painter, frame: Rect) {
        if self.texture == 0 { return; }
        let side = self.pixels as f32;
        let square = Rect::new((frame.cx() - side / 2.0).round(), (frame.cy() - side / 2.0).round(), side, side);
        painter.tex(self.texture, square, 0.0, theme::SURFACE_QR_PLATE);
    }
}

impl Drop for QrCode {
    fn drop(&mut self) { plx_gfx::gfx::delete_tex(self.texture); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    struct CaptionMetrics;
    impl Measure for CaptionMetrics {
        fn width(&self, text: &CStr, size: i32, bold: bool) -> f32 {
            assert_eq!(size, theme::size::CAPTION);
            assert!(!bold, "caption and address use the same regular face");
            text.to_str().unwrap().chars().count() as f32 * 12.0
        }
        fn cap_h(&self, size: i32) -> f32 {
            assert_eq!(size, theme::size::CAPTION);
            18.0
        }
        fn line_h(&self, _: i32) -> f32 { LINK_LINE_H }
    }

    const ADDRESS: &str = "github.com/GLinnik21/plx-native\n/blob/main/docs/localization.md";

    #[test]
    fn qr_link_preserves_code_size_and_centres_text_within_its_width() {
        let frame = Rect::new(960.0, 180.0, 864.0, 720.0);
        let link = QrLink::new("Scan the QR code", ADDRESS);
        let layout = link.layout(frame, &CaptionMetrics);
        assert!((layout.code.w - 432.0).abs() < 0.001);
        assert_eq!(layout.code.w, layout.code.h);
        assert_eq!(layout.code.cx(), frame.cx());
        assert_eq!(layout.code.y, frame.y);
        for block in [layout.caption, layout.address] {
            assert_eq!(block.x, layout.code.x);
            assert_eq!(block.w, layout.code.w);
            assert_eq!(block.cx(), layout.code.cx());
        }
        assert_eq!(link.address, ADDRESS, "display address must remain literal, including /blob/main/");
    }

    #[test]
    fn qr_link_spaces_caption_and_two_address_lines_from_their_ink() {
        let frame = Rect::new(960.0, 180.0, 864.0, 720.0);
        let layout = QrLinkLayout::new(frame, "Скануйце QR-код", ADDRESS, &CaptionMetrics);
        assert_eq!(theme::size::CAPTION, 24);
        assert_eq!(layout.caption.y - (layout.code.y + layout.code.h), 24.0);
        assert_eq!(layout.address.y - (layout.caption.y + layout.caption.h), 24.0);
        assert_eq!(layout.caption.h, 18.0);
        assert_eq!(layout.address.h, 50.0, "two lines form one block with one 32 px pitch");
        assert_eq!(LINK_LINE_H - CaptionMetrics.cap_h(theme::size::CAPTION), 14.0);
        assert_eq!(layout.height, layout.address.y + layout.address.h - frame.y);
        assert!(layout.height <= frame.h);
        let old_caption_top = layout.code.y + layout.code.h + theme::space::LG;
        assert_eq!(old_caption_top - layout.caption.y, 16.0, "the text starts 16 px above the old reader");
    }

    #[test]
    fn qr_link_flows_wrapped_captions_without_adding_address_paragraph_gaps() {
        let frame = Rect::new(972.0, 180.0, 520.0, 800.0);
        let short = QrLinkLayout::new(frame, "Скануйце QR-код", ADDRESS, &CaptionMetrics);
        let long = QrLinkLayout::new(frame,
            "Скануйце QR-код на тэлефоне, каб адкрыць інструкцыю для перакладчыкаў.",
            ADDRESS, &CaptionMetrics);
        assert!(long.caption.h > short.caption.h);
        assert_eq!(long.address.y - short.address.y, long.caption.h - short.caption.h);
        assert_eq!(long.address.h, short.address.h);
        assert_eq!(long.address.h, 50.0);
        assert_eq!(long.code.y, short.code.y);
        assert_eq!(long.code.w, short.code.w);
    }
}
