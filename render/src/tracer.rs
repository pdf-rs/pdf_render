use crate::{TextSpan, DrawMode, Backend, FontEntry};
use pathfinder_content::{
    outline::Outline,
    fill::FillRule,
};
use pathfinder_color::{ColorU, ColorF};
use pathfinder_geometry::{
    rect::RectF,
    transform2d::Transform2F,
    vector::Vector2F,
};
use pdf::object::{Ref, XObject, ImageXObject, Resolve, PlainRef};
use font::Glyph;
use pdf::font::Font as PdfFont;
use pdf::error::PdfError;
use std::rc::Rc;
use std::path::PathBuf;
use std::collections::HashMap;
use crate::font::{load_font, StandardCache};

pub struct Tracer<'a> {
    items: Vec<DrawItem>,
    view_box: RectF,
    cache: &'a mut TraceCache,
}
pub struct TraceCache {
    standard_fonts: PathBuf,
    fonts: HashMap<Ref<PdfFont>, Option<Rc<FontEntry>>>,
    std: StandardCache,
}
impl TraceCache {
    pub fn new() -> Self {
        TraceCache {
            standard_fonts: PathBuf::from(std::env::var_os("STANDARD_FONTS").expect("no STANDARD_FONTS")),
            fonts: HashMap::new(),
            std: StandardCache::new(),
        }
    }
}
impl<'a> Tracer<'a> {
    pub fn new(cache: &'a mut TraceCache) -> Self {
        Tracer {
            items: vec![],
            view_box: RectF::new(Vector2F::zero(), Vector2F::zero()),
            cache
        }
    }
    pub fn view_box(&self) -> RectF {
        self.view_box
    }
    pub fn finish(self) -> Vec<DrawItem> {
        self.items
    }
}
impl<'a> Backend for Tracer<'a> {
    fn set_clip_path(&mut self, path: &Outline) {}
    fn draw(&mut self, outline: &Outline, mode: DrawMode, fill_rule: FillRule, transform: Transform2F) {
        let stroke = match mode {
            DrawMode::FillStroke(_, c, s) | DrawMode::Stroke(c, s) => Some((c.to_u8(), s.line_width)),
            DrawMode::Fill(_) => None,
        };
        self.items.push(DrawItem::Vector(VectorPath {
            outline: outline.clone(),
            fill: match mode {
                DrawMode::Fill(c) | DrawMode::FillStroke(c, _, _) => Some(c.to_u8()),
                _ => None
            },
            stroke,
            transform,
        }));
    }
    fn set_view_box(&mut self, r: RectF) {
        self.view_box = r;
    }
    fn draw_image(&mut self, xref: Ref<XObject>, im: &ImageXObject, transform: Transform2F, resolve: &impl Resolve) {
        let rect = transform * RectF::new(
            Vector2F::new(0.0, 0.0), Vector2F::new(1.0, 1.0)
        );
        self.items.push(DrawItem::Image(ImageObject {
            rect, id: xref,
        }));
    }
    fn draw_glyph(&mut self, glyph: &Glyph, mode: DrawMode, transform: Transform2F) {}
    fn get_font(&mut self, font_ref: Ref<PdfFont>, resolve: &impl Resolve) -> Result<Option<Rc<FontEntry>>, PdfError> {
        use std::collections::hash_map::Entry;
        match self.cache.fonts.entry(font_ref) {
            Entry::Occupied(e) => Ok(e.get().clone()),
            Entry::Vacant(entry) => {
                match load_font(font_ref, resolve, self.cache.standard_fonts.as_ref(), &mut self.cache.std) {
                    Ok(f) => {
                        Ok(entry.insert(f.clone()).clone())
                    }
                    Err(e) => {
                        entry.insert(None);
                        Err(e)
                    }
                }
            }
        }
    }
    fn add_text(&mut self, span: TextSpan) {
        self.items.push(DrawItem::Text(span));
    }
}

#[derive(Debug)]
pub struct ImageObject {
    pub rect: RectF,
    pub id: Ref<XObject>,
}

pub struct TraceResults {
    pub draw: Vec<DrawItem>,
}
impl TraceResults {
    pub fn texts(&self) -> impl Iterator<Item=&TextSpan> {
        self.draw.iter().filter_map(|i| match i {
            DrawItem::Text(t) => Some(t),
            _ => None
        })
    }
    pub fn images(&self) -> impl Iterator<Item=&ImageObject> {
        self.draw.iter().filter_map(|i| match i {
            DrawItem::Image(i) => Some(i),
            _ => None
        })
    }
    pub fn paths(&self) -> impl Iterator<Item=&VectorPath> {
        self.draw.iter().filter_map(|i| match i {
            DrawItem::Vector(p) => Some(p),
            _ => None
        })
    }
}
#[derive(Debug)]
pub enum DrawItem {
    Vector(VectorPath),
    Image(ImageObject),
    Text(TextSpan),
}

#[derive(Debug)]
pub struct VectorPath {
    pub outline: Outline,
    pub fill: Option<ColorU>,
    pub stroke: Option<(ColorU, f32)>,
    pub transform: Transform2F,
}
