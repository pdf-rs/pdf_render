use vello::kurbo::{Affine, Rect as RectF};

use pathfinder_content::outline::Outline;

use vello::peniko::{Fill as FillRule};
use pdf::{object::{Ref, XObject, ImageXObject, Resolve, Resources, MaybeRef}, content::Op};
use pdf::error::PdfError;
use font::{Glyph, Encoder};

use super::{FontEntry, TextSpan, Fill};
use pdf::font::Font as PdfFont;
use std::sync::Arc;

#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
pub enum BlendMode {
    Overlay,
    Darken
}

pub trait Backend {
    type Encoder: Encoder;
    type ClipPathId: Copy;

    fn create_clip_path(&mut self, path: Outline, fill_rule: FillRule, parent: Option<Self::ClipPathId>) -> Self::ClipPathId;
    fn draw(&mut self, outline: &Outline, mode: &DrawMode, fill_rule: FillRule, transform: Affine, clip: Option<Self::ClipPathId>);
    fn set_view_box(&mut self, r: RectF);
    fn draw_image(&mut self, xref: Ref<XObject>, im: &ImageXObject, resources: &Resources, transform: Affine, mode: BlendMode, clip: Option<Self::ClipPathId>, resolve: &impl Resolve);
    fn draw_inline_image(&mut self, im: &Arc<ImageXObject>, resources: &Resources, transform: Affine, mode: BlendMode, clip: Option<Self::ClipPathId>, resolve: &impl Resolve);
    fn draw_glyph(&mut self, glyph: &Glyph<Self::Encoder>, mode: &DrawMode, transform: Affine, clip: Option<Self::ClipPathId>) {
        self.draw(&glyph.path, mode, FillRule::EvenOdd, transform, clip);
    }
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
pub enum DrawMode {
    Fill { fill_mode: FillMode },
    Stroke { fill_mode: FillMode, stroke_style: StrokeStyle },
    FillStroke { fill_mode: FillMode, stroke_style: StrokeStyle, stroke_mode: FillMode},
}

impl DrawMode {
    pub fn fill(&self) -> Option<&FillMode> {
        match self {
            DrawMode::Fill { fill_mode } | DrawMode::FillStroke { fill_mode, .. } => Some(fill_mode),
            _ => None
        }
    }
    pub fn stroke(&self) -> Option<(&FillMode, &StrokeStyle)> {
        match self {
            DrawMode::FillStroke { fill_mode, stroke_style, .. } | DrawMode::Stroke { fill_mode, stroke_style } => Some((fill_mode, stroke_style)),
            _ => None
        }
    }
}


/// How an outline should be stroked.
#[derive(Clone, Debug, PartialEq)]
pub struct StrokeStyle {
    pub dash_pattern: Option<(Vec<f32>, f32)>,

    /// The width of the stroke in scene units.
    pub line_width: f32,
    /// The shape of the ends of the stroke.
    pub line_cap: LineCap,
    /// The shape used to join two line segments where they meet.
    pub line_join: LineJoin,
}

/// The shape of the ends of the stroke.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineCap {
    /// The ends of lines are squared off at the endpoints.
    Butt,
    /// The ends of lines are squared off by adding a box with an equal width and half the height
    /// of the line's thickness.
    Square,
    /// The ends of lines are rounded.
    Round,
}


/// The shape used to join two line segments where they meet.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineJoin {
    /// Connected segments are joined by extending their outside edges to connect at a single
    /// point, with the effect of filling an additional lozenge-shaped area. The `f32` value
    /// specifies the miter limit ratio.
    Miter(f32),
    /// Fills an additional triangular area between the common endpoint of connected segments and
    /// the separate outside rectangular corners of each segment.
    Bevel,
    /// Rounds off the corners of a shape by filling an additional sector of disc centered at the
    /// common endpoint of connected segments. The radius for these rounded corners is equal to the
    /// line width.
    Round,
}