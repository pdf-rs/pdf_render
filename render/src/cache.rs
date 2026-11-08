use std::sync::Arc;

use font::Encoder;
use log::info;
use pathfinder_color::ColorU;
use pdf::object::*;
use pdf::primitive::Name;
use pdf::font::{Font as PdfFont};
use pdf::error::{Result};
use peniko::{Blob, ImageAlphaType, ImageBrush, ImageData, ImageFormat};
use std::slice;

use pathfinder_content::{
    pattern::{Image},
};

use crate::font::GlyphData;
use crate::BlendMode;

use super::{fontentry::FontEntry};
use super::image::load_image;
use super::font::{load_font, StandardCache};
use globalcache::{sync::SyncCache, ValueSize};

#[derive(Clone)]
pub struct ImageResult(pub Arc<Result<ImageBrush>>);
impl ValueSize for ImageResult {
    fn size(&self) -> usize {
        match *self.0 {
            Ok(ref im) => im.image.data.len(),
            Err(_) => 1,
        }
    }
}

impl ImageResult
{
    /// The pixels as straight RGBA8, `width * height * 4` bytes, first row on
    /// top — what an SVG host needs to make a PNG of.
    ///
    /// `image.data` is already a byte blob (`w * h * 4`); it is not a slice of
    /// `ColorU` any more, and reading it as one would hand out a slice four
    /// times too long, pointing past the end of the allocation. That is how
    /// every raster layer of every page came out of `svg::combine` as a failed
    /// PNG encode: the layer was right, its length was not.
    pub fn rgba_data(&self) -> Option<(Arc<&'static [u8]>, u32, u32)> {
        match *self.0 {
            Ok(ref im) => {
                let bytes: &[u8] = im.image.data.data();
                let data = unsafe {
                    std::slice::from_raw_parts(bytes.as_ptr(), bytes.len())
                };

                Some((Arc::from(data), im.image.width, im.image.height))
            },
            Err(_) => None,
        }
    }
}

#[inline]
pub fn color_slice_to_u8_slice(slice: &[ColorU]) -> &[u8] {
    unsafe {
        slice::from_raw_parts(slice.as_ptr() as *const u8, slice.len() * 4)
    }
}

pub struct Cache<E: Encoder> {
    // shared mapping of fontname -> font
    fonts: Arc<SyncCache<usize, Option<Arc<FontEntry<E>>>>>,
    images: Arc<SyncCache<(Ref<XObject>, BlendMode), ImageResult>>,
    std: StandardCache<E>,
    missing_fonts: Vec<Name>,
    encoder: E,
    /// Glyph outlines already turned into SVG path data, by the address of the
    /// font entry and of the outline inside it. Both live as long as the font
    /// is in the cache, which is longer than any one page. Rendering a page of
    /// text offers the same glyph dozens of times and the old code built its
    /// path string every single time: this is where a page turn's milliseconds
    /// were going.
    glyph_paths: std::collections::HashMap<(usize, usize), Arc<str>>,
}

/// Past this many glyphs the strings are worth more than the lookups. Two
/// thousand distinct glyphs is a huge font set; a document that passes it
/// starts again with an empty table.
const GLYPH_PATHS_MAX: usize = 20_000;

impl<E: Encoder> Cache<E> {
    /// `d` for the glyph at `font`/`outline`, if it was built before.
    pub fn glyph_path(&self, font: usize, outline: usize) -> Option<Arc<str>> {
        self.glyph_paths.get(&(font, outline)).cloned()
    }

