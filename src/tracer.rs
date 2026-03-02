use crate::{BBox, Backend, DrawMode, Fill, FontEntry, TextSpan, backend::{BlendMode, FillMode, Stroke}, font::FontRc};
use pathfinder_content::{
    fill::FillRule, outline::Outline, stroke::StrokeStyle
};
use pathfinder_geometry::{
    rect::RectF,
    transform2d::Transform2F,
    vector::Vector2F,
};
use pdf::object::{Ref, XObject, ImageXObject, Resolve, Resources, MaybeRef};
use font::{Encoder, Glyph, Pen};
use pdf::font::Font as PdfFont;
use pdf::error::PdfError;
use std::sync::Arc;
use std::path::PathBuf;
use crate::font::{load_font, StandardCache};
use globalcache::sync::SyncCache;

pub struct ClipPath {
    pub path: Outline,
    pub fill_rule: FillRule,
    pub parent: Option<ClipPathId>,
}

#[derive(Copy, Clone, Debug)]
pub struct ClipPathId(pub usize);

pub struct NullPen;
impl Pen for NullPen {
    fn close(&mut self) {

    }
    fn cubic_to(&mut self, p1: Vector2F, p2: Vector2F, p3: Vector2F) {

    }
    fn line_to(&mut self, p: Vector2F) {

    }
    fn move_to(&mut self, p: Vector2F) {

    }
    fn quad_to(&mut self, p1: Vector2F, p2: Vector2F) {

    }
}

#[derive(Clone, Default)]
pub struct NullEncoder;
impl Encoder for NullEncoder {
    type Pen<'a> = NullPen;
    type GlyphRef = ();
    fn encode_shape<'f, O, E>(&mut self, mut f: impl for<'a> FnMut(&mut Self::Pen<'a>) -> Result<O, E> + 'f) -> Result<(O, Self::GlyphRef), E> {
        let o = f(&mut NullPen)?;
        Ok((o, ()))
    }
}

pub struct Tracer<'a, E: Encoder> {
    pub items: Vec<DrawItem<E>>,
    clip_paths: &'a mut Vec<ClipPath>,
    pub view_box: RectF,
    cache: &'a TraceCache<E>,
    op_nr: usize,
}
pub struct TraceCache<E: Encoder> {
    fonts: Arc<SyncCache<u64, Option<Arc<FontEntry<E>>>>>,
    std: StandardCache<E>,
}
fn font_key(font_ref: &MaybeRef<PdfFont>) -> u64 {
    match font_ref {
        MaybeRef::Direct(ref shared) => shared.as_ref() as *const PdfFont as _,
        MaybeRef::Indirect(re) => re.get_ref().get_inner().id as _
    }
}
impl<E> TraceCache<E>
where
    E: Encoder + Default + 'static,
    E::GlyphRef: Sync + Send
{
    pub fn new() -> Self {
        let standard_fonts = PathBuf::from(std::env::var_os("STANDARD_FONTS").expect("STANDARD_FONTS is not set. Please check https://github.com/pdf-rs/pdf_render/#fonts for instructions."));

        TraceCache {
            fonts: SyncCache::new(),
            std: StandardCache::new(standard_fonts),
        }
    }
    pub fn get_font(&self, font_ref: &MaybeRef<PdfFont>, resolve: &impl Resolve) -> Result<Option<Arc<FontEntry<E>>>, PdfError> {
        let mut error = None;

        let val = self.fonts.get(font_key(font_ref), |_| {
            let mut encoder = E::default();
                match load_font(&mut encoder, font_ref, resolve, &self.std) {
                    Ok(Some(f)) => Some(Arc::new(f)),
                    Ok(None) => None,
                    Err(e) => {
                        error = Some(e);
                        None
                    }
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
impl<'a, E: Encoder> Tracer<'a, E> {
    pub fn new(cache: &'a TraceCache<E>, clip_paths: &'a mut Vec<ClipPath>) -> Self {
        Tracer {
            items: vec![],
            view_box: RectF::new(Vector2F::zero(), Vector2F::zero()),
            cache,
            op_nr: 0,
            clip_paths,
        }
    }
    pub fn finish(self) -> Vec<DrawItem<E>> {
        self.items
    }
    pub fn view_box(&self) -> RectF {
        self.view_box
    }
}
impl<'a, E> Backend for Tracer<'a, E>
    where
        E: Encoder + Clone + Default + 'static,
        E::GlyphRef: Sync + Send + 'static
{
    type ClipPathId = ClipPathId;
    type Encoder = E;
    type Image = ();

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
    fn draw_glyph(&mut self, font: &FontRc<Self::Encoder>, glyph: &Glyph<Self::Encoder>, mode: &DrawMode, transform: Transform2F, clip: Option<Self::ClipPathId>) {}
    fn get_font(&mut self, font_ref: &MaybeRef<PdfFont>, resolve: &impl Resolve) -> Result<Option<Arc<FontEntry<E>>>, PdfError> {
        self.cache.get_font(font_ref, resolve)
    }
    fn add_text(&mut self, span: TextSpan<E>, clip: Option<Self::ClipPathId>) {
        self.items.push(DrawItem::Text(span, clip));
    }
    fn inspect_op(&mut self, op_nr: usize, op: &pdf::content::Op) {
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
pub enum DrawItem<E: Encoder> {
    Vector(VectorPath),
    Image(ImageObject),
    InlineImage(InlineImageObject),
    Text(TextSpan<E>, Option<ClipPathId>),
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
