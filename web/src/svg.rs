//! One document per page: the vector layers and the raster ones, in paint
//! order, as a single self-contained SVG.
//!
//! The backend hands out layers because a page is not one thing: vectors are
//! emitted as SVG, rasters as raw RGBA, and the two can interleave with a
//! `flush` between them. A host that wants one image to hand to `<img>` — or
//! to save — needs them put back together, which is what this does: the image
//! goes in as a PNG data URL, the layers keep their order, and every `id` is
//! prefixed per layer so two layers of one page cannot collide.
//!
//! The alternative would be a canvas and `drawImage` per raster layer. That
//! needs JavaScript to touch pixels and loses "the page is a file you can
//! keep"; a page with a scanned photograph is the one case where the data URL
//! is a third bigger than the pixels, and it is still the smaller problem.

use pdf_render::svg_backend::{ImageLayer, Layer, SvgPage};

/// Assemble the layers of one page into a single SVG document.
pub fn combine(page: SvgPage) -> String {
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
        let prefix = format!("l{i}_");
        match layer {
            Layer::Vector(doc) => match body_of(doc) {
                Some(body) => out.push_str(&prefix_ids(body, &prefix)),
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

/// Rename every `id` a layer defines, and every reference to one, so layers
/// can share a document. Only the three forms the backend emits are touched:
/// `id="…"`, `href="#…"` and `url(#…)`.
///
/// One pass, with each marker's next position looked up once: searching for all
/// three markers at every hit made this quadratic. A page with four thousand
/// `<use href="#gN">` and a marker that occurs twice scanned the rest of the
/// page thousands of times, and that search was 93 % of the time a page took to
/// render — 400 ms of a 430 ms page.
fn prefix_ids(fragment: &str, prefix: &str) -> String {
    const MARKERS: [&str; 3] = ["id=\"", "href=\"#", "url(#"];
    fn is_ident_byte(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'_'
    }
    let bytes = fragment.as_bytes();
    let mut out = String::with_capacity(fragment.len() + 64);
    let mut next: [Option<usize>; 3] = [
        fragment.find(MARKERS[0]),
        fragment.find(MARKERS[1]),
        fragment.find(MARKERS[2]),
    ];
    let mut i = 0;
    while i < bytes.len() {
        // The earliest marker still ahead of us.
        let mut hit: Option<(usize, usize)> = None;
        for (k, at) in next.iter().enumerate() {
            if let Some(p) = *at {
                if p >= i && hit.is_none_or(|(q, _)| p < q) {
                    hit = Some((p, k));
                }
            }
        }
        let Some((p, k)) = hit else {
            out.push_str(&fragment[i..]);
            return out;
        };
        out.push_str(&fragment[i..p + MARKERS[k].len()]);
        i = p + MARKERS[k].len();
        // The identifier as the backend writes it: `g<number>` or `c<number>`.
        let mut end = i;
        while end < bytes.len() && is_ident_byte(bytes[end]) {
            end += 1;
        }
        if is_backend_id(&fragment[i..end]) {
            out.push_str(prefix);
        }
        out.push_str(&fragment[i..end]);
        i = end;
        // Only the positions we walked past are looked up again.
        for (k, at) in next.iter_mut().enumerate() {
            if at.is_some_and(|p| p < i) {
                *at = fragment[i..].find(MARKERS[k]).map(|q| q + i);
            }
        }
    }
    out
}

fn is_backend_id(ident: &str) -> bool {
    let mut chars = ident.chars();
    matches!(chars.next(), Some('g') | Some('c')) && chars.next().is_some_and(|c| c.is_ascii_digit())
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
    fn ids_get_a_layer_prefix_and_nothing_else_changes() {
        let layer = "<defs><g id=\"g0\"><path d=\"M0 0\"/></g><clipPath id=\"c1\"><path d=\"M1 1\"/></clipPath></defs>\
                     <path d=\"M0 0\" fill=\"#abcdef\"/><use href=\"#g0\" transform=\"matrix(1)\"/>\
                     <g clip-path=\"url(#c1)\"><use xlink:href=\"#g0\"/></g>";
        let got = prefix_ids(layer, "l3_");
        assert!(got.contains("id=\"l3_g0\""), "{got}");
        assert!(got.contains("id=\"l3_c1\""), "{got}");
        assert!(got.contains("href=\"#l3_g0\""), "{got}");
        assert!(got.contains("url(#l3_c1)"), "{got}");
        // A colour is not an id and must survive untouched.
        assert!(got.contains("fill=\"#abcdef\""), "{got}");
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
