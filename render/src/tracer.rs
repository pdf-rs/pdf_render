use crate::{TextSpan, DrawMode, Backend, FontEntry, Fill, backend::{BlendMode, FillMode}, BBox};
use pathfinder_content::{
    outline::Outline,
    fill::FillRule,
};
use pathfinder_geometry::{
    rect::RectF,
    transform2d::Transform2F,
    vector::Vector2F,
};
use pathfinder_content::{
    stroke::{StrokeStyle},
}; 
use pdf::object::{Ref, XObject, ImageXObject, Resolve, Resources, MaybeRef};
use font::Glyph;
use pdf::font::Font as PdfFont;
use pdf::error::PdfError;
use std::sync::Arc;
use std::path::PathBuf;
use crate::font::{load_font, StandardCache};
use globalcache::sync::SyncCache;
use crate::backend::Stroke;

pub struct ClipPath {
    pub path: Outline,
    pub fill_rule: FillRule,
    pub parent: Option<ClipPathId>,
}

#[derive(Copy, Clone, Debug)]
pub struct ClipPathId(pub usize);

pub struct Tracer<'a> {
    pub items: Vec<DrawItem>,
    clip_paths: &'a mut Vec<ClipPath>,
    pub view_box: RectF,
    cache: &'a TraceCache,
    op_nr: usize,
}
pub struct TraceCache {
    fonts: Arc<SyncCache<u64, Option<Arc<FontEntry>>>>,
    std: StandardCache,
}
fn font_key(font_ref: &MaybeRef<PdfFont>) -> u64 {
    match font_ref {
        MaybeRef::Direct(ref shared) => shared.as_ref() as *const PdfFont as _,
        MaybeRef::Indirect(re) => re.get_ref().get_inner().id as _
    }
}
impl TraceCache {
    pub fn new() -> Self {
        let standard_fonts = PathBuf::from(std::env::var_os("STANDARD_FONTS").expect("STANDARD_FONTS is not set. Please check https://github.com/pdf-rs/pdf_render/#fonts for instructions."));

        TraceCache {
            fonts: SyncCache::new(),
            std: StandardCache::new(standard_fonts),
        }
    }
    pub fn get_font(&self, font_ref: &MaybeRef<PdfFont>, resolve: &impl Resolve) -> Result<Option<Arc<FontEntry>>, PdfError> {
        let mut error = None;
        let val = self.fonts.get(font_key(font_ref), || 
            match load_font(font_ref, resolve, &self.std) {
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
    pub fn require_unique_unicode(&mut self, require_unique_unicode: bool) {
        self.std.require_unique_unicode(require_unique_unicode);
    }
}
impl<'a> Tracer<'a> {
    pub fn new(cache: &'a TraceCache, clip_paths: &'a mut Vec<ClipPath>) -> Self {
        Tracer {
            items: vec![],
            view_box: RectF::new(Vector2F::zero(), Vector2F::zero()),
            cache,
            op_nr: 0,
            clip_paths,
        }
    }
    pub fn finish(self) -> Vec<DrawItem> {
        self.items
    }
    pub fn view_box(&self) -> RectF {
        self.view_box
    }
}
impl<'a> Backend for Tracer<'a> {
    type ClipPathId = ClipPathId;

    fn create_clip_path(&mut self, path: Outline, fill_rule: FillRule, parent: Option<ClipPathId>) -> ClipPathId {
        let id = ClipPathId(self.clip_paths.len());
        self.clip_paths.push(ClipPath {
            path,
            fill_rule,
            parent,
        });
        id
    }
    fn draw(&mut self, outline: &Outline, mode: &DrawMode, _fill_rule: FillRule, transform: Transform2F, clip: Option<ClipPathId>) {
        let stroke = match mode {
            DrawMode::FillStroke { stroke, stroke_mode, .. } | DrawMode::Stroke { stroke, stroke_mode } => Some((stroke.clone(), stroke_mode.clone())),
            DrawMode::Fill { .. } => None,
        };
        self.items.push(DrawItem::Vector(VectorPath {
            outline: outline.clone(),
            fill: match mode {
                DrawMode::Fill { fill } | DrawMode::FillStroke { fill, .. } => Some(fill.clone()),
                _ => None
            },
            stroke,
            transform,
            clip,
            op_nr: self.op_nr,
        }));
    }
    fn set_view_box(&mut self, r: RectF) {
        self.view_box = r;
    }
    fn draw_image(&mut self, xref: Ref<XObject>, _im: &ImageXObject, _resources: &Resources, transform: Transform2F, mode: BlendMode, clip: Option<ClipPathId>, _resolve: &impl Resolve) {
        let rect = transform * RectF::new(
            Vector2F::new(0.0, 0.0), Vector2F::new(1.0, 1.0)
        );
        self.items.push(DrawItem::Image(ImageObject {
            rect, id: xref, transform, op_nr: self.op_nr, mode, clip
        }));
    }
    fn draw_inline_image(&mut self, im: &Arc<ImageXObject>, _resources: &Resources, transform: Transform2F, mode: BlendMode, clip: Option<ClipPathId>, _resolve: &impl Resolve) {
        let rect = transform * RectF::new(
            Vector2F::new(0.0, 0.0), Vector2F::new(1.0, 1.0)
        );

        self.items.push(DrawItem::InlineImage(InlineImageObject {
            rect, im: im.clone(), transform, op_nr: self.op_nr, mode, clip
        }));
    }
    fn draw_glyph(&mut self, _glyph: &Glyph, _mode: &DrawMode, _transform: Transform2F, clip: Option<ClipPathId>) {}
    fn get_font(&mut self, font_ref: &MaybeRef<PdfFont>, resolve: &impl Resolve) -> Result<Option<Arc<FontEntry>>, PdfError> {
        self.cache.get_font(font_ref, resolve)
    }
    fn add_text(&mut self, span: TextSpan, clip: Option<Self::ClipPathId>) {
        self.items.push(DrawItem::Text(span, clip));
    }
    fn bug_op(&mut self, op_nr: usize) {
        self.op_nr = op_nr;
    }
}

#[derive(Debug)]
pub struct ImageObject {
    pub rect: RectF,
    pub id: Ref<XObject>,
    pub transform: Transform2F,
    pub op_nr: usize,
    pub mode: BlendMode,
    pub clip: Option<ClipPathId>,
}
#[derive(Debug)]
pub struct InlineImageObject {
    pub rect: RectF,
    pub im: Arc<ImageXObject>,
    pub transform: Transform2F,
    pub op_nr: usize,
    pub mode: BlendMode,
    pub clip: Option<ClipPathId>,
}

#[derive(Debug)]
pub enum DrawItem {
    Vector(VectorPath),
    Image(ImageObject),
    InlineImage(InlineImageObject),
    Text(TextSpan, Option<ClipPathId>),
}

#[derive(Debug)]
pub struct VectorPath {
    pub outline: Outline,
    pub fill: Option<FillMode>,
    pub stroke: Option<(FillMode, Stroke)>,
    pub transform: Transform2F,
    pub op_nr: usize,
    pub clip: Option<ClipPathId>,
}
