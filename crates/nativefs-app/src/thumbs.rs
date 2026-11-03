//! Thumbnails for "Recently uploaded". The server makes a 480 px AVIF still
//! (WebP when its ffmpeg lacks AV1) for videos and images; images without one
//! fall back to the file itself, like the web. Decoding runs off the UI thread
//! into a plain RGBA buffer (`slint::Image` is not `Send`).

use image::imageops::FilterType;
use image::RgbaImage;
use slint::{Rgba8Pixel, SharedPixelBuffer};

/// ISO-BMFF `ftyp` brand of AVIF stills / sequences.
pub(crate) fn is_avif(data: &[u8]) -> bool {
    data.len() > 12 && &data[4..8] == b"ftyp" && matches!(&data[8..12], b"avif" | b"avis" | b"mif1" | b"msf1")
}

pub(crate) fn decode_avif(data: &[u8]) -> Option<RgbaImage> {
    use avif_decode::Image;
    let img = avif_decode::Decoder::from_avif(data).ok()?.to_image().ok()?;
    let (w, h) = match &img {
        Image::Rgb8(i) => (i.width(), i.height()),
        Image::Rgba8(i) => (i.width(), i.height()),
        Image::Rgb16(i) => (i.width(), i.height()),
        Image::Rgba16(i) => (i.width(), i.height()),
        Image::Gray8(i) => (i.width(), i.height()),
        Image::Gray16(i) => (i.width(), i.height()),
    };
    let mut out = Vec::with_capacity(w * h * 4);
    match &img {
        Image::Rgb8(i) => i.rows().flatten().for_each(|p| out.extend_from_slice(&[p.r, p.g, p.b, 255])),
        Image::Rgba8(i) => i.rows().flatten().for_each(|p| out.extend_from_slice(&[p.r, p.g, p.b, p.a])),
        Image::Rgb16(i) => i.rows().flatten().for_each(|p| out.extend_from_slice(&[hi(p.r), hi(p.g), hi(p.b), 255])),
        Image::Rgba16(i) => i.rows().flatten().for_each(|p| out.extend_from_slice(&[hi(p.r), hi(p.g), hi(p.b), hi(p.a)])),
        Image::Gray8(i) => i.rows().flatten().for_each(|p| out.extend_from_slice(&[p.value(), p.value(), p.value(), 255])),
        Image::Gray16(i) => i.rows().flatten().for_each(|p| out.extend_from_slice(&[hi(p.value()), hi(p.value()), hi(p.value()), 255])),
    }
    RgbaImage::from_raw(w as u32, h as u32, out)
}

fn hi(v: u16) -> u8 {
    (v >> 8) as u8
}

/// Decode `data` and crop/scale it to a `px` x `px` square (CSS `object-fit: cover`).
pub fn decode_square(data: &[u8], px: u32) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let img = if is_avif(data) { decode_avif(data)? } else { image::load_from_memory(data).ok()?.into_rgba8() };
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let side = w.min(h);
    let cropped = image::imageops::crop_imm(&img, (w - side) / 2, (h - side) / 2, side, side).to_image();
    let px = px.clamp(16, 256);
    let small = image::imageops::resize(&cropped, px, px, FilterType::Triangle);
    Some(SharedPixelBuffer::clone_from_slice(small.as_raw(), px, px))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_is_cropped_to_a_square() {
        let mut img = RgbaImage::new(40, 20);
        img.put_pixel(20, 10, image::Rgba([255, 0, 0, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let buf = decode_square(&png, 32).unwrap();
        assert_eq!((buf.width(), buf.height()), (32, 32));
    }

    /// Fixtures made with the server's own ffmpeg commands (thumbnails.js) from a test pattern.
    #[test]
    fn server_formats_decode() {
        for data in [&include_bytes!("../tests/fixtures/thumb.avif")[..], &include_bytes!("../tests/fixtures/thumb.webp")[..]] {
            let buf = decode_square(data, 63).expect("decodes");
            assert_eq!((buf.width(), buf.height()), (63, 63));
            // the test pattern is colourful, not a blank frame
            let px = buf.as_slice();
            assert!(px.iter().any(|p| p.r > 128) && px.iter().any(|p| p.b > 128));
        }
        assert!(is_avif(include_bytes!("../tests/fixtures/thumb.avif")));
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(decode_square(b"not an image at all", 32).is_none());
        assert!(!is_avif(b"short"));
    }
}
