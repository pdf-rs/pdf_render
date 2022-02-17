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
mod scene;
mod font;

pub use cache::{Cache};
pub use fontentry::FontEntry;
pub use backend::{DrawMode, Backend};
pub use scene::SceneBackend;
pub use crate::image::load_image;
use custom_debug_derive::Debug;

use pdf::object::*;
use pdf::error::PdfError;
use pathfinder_geometry::{
    vector::{Vector2F, Vector2I},
    rect::RectF, transform2d::Transform2F,
};
use pathfinder_color::ColorU;
use renderstate::RenderState;
use std::rc::Rc;
const SCALE: f32 = 25.4 / 72.;


#[derive(Copy, Clone)]
pub struct BBox(Option<RectF>);
impl BBox {
    pub fn empty() -> Self {
        BBox(None)
    }
    pub fn add(&mut self, r2: RectF) {
        self.0 = Some(match self.0 {
            Some(r1) => r1.union_rect(r2),
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
    let Rect { left, right, top, bottom } = page.crop_box().expect("no media box");
    RectF::from_points(Vector2F::new(left, bottom), Vector2F::new(right, top)) * SCALE
}
pub fn render_page(backend: &mut impl Backend, resolve: &impl Resolve, page: &Page, transform: Transform2F) -> Result<(), PdfError> {
    let bounds = page_bounds(page);
    let rotate = Transform2F::from_rotation(page.rotate as f32 * std::f32::consts::PI / 180.);
    let br = rotate * RectF::new(Vector2F::zero(), bounds.size());
    let translate = Transform2F::from_translation(Vector2F::new(
        -br.min_x().min(br.max_x()),
        -br.min_y().min(br.max_y()),
    ));
    let view_box = transform * translate * br;
    backend.set_view_box(view_box);
    
    let root_transformation = transform
        * translate
        * rotate
        * Transform2F::row_major(SCALE, 0.0, -bounds.min_x(), 0.0, -SCALE, bounds.max_y());
    
    let resources = t!(page.resources());

    let contents = try_opt!(page.contents.as_ref());
    let ops = contents.operations(resolve)?;
    let mut renderstate = RenderState::new(backend, resolve, &resources, root_transformation);
    for (i, op) in ops.iter().enumerate() {
        debug!("op {}: {:?}", i, op);
        renderstate.draw_op(op)?;
    }

    Ok(())
}

#[derive(Debug)]
pub struct TextSpan {
    // A rect with the origin at the baseline, a height of 1em and width that corresponds to the advance width.
    pub rect: RectF,

    // width in textspace units (before applying transform)
    pub width: f32,
    // Bounding box of the rendered outline
    pub bbox: RectF,
    pub font_size: f32,
    #[debug(skip)]
    pub font: Rc<FontEntry>,
    pub text: String,
    pub color: ColorU,

    // apply this transform to a text draw in at the origin with the given width and font-size
    pub transform: Transform2F,

    // in text units
    pub char_space: f32,
    pub word_space: f32,
}