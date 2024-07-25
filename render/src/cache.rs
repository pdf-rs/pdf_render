use std::path::{PathBuf};
use std::sync::Arc;

use font::Encoder;
use pathfinder_color::ColorU;
use pdf::object::*;
use pdf::primitive::Name;
use pdf::font::{Font as PdfFont};
use pdf::error::{Result};
use std::slice;

use pathfinder_geometry::{
    vector::{Vector2I},
};
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
pub struct ImageResult(pub Arc<Result<Image>>);
impl ValueSize for ImageResult {
    fn size(&self) -> usize {
        match *self.0 {
            Ok(ref im) => im.pixels().len() * 4,
            Err(_) => 1,
        }
    }
}

impl ImageResult
{
    pub fn rgba_data(&self) -> Option<(Arc<&'static [u8]>, u32, u32)> {
        match *self.0 {
            Ok(ref im) => {
                let len = im.pixels().len();
                let data = unsafe {
                    std::slice::from_raw_parts(im.pixels().as_ptr()  as *const u8, 4 * len)
                };

                Some((Arc::from(data), im.size().x() as u32, im.size().y() as u32))
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
}
impl<E: Encoder + 'static> Cache<E> where E::GlyphRef: Send + Sync {
    pub fn new(encoder: E) -> Cache<E> {
        let standard_fonts;
        if let Some(path) = std::env::var_os("STANDARD_FONTS") {
            standard_fonts = PathBuf::from(path);
        } else {
            eprintln!("PDF: STANDARD_FONTS not set. using fonts/ instead.");
            standard_fonts = PathBuf::from("fonts");
        }
        if !standard_fonts.is_dir() {
            panic!("STANDARD_FONTS (or fonts/) is not directory.");
        }
        Cache {
            fonts: SyncCache::new(),
            images: SyncCache::new(),
            std: StandardCache::new(standard_fonts),
            missing_fonts: Vec::new(),
            encoder,
        }
    }
    pub fn get_font(&mut self, pdf_font: &MaybeRef<PdfFont>, resolve: &impl Resolve) -> Result<Option<Arc<FontEntry<E>>>> {
        let mut error = None;
        let val = self.fonts.get(&**pdf_font as *const PdfFont as usize, || 
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
        self.images.get((xobject_ref, mode), ||
            ImageResult(Arc::new(load_image(im, resources, resolve, mode).map(|image|
                Image::new(Vector2I::new(im.width as i32, im.height as i32), Arc::new(image.into_data().into()))
            )))
        )
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
