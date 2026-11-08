//! Every image on a page, decoded and encoded through the dependency set the
//! viewer is built with: `pdf_render` without `native`, `image` with only the
//! `png` codec.
//!
//!     cargo run --example images -- nasa.pdf 58
//!
//! A picture that fails here is a picture the browser never shows, and it says
//! so nowhere: the SVG backend leaves out an image it cannot decode, and
//! `svg::combine` leaves out a layer it cannot turn into a PNG. Both are
//! deliberate — a page with a broken figure still reads — and both are silent,
//! so this is the check that has to be run by hand.

use std::path::Path;

use pdf_render::pdf::file::FileOptions;
use pdf_render::pdf::object::{Resolve, XObject};
use pdf_render::{BlendMode, load_image};

fn main() {
    if std::env::var_os("STANDARD_FONTS").is_none() {
        std::env::set_var("STANDARD_FONTS", "/home/hematite/task-clones/pdf_render/standard_fonts");
    }
    let a: Vec<String> = std::env::args().collect();
    let file = FileOptions::cached()
        .parse_options(pdf_web::viewer_options())
        .open(Path::new(&a[1]))
        .expect("open pdf");
    let page_index: u32 = a.get(2).map(|p| p.parse::<u32>().unwrap().saturating_sub(1)).unwrap_or(0);
    let page = file.get_page(page_index).expect("load page");
    let resolver = file.resolver();
    let Some(ref resources) = page.resources else {
        println!("page {}: no resources", page_index + 1);
        return;
    };
    let resources = &**resources.data();
    let mut n = 0;
    for (name, xobject) in &resources.xobjects {
        let Ok(object) = resolver.get(*xobject) else { continue };
        let XObject::Image(ref image) = *object else { continue };
        n += 1;
        let (w, h) = (image.width, image.height);
        match load_image(image, resources, &resolver, BlendMode::Overlay) {
            Ok(data) => {
                let rgba = data.rgba_data().to_vec();
                let png = encode_png(&rgba, data.width(), data.height());
                println!(
                    "{name:>8}: {w}x{h} -> {}x{} {} bytes of RGBA, png {}",
                    data.width(),
                    data.height(),
                    rgba.len(),
                    match png {
                        Some(ref p) => format!("{} bytes", p.len()),
                        None => "FAILED".to_string(),
                    }
                );
            }
            Err(e) => println!("{name:>8}: {w}x{h} DECODE FAILED: {e}"),
        }
    }
    if n == 0 {
        println!("page {}: no images in the page's own resources", page_index + 1);
    }
}

/// The same call `svg::combine` makes, with the `image` features this crate is
/// built with.
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
