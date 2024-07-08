use std::path::{PathBuf};
use std::sync::Arc;

use font::Encoder;
use pdf::object::*;
use pdf::primitive::Name;
use pdf::font::{Font as PdfFont};
use pdf::error::{Result};

use vello::peniko::{Image, Format};

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
            Ok(ref im) => im.format.size_in_bytes(im.width, im.height).unwrap(),
            Err(_) => 1,
        }
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
            match load_font(&mut self.encoder, resolve, &mut self.std) {
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
                Image::new( Arc::new(image.into_data().into()), Format::Rgba8, im.width as i32, im.height as i32)
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
