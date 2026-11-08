//! One document per page: the vector layers and the raster ones, in paint
//! order, as a single self-contained SVG.
//!
//! The backend hands out layers because a page is not one thing: vectors are
//! emitted as SVG, rasters as raw RGBA, and the two can interleave with a
//! `flush` between them. A host that wants one image to hand to `<img>` — or
//! to save — needs them put back together, which is what this does: the image
//! go in as a PNG data URL, the layers keep their order, and the backend has
//! already scoped every `id` to its layer (`l0_g17`) and, when the host asks
//! for it, to its page (`p34_l0_g17`), so neither two layers nor two pages of
//! one document can collide, and nothing has to be renamed here.
//!
//! The alternative would be a canvas and `drawImage` per raster layer. That
//! needs JavaScript to touch pixels and loses "the page is a file you can
//! keep"; a page with a scanned photograph is the one case where the data URL
//! is a third bigger than the pixels, and it is still the smaller problem.

use pdf_render::svg_backend::{ImageLayer, Layer, SvgPage};

/// Assemble the layers of one page into a single SVG document. `scope` is the
/// page's prefix (`p34_`), or `""` for a page that will be a document of its
/// own.
pub fn combine(page: SvgPage, scope: &str) -> String {
    let [x, y, w, h] = page.view_box;
    // PDF space has y up, SVG's y down. Every vector layer flips its own
    // content inside a group; an image layer is put in the same group so both
    // speak page coordinates.
    let flip = y + y + h;

    let mut out = String::with_capacity(4096);
    // `view_box` is in millimetres (the render space), so the intrinsic size is
    // in `mm`: 215.9×279.4mm is 816×1056 CSS px, the size of the paper at 100%.
    // Unitless it would be 216 px wide — a quarter of the page, unreadable.
    out.push_str("<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"");
    num(w, &mut out);
    out.push_str("mm\" height=\"");
    num(h, &mut out);
    out.push_str("mm\" viewBox=\"");
    num(x, &mut out);
    out.push(' ');
    num(y, &mut out);
    out.push(' ');
    num(w, &mut out);
    out.push(' ');
    num(h, &mut out);
    out.push_str("\" shape-rendering=\"geometricPrecision\">");

    for (i, layer) in page.layers.iter().enumerate() {
        let prefix = format!("{scope}l{i}_");
        match layer {
            // The backend already numbers its ids per layer (`l0_g17`), so the
            // text goes in as it is: two layers cannot collide.
            Layer::Vector(doc) => match body_of(doc) {
                Some(body) => out.push_str(body),
                // A layer that is not the document it claims to be is left
                // out rather than spliced in broken.
                None => continue,
            },
            Layer::Image(image) => match image_element(image, &prefix, flip) {
                Some(element) => out.push_str(&element),
                None => continue,
            },
        }
    }
    out.push_str("</svg>");
    out
}

/// Whatever sits between the outer `<svg>` tag and its close.
fn body_of(svg: &str) -> Option<&str> {
    let start = svg.find('>')? + 1;
    let end = svg.rfind("</svg>")?;
    (start <= end).then(|| &svg[start..end])
}

/// A raster layer as an `<image>`: PNG bytes as a data URL, placed by the
/// matrix the backend computed, inside the page's y flip.
fn image_element(layer: &ImageLayer, prefix: &str, flip: f32) -> Option<String> {
    let png = encode_png(&layer.rgba, layer.width, layer.height)?;
    let data = base64(&png);

    let mut out = String::with_capacity(data.len() + 512);
    let clip_id = format!("{prefix}clip");
    if let Some(ref clip) = layer.clip {
        out.push_str("<defs><clipPath id=\"");
        out.push_str(&clip_id);
        out.push_str("\" clipPathUnits=\"userSpaceOnUse\"><path d=\"");
        out.push_str(clip);
        out.push_str("\"/></clipPath></defs>");
    }
    out.push_str("<g transform=\"matrix(1 0 0 -1 0 ");
    num(flip, &mut out);
    out.push_str(")\">");
    if layer.clip.is_some() {
        out.push_str("<g clip-path=\"url(#");
        out.push_str(&clip_id);
        out.push_str(")\">");
    }
    // The matrix maps the image's unit square; the image itself is drawn into
    // a 1×1 box, so its own resolution never enters the coordinates.
    out.push_str("<image width=\"1\" height=\"1\" preserveAspectRatio=\"none\" transform=\"matrix(");
    for (n, v) in layer.matrix.iter().enumerate() {
        if n > 0 {
            out.push(' ');
        }
        num(*v, &mut out);
    }
    out.push_str(")\" xlink:href=\"data:image/png;base64,");
    out.push_str(&data);
    out.push_str("\"/>");
    if layer.clip.is_some() {
        out.push_str("</g>");
    }
    out.push_str("</g>");
    Some(out)
}

fn encode_png(rgba: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    use image::ImageEncoder;
    if width == 0 || height == 0 || rgba.len() != width as usize * height as usize * 4 {
        return None;
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .ok()?;
    Some(png)
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// An f32 as SVG wants it: shortest form, no exponent, no trailing `.0`.
fn num(v: f32, out: &mut String) {
    use std::fmt::Write;
    if v.is_finite() {
        let _ = write!(out, "{v}");
    } else {
        out.push('0');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_assembles_its_layers_in_order() {
        let page = SvgPage {
            view_box: [0.0, 0.0, 10.0, 20.0],
            layers: vec![
                Layer::Vector("<svg xmlns=\"x\" viewBox=\"0 0 10 20\"><g><path d=\"M0 0\"/></g></svg>".into()),
                Layer::Vector("<svg xmlns=\"x\" viewBox=\"0 0 10 20\"><use href=\"#l1_g0\"/></svg>".into()),
            ],
        };
        let got = combine(page, "");
        assert!(got.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""), "{got}");
        assert!(got.contains("<path d=\"M0 0\"/>"), "{got}");
        // The body of a layer goes in whole: the backend already scoped its ids.
        assert!(got.contains("<use href=\"#l1_g0\"/>"), "{got}");
        // Layer order is paint order.
        assert!(got.find("M0 0").unwrap() < got.find("#l1_g0").unwrap(), "{got}");
        // Each layer's outer element is not repeated.
        assert_eq!(got.matches("<svg").count(), 1, "{got}");
    }

    #[test]
    fn a_document_is_reduced_to_its_body() {
        let doc = "<svg xmlns=\"x\" width=\"1\" height=\"1\" viewBox=\"0 0 1 1\"><path d=\"M0 0\"/></svg>";
        assert_eq!(body_of(doc), Some("<path d=\"M0 0\"/>"));
        assert_eq!(body_of("not an svg"), None);
    }

    #[test]
    fn base64_matches_the_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn a_png_round_trips_through_the_encoder() {
        let rgba = [255u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 9, 9, 9, 0];
        let png = encode_png(&rgba, 2, 2).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let image = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(image.as_raw(), &rgba);
        // A buffer that does not match the dimensions is refused, not encoded.
        assert!(encode_png(&rgba, 3, 2).is_none());
    }
}
