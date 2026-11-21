//! Shading → peniko, for the two backends that paint with peniko.
//!
//! `vello` (GPU) and `vello_cpu` both take a gradient as a `peniko::Gradient`
//! plus an affine that places it. The shading has already been reduced to
//! stops, a gradient kind and a matrix by `renderstate`, so this is a copy, not
//! a decision — and it lives here rather than in either backend so the two
//! cannot drift apart.

use pathfinder_color::ColorF;
use pathfinder_geometry::transform2d::Transform2F;

use crate::shading::{Gradient, GradientKind};

use peniko::kurbo::Affine;

pub(crate) fn transform_to_affine(transform: Transform2F) -> Affine {
    Affine::new([
        transform.m11() as f64,
        transform.m21() as f64,
        transform.m12() as f64,
        transform.m22() as f64,
        transform.m13() as f64,
        transform.m23() as f64,
    ])
}

/// The stops and geometry of a shading, as peniko wants them.
pub(crate) fn to_peniko_gradient(g: &Gradient, alpha: f32) -> peniko::Gradient {
    use peniko::color::{AlphaColor, DynamicColor, Srgb};
    use peniko::{ColorStop, ColorStops};

    let mut pg = match g.kind {
        GradientKind::Linear { start, end } => peniko::Gradient::new_linear(
            (start.x() as f64, start.y() as f64),
            (end.x() as f64, end.y() as f64),
        ),
        GradientKind::Radial { start, start_radius, end, end_radius } => {
            peniko::Gradient::new_two_point_radial(
                (start.x() as f64, start.y() as f64),
                start_radius,
                (end.x() as f64, end.y() as f64),
                end_radius,
            )
        }
    };
    // A shading pads; an end `/Extend` switched off is a transparent stop.
    pg.extend = peniko::Extend::Pad;
    let mut stops = ColorStops::new();
    for s in &g.stops {
        let c = ColorF::new(s.color.0, s.color.1, s.color.2, alpha).to_u8();
        stops.push(ColorStop {
            offset: s.offset,
            color: DynamicColor::from_alpha_color(AlphaColor::<Srgb>::from_rgba8(
                c.r, c.g, c.b, c.a,
            )),
        });
    }
    pg.stops = stops;
    pg
}

/// The paint transform that puts a gradient where the shading says.
///
/// Both backends compose the brush transform as `path_transform * brush`, and
/// the shape is already drawn with the CTM, so this is the gradient's own
/// matrix — shading space to user space — with no CTM in it.
pub(crate) fn paint_transform(fill: &crate::FillMode) -> Option<Affine> {
    match &fill.color {
        crate::Fill::Gradient(g) => Some(transform_to_affine(g.matrix)),
        _ => None,
    }
}
