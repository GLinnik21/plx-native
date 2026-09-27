//! Offline QR links with a four-module quiet zone and integer-sized modules.
//! Encode on mount, upload once in `prepare`, then paint one texture per frame.
use super::{theme, Painter, Rect};

pub(crate) struct QrCode {
    code: qrcodegen::QrCode,
    texture: u32,
    pixels: i32,
}

impl QrCode {
    pub(crate) fn new(text: &str) -> Result<Self, qrcodegen::DataTooLong> {
        qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium)
            .map(|code| Self { code, texture: 0, pixels: 0 })
    }

    /// Main-thread GL preparation, like the sign-in screen's downloaded QR image.
    pub(crate) fn prepare(&mut self, frame: Rect) {
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
        crate::gfx::delete_tex(self.texture);
        self.texture = crate::img::img_upload_rgba(rgba.as_ptr(), side, side);
        self.pixels = side;
    }

    pub(crate) fn draw(&self, painter: Painter, frame: Rect) {
        if self.texture == 0 { return; }
        let side = self.pixels as f32;
        let square = Rect::new((frame.cx() - side / 2.0).round(), (frame.cy() - side / 2.0).round(), side, side);
        painter.tex(self.texture, square, 0.0, theme::SURFACE_QR_PLATE);
    }
}

impl Drop for QrCode {
    fn drop(&mut self) { crate::gfx::delete_tex(self.texture); }
}
