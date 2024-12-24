use std::path::{PathBuf};
use std::sync::Arc;

use pdf::object::*;
use pdf::primitive::Name;
use pdf::font::{Font as PdfFont};
use pdf::error::{Result};

use pathfinder_geometry::{
    vector::{Vector2I},
};
use pathfinder_content::{
    pattern::{Image},
};

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

pub struct Cache {
    // shared mapping of fontname -> font
    fonts: Arc<SyncCache<usize, Option<Arc<FontEntry>>>>,
    images: Arc<SyncCache<(Ref<XObject>, BlendMode), ImageResult>>,
    std: StandardCache,
    missing_fonts: Vec<Name>,
}

impl Cache {
    pub fn new() -> Cache {
        Cache {
            fonts: SyncCache::new(),
            images: SyncCache::new(),
            std: StandardCache::new(),
            missing_fonts: Vec::new(),
        }
    }
    pub fn get_font(&mut self, pdf_font: &MaybeRef<PdfFont>, resolve: &impl Resolve) -> Result<Option<Arc<FontEntry>>, > {
        let mut error = None;
        let val = self.fonts.get(&**pdf_font as *const PdfFont as usize, |_| 
            match load_font(pdf_font, resolve, &mut self.std) {
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
        self.images.get((xobject_ref, mode), |_|
            ImageResult(Arc::new(load_image(im, resources, resolve, mode).map(|image|
                Image::new(Vector2I::new(im.width as i32, im.height as i32), Arc::new(image.into_data().into()))
            )))
        )
    }
}
impl Drop for Cache {
    fn drop(&mut self) {
        info!("missing fonts:");
        for name in self.missing_fonts.iter() {
            info!("{}", name.as_str());
        }
    }
}
