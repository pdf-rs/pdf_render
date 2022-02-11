use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs;
use std::borrow::Cow;
use std::sync::Arc;

use pdf::file::File as PdfFile;
use pdf::object::*;
use pdf::content::Op;
use pdf::backend::Backend as PdfBackend;
use pdf::font::{Font as PdfFont};
use pdf::error::{Result, PdfError};

use pathfinder_geometry::{
    vector::{Vector2F, Vector2I},
    rect::RectF, transform2d::Transform2F,
};
use pathfinder_color::ColorU;
use pathfinder_renderer::{
    scene::{DrawPath, Scene},
    paint::Paint,
};
use pathfinder_content::{
    outline::Outline,
    pattern::{Image},
};
use font::{self};
use std::rc::Rc;

use super::{BBox, fontentry::FontEntry, renderstate::RenderState, Backend};
use super::image::load_image;
use super::font::load_font;


pub type FontMap = HashMap<Ref<PdfFont>, Option<Rc<FontEntry>>>;
pub type ImageMap = HashMap<Ref<XObject>, Result<Image>>;
pub struct Cache {
    // shared mapping of fontname -> font
    fonts: FontMap,
    standard_fonts: PathBuf,
    data_dir: Option<PathBuf>,
    images: ImageMap,
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
            fonts: HashMap::new(),
            images: HashMap::new(),
            standard_fonts,
            data_dir: None,
        }
    }
    pub fn get_font(&mut self, font_ref: Ref<PdfFont>, resolve: &impl Resolve) -> Result<Option<Rc<FontEntry>>> {
        match self.fonts.entry(font_ref) {
            Entry::Occupied(e) => Ok(e.get().clone()),
            Entry::Vacant(e) => {
                let font = load_font(font_ref, resolve, &self.standard_fonts)?;
                Ok(e.insert(font).clone())
            }
        }
    }

    pub fn get_image(&mut self, xobject_ref: Ref<XObject>, im: &ImageXObject, resolve: &impl Resolve) -> &Result<Image> {
        match self.images.entry(xobject_ref) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                let img = load_image(im, resolve).map(|image|
                    Image::new(Vector2I::new(im.width, im.height), Arc::new(image.data))
                );
                e.insert(img)
            }
        }
    }
}
