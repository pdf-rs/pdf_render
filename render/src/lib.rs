#[macro_use] extern crate log;
#[macro_use] extern crate pdf;

macro_rules! assert_eq {
    ($a:expr, $b:expr) => {
        if $a != $b {
            return Err(pdf::error::PdfError::Other { msg: format!("{} ({}) != {} ({})", stringify!($a), $a, stringify!($b), $b)});
        }

    };
}

macro_rules! unimplemented {
    ($msg:tt $(, $arg:expr)*) => {
        return Err(pdf::error::PdfError::Other { msg: format!(concat!("Unimplemented: ", $msg) $(, $arg)*) })
    };
}

mod cache;
mod fontentry;
mod graphicsstate;
mod renderstate;
mod textstate;
mod backend;
pub mod tracer;
mod image;
// mod pathfinder_backend;
pub mod vello_backend;
mod font;

pub use cache::{Cache};
use ::font::Encoder;
pub use fontentry::{FontEntry};
pub use backend::{DrawMode, Backend, BlendMode, FillMode};
// pub use pathfinder_backend::SceneBackend;
pub use crate::image::{load_image, ImageData};
use custom_debug_derive::Debug;

use pdf::{object::*, content::TextMode};
use pdf::error::PdfError;
use vello::kurbo::{Affine, Vec2 as Vector2F, Rect as RectF};
use renderstate::RenderState;
use std::sync::Arc;
use itertools::Itertools;

const SCALE: f32 = 25.4 / 72.;

#[derive(Copy, Clone, Default)]
pub struct BBox(Option<RectF>);
impl BBox {
    pub fn empty() -> Self {
        BBox(None)
    }
    pub fn add(&mut self, r2: RectF) {
        self.0 = Some(match self.0 {
            Some(r1) => r1.union(r2),
            None => r2
        });
    }
    pub fn add_bbox(&mut self, bb: Self) {
        if let Some(r) = bb.0 {
            self.add(r);
        }
    }
    pub fn rect(self) -> Option<RectF> {
        self.0
    }
}
impl From<RectF> for BBox {
    fn from(r: RectF) -> Self {
        BBox(Some(r))
    }
}

pub fn page_bounds(page: &Page) -> RectF {
    let Rect { left, right, top, bottom } = page.media_box().expect("no media box");
    RectF::from_points((left as f64, bottom as f64), (right as f64, top as f64))
        .scale_from_origin(SCALE as f64)
}

pub fn render_page(backend: &mut impl Backend, resolve: &impl Resolve, page: &Page, transform: Affine) -> Result<Affine, PdfError> {
    let bounds = page_bounds(page);
    let rotate = Affine::rotate(page.rotate as f64 * std::f64::consts::PI / 180.);
    let br = bounds.with_origin((0,0).into());
    let translate = Affine::translate(Vector2F::new(
        -br.min_x().min(br.max_x()),
        -br.min_y().min(br.max_y()),
    ));
    let view_box = transform * translate * rotate;
    backend.set_view_box(view_box);


    let root_transformation = transform
        * translate
        * rotate
        * Affine::new([
            SCALE as f64,   0.0,   -bounds.min_x(),
            0.0,    -SCALE as f64, bounds.max_y(),
        ]);

    let resources = t!(page.resources());

    let contents = try_opt!(page.contents.as_ref());
    let ops = contents.operations(resolve)?;
    let mut renderstate = RenderState::new(backend, resolve, &resources, root_transformation);
    for (i, op) in ops.iter().enumerate() {
        debug!("op {}: {:?}", i, op);
        renderstate.draw_op(op, i)?;
    }

    Ok(root_transformation)
}
pub fn render_pattern(backend: &mut impl Backend, pattern: &Pattern, resolve: &impl Resolve) -> Result<(), PdfError> {
    match pattern {
        Pattern::Stream(ref dict, ref ops) => {
            let resources = resolve.get(dict.resources)?;
            let mut renderstate = RenderState::new(backend, resolve, &*resources, Affine::default());
            for (i, op) in ops.iter().enumerate() {
                debug!("op {}: {:?}", i, op);
                renderstate.draw_op(op, i)?;
            }
        }
        Pattern::Dict(_) => {}
    }
    Ok(())
}


#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Fill {
    Solid(f32, f32, f32),
    Pattern(Ref<Pattern>),
}
impl Fill {
    pub fn black() -> Self {
        Fill::Solid(0., 0., 0.)
    }
}

#[derive(Debug)]
pub struct TextSpan<E: Encoder> {
    // A rect with the origin at the baseline, a height of 1em and width that corresponds to the advance width.
    pub rect: RectF,

    // width in textspace units (before applying transform)
    pub width: f32,
    // Bounding box of the rendered outline
    pub bbox: Option<RectF>,
    pub font_size: f32,
    #[debug(skip)]
    pub font: Option<Arc<FontEntry<E>>>,
    pub text: String,
    pub chars: Vec<TextChar>,
    pub color: Fill,
    pub alpha: f32,

    // apply this transform to a text draw in at the origin with the given width and font-size
    pub transform: Affine,
    pub mode: TextMode,
    pub op_nr: usize,
}
impl<E: Encoder> TextSpan<E> {
    pub fn parts(&self) -> impl Iterator<Item=Part> + '_ {
        self.chars.iter().cloned()
            .chain(std::iter::once(TextChar { offset: self.text.len(), pos: self.width, width: 0.0 }))
            .tuple_windows()
            .map(|(a, b)| Part {
                text: &self.text[a.offset..b.offset],
                pos: a.pos,
                width: a.width,
                offset: a.offset
            })
    }
    pub fn rparts(&self) -> impl Iterator<Item=Part> + '_ {
        self.chars.iter().cloned()
            .chain(std::iter::once(TextChar { offset: self.text.len(), pos: self.width, width: 0.0 })).rev()
            .tuple_windows()
            .map(|(b, a)| Part {
                text: &self.text[a.offset..b.offset],
                pos: a.pos,
                width: a.width,
                offset: a.offset
            })
    }
}
pub struct Part<'a> {
    pub text: &'a str,
    pub pos: f32,
    pub width: f32,
    pub offset: usize,
}
#[derive(Debug, Clone, Copy)]
pub struct TextChar {
    pub offset: usize,
    pub pos: f32,
    pub width: f32,
}
