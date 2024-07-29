use pathfinder_geometry::{
    transform2d::Transform2F,
    rect::RectF,
};
use pathfinder_content::{
    fill::FillRule,
    stroke::{StrokeStyle},
    outline::Outline,
};

use pdf::{content::{Op, Point, ViewRect}, object::{ImageXObject, MaybeRef, Ref, Resolve, Resources, XObject}};
use pdf::error::PdfError;
use font::{Encoder, Glyph};
use crate::font::FontRc;

use super::{FontEntry, TextSpan, Fill};
use pdf::font::Font as PdfFont;
use std::sync::Arc;

#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
pub enum BlendMode {
    Overlay,
    Darken
}

pub trait Backend {
    type Encoder: Encoder + Clone + 'static;
    type ClipPathId: Copy;

    fn create_clip_path(&mut self, path: Outline, fill_rule: FillRule, parent: Option<Self::ClipPathId>) -> Self::ClipPathId;
    fn draw(&mut self, outline: &Outline, mode: &DrawMode, fill_rule: FillRule, transform: Transform2F, clip: Option<Self::ClipPathId>);
    fn set_view_box(&mut self, r: RectF, page_nr: u32);
    fn draw_image(&mut self, xref: Ref<XObject>, im: &ImageXObject, resources: &Resources, transform: Transform2F, mode: BlendMode, clip: Option<Self::ClipPathId>, resolve: &impl Resolve);
    fn draw_inline_image(&mut self, im: &Arc<ImageXObject>, resources: &Resources, transform: Transform2F, mode: BlendMode, clip: Option<Self::ClipPathId>, resolve: &impl Resolve);
    fn draw_glyph(&mut self, font: &FontRc<Self::Encoder>, glyph: &Glyph<Self::Encoder>, mode: &DrawMode, transform: Transform2F, clip: Option<Self::ClipPathId>);
    fn get_font(&mut self, font_ref: &MaybeRef<PdfFont>, resolve: &impl Resolve) -> Result<Option<Arc<FontEntry<Self::Encoder>>>, PdfError>;
    fn add_text(&mut self, span: TextSpan<Self::Encoder>, clip: Option<Self::ClipPathId>);

    /// The following functions are for debugging PDF files and not relevant for rendering them.
    fn bug_text_no_font(&mut self, data: &[u8]) {}
    fn bug_text_invisible(&mut self, text: &str) {}
    fn bug_postscript(&mut self, data: &[u8]) {}
    fn bug_op(&mut self, op_nr: usize) {}
    fn inspect_op(&mut self, op: &Op) {}
}
#[derive(Clone, Debug)]

pub struct FillMode {
    pub color: Fill,
    pub alpha: f32,
    pub mode: BlendMode,
}

#[derive(Debug)]
pub enum DrawMode {
    Fill { fill: FillMode },
    Stroke { stroke: FillMode, stroke_mode: Stroke },
    FillStroke { fill: FillMode, stroke: FillMode, stroke_mode: Stroke },
}
impl DrawMode {
    pub fn fill(&self) -> Option<&FillMode> {
        match self {
            DrawMode::Fill { fill } | DrawMode::FillStroke { fill, .. } => Some(fill),
            _ => None
        }
    }
    pub fn stroke(&self) -> Option<(&FillMode, &Stroke)> {
        match self {
            DrawMode::FillStroke { stroke, stroke_mode, .. } | DrawMode::Stroke { stroke, stroke_mode } => Some((stroke, stroke_mode)),
            _ => None
        }
    }
}


#[derive(Clone, Debug)]
pub struct Stroke {
    pub dash_pattern: Option<(Vec<f32>, f32)>,
    pub style: StrokeStyle,
}
