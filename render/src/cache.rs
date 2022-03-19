use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs;
use std::borrow::Cow;
use std::sync::Arc;

use pdf::file::File as PdfFile;
use pdf::object::*;
use pdf::font::{Font as PdfFont};
use pdf::error::{Result};

use pathfinder_geometry::{
    vector::{Vector2I},
};
use pathfinder_content::{
    pattern::{Image},
};

use super::{fontentry::FontEntry};
use super::image::load_image;
use super::font::{load_font, StandardCache};
use cachelib::sync::SyncCache;

pub struct Cache {
    // shared mapping of fontname -> font
    fonts: SyncCache<Ref<PdfFont>, Option<Arc<FontEntry>>>,
    standard_fonts: PathBuf,
    images: SyncCache<Ref<XObject>, Arc<Result<Image>>>,
    std: StandardCache,
}
impl Cache {
    pub fn new() -> Cache {
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
            standard_fonts,
            std: StandardCache::new(),
        }
    }
    pub fn get_font(&mut self, font_ref: Ref<PdfFont>, resolve: &impl Resolve) -> Result<Option<Arc<FontEntry>>, > {
        let mut error = None;
        let val = self.fonts.get(font_ref, || 
            match load_font(font_ref, resolve, &self.standard_fonts, &mut self.std) {
                Ok(Some(f)) => Some(Arc::new(f)),
                Ok(None) => None,
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

    pub fn get_image(&mut self, xobject_ref: Ref<XObject>, im: &ImageXObject, resolve: &impl Resolve) -> Arc<Result<Image>> {
        self.images.get(xobject_ref, ||
            Arc::new(load_image(im, resolve).map(|image|
                Image::new(Vector2I::new(im.width as i32, im.height as i32), Arc::new(image.data))
            ))
        )
    }
}