    pub fn put_glyph_path(&mut self, font: usize, outline: usize, d: Arc<str>) {
        if self.glyph_paths.len() >= GLYPH_PATHS_MAX {
            self.glyph_paths.clear();
        }
        self.glyph_paths.insert((font, outline), d);
    }
}
impl<E: Encoder + 'static> Cache<E> where E::GlyphRef: Send + Sync {
    pub fn new(encoder: E) -> Cache<E> {
        Cache {
            fonts: SyncCache::new(),
            images: SyncCache::new(),
            std: StandardCache::new(),
            missing_fonts: Vec::new(),
            encoder,
            glyph_paths: std::collections::HashMap::new(),
        }
    }

    /// A cache whose standard fonts are already in memory — the browser case,
    /// where the font bundle is fetched alongside the document and every font
    /// in it is loaded whatever the document uses.
    pub fn with_standard(encoder: E, std: StandardCache<E>) -> Cache<E> {
        Cache {
            fonts: SyncCache::new(),
            images: SyncCache::new(),
            std,
            missing_fonts: Vec::new(),
            encoder,
            glyph_paths: std::collections::HashMap::new(),
        }
    }

    /// Replace the standard fonts, e.g. when the bundle arrives after the
    /// cache was made. Fonts already looked up are looked up again.
    pub fn set_standard(&mut self, std: StandardCache<E>) {
        self.std = std;
        self.fonts = SyncCache::new();
    }
    pub fn get_font(&mut self, pdf_font: &MaybeRef<PdfFont>, resolve: &impl Resolve) -> Result<Option<Arc<FontEntry<E>>>> {
        let mut error = None;
        let val = self.fonts.get(&**pdf_font as *const PdfFont as usize, |_| 
            match load_font(&mut self.encoder, pdf_font, resolve, &self.std) {
                Ok(Some(f)) => Some(Arc::new(f)),
                Ok(None) => {
                    if let Some(ref name) = pdf_font.name {
                        self.missing_fonts.push(name.clone());
                    }
                    None
                },
                Err(e) => {
                    error = Some(e);
                    None
                }
            }
        );
        match error {
            None => Ok(val),
            Some(e) => Err(e)
        }
    }

    pub fn get_image(&mut self, xobject_ref: Ref<XObject>, im: &ImageXObject, resources: &Resources, resolve: &impl Resolve, mode: BlendMode) -> ImageResult {
        self.images.get((xobject_ref, mode), |_| {
            let im = match load_image(im, resources, resolve, mode) {
                Ok(im) => im,
                Err(e) => return ImageResult(Arc::new(Err(e)))
            };
            let data = ImageData {
                alpha_type: ImageAlphaType::Alpha,
                data: Blob::new(Arc::new(im.rgba_data().to_owned())),
                format: ImageFormat::Rgba8,
                height: im.height(),
                width: im.width()
            };
            let brush = ImageBrush::new(data);
            ImageResult(Arc::new(Ok(brush)))
        })
    }
}
impl<E: Encoder> Drop for Cache<E> {
    fn drop(&mut self) {
        info!("missing fonts:");
        for name in self.missing_fonts.iter() {
            info!("{}", name.as_str());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_brush(rgba: Vec<u8>, width: u32, height: u32) -> ImageBrush {
        ImageBrush::new(ImageData {
            alpha_type: ImageAlphaType::Alpha,
            data: Blob::new(Arc::new(rgba)),
            format: ImageFormat::Rgba8,
            height,
            width,
        })
    }

    /// The pixels handed to an SVG host are `width * height * 4` bytes — not
    /// four times that, which is what a `ColorU` slice would have been and what
    /// made every raster layer in the browser a PNG that would not encode.
    #[test]
    fn an_image_hands_out_its_pixels_and_nothing_more() {
        let pixels = [255u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 9, 9, 9, 0];
        let result = ImageResult(Arc::new(Ok(image_brush(pixels.to_vec(), 2, 2))));
        let (data, width, height) = result.rgba_data().expect("rgba");
        assert_eq!((width, height), (2, 2));
        assert_eq!(&*data, &pixels);
        assert_eq!(result.size(), pixels.len());
    }

    #[test]
    fn a_failed_image_has_no_pixels() {
        let result = ImageResult(Arc::new(Err(pdf::error::PdfError::Other { msg: "no".into() })));
        assert!(result.rgba_data().is_none());
        assert_eq!(result.size(), 1);
    }
}
