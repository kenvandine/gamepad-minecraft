// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

//! Renders the device-code verification URI as a QR code, in-process,
//! using the `qrcode` crate's raw module matrix. No external `qrencode`
//! binary and no `image` crate — deliberately hand-rolled, same "no extra
//! rendering library" philosophy as gamepad-2048's `audio.rs`. GTK-free;
//! `main.rs` wraps the resulting buffer in a `gdk::MemoryTexture`.

use qrcode::QrCode;

pub struct QrImage {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Renders `data` as a QR code, `px_per_module` pixels per module, with a
/// one-module quiet border on each side.
pub fn render_verification_qr(data: &str, px_per_module: u32) -> Result<QrImage, String> {
    let code = QrCode::new(data.as_bytes()).map_err(|e| e.to_string())?;
    let modules = code.width() as u32;
    let border = 1u32;
    let side_modules = modules + border * 2;
    let side_px = side_modules * px_per_module;

    let mut rgba = vec![0xFFu8; (side_px * side_px * 4) as usize];

    for y in 0..modules {
        for x in 0..modules {
            let dark = code[(x as usize, y as usize)] == qrcode::Color::Dark;
            if !dark {
                continue;
            }
            let px0 = (x + border) * px_per_module;
            let py0 = (y + border) * px_per_module;
            for py in py0..py0 + px_per_module {
                for px in px0..px0 + px_per_module {
                    let idx = ((py * side_px + px) * 4) as usize;
                    rgba[idx] = 0x15;
                    rgba[idx + 1] = 0x06;
                    rgba[idx + 2] = 0x10;
                    rgba[idx + 3] = 0xFF;
                }
            }
        }
    }

    Ok(QrImage {
        rgba,
        width: side_px,
        height: side_px,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_square_buffer() {
        let img = render_verification_qr("https://microsoft.com/link", 4).unwrap();
        assert_eq!(img.width, img.height);
        assert_eq!(img.rgba.len(), (img.width * img.height * 4) as usize);
    }
}
