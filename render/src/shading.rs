//! PDF shadings (§8.7.4), as far as this renderer paints them.
//!
//! A shading is not a shape but a paint: `sh` fills the region the current
//! clip selects with colour that varies across the page. What a backend can do
//! with that differs — SVG has `<linearGradient>`/`<radialGradient>`, the vello
//! backends take a peniko gradient as the brush, pathfinder has only solid and
//! pattern paints — so the shading is reduced here to a [`Gradient`] in
//! *shading space* (the user space at the `sh`), and each backend maps it to
//! its own paint. A backend that cannot paint a gradient paints
//! [`Gradient::average_color`] instead of nothing: a wrong fill colour is a
//! visible bug, a missing fill can pass for a document that has no shading.
//!
//! Supported: type 2 (axial) and type 3 (radial), which is what real documents
//! use — a scan of 1251 PDFs found nothing else. Types 1, 4, 5, 6 and 7 are
//! meshes (free-form Gouraud, lattice-form Gouraud, Coons, tensor-product
//! patch); they arrive as streams of triangles rather than as a function of one
//! parameter, and are not painted.

use pathfinder_geometry::{transform2d::Transform2F, vector::Vector2F};

/// The shading's parameter domain mapped to colour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GradientKind {
    /// Type 2: `t` runs from 0 at `start` to 1 at `end` along the line. Points
    /// off the line are projected onto it.
    Linear { start: Vector2F, end: Vector2F },
    /// Type 3: `t` runs from 0 on the `start` circle to 1 on the `end` circle.
    /// The two may share a centre (the common case) or not (a cone).
    Radial {
        start: Vector2F,
        start_radius: f32,
        end: Vector2F,
        end_radius: f32,
    },
}

/// What happens outside the parameter range 0..1.
///
/// A PDF shading has no `repeat` and no `reflect`: `/Extend` says only
/// whether the region beyond an end is painted at all, and if it is, it takes
/// the colour of that end — which is SVG's `pad`. `false` means that region
/// is not painted, and since no gradient paint can say that, the *shape* the
/// gradient is drawn into is cut back to the parameter domain instead (see
/// `RenderState::shading_shape`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extend {
    /// Use the colour at the nearest end: one solid area.
    Pad,
    /// Paint nothing there.
    None,
}

/// One colour stop, already converted to the renderer's colour space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stop {
    pub offset: f32,
    pub color: (f32, f32, f32),
}

/// A gradient in *shading space*, the space its coordinates are written in.
/// `/Matrix` (or, for a shading pattern, the pattern matrix) maps that to the
/// user space the shape is drawn in: a device point is
/// `transformation_matrix * matrix * p`. `matrix` itself carries no CTM — each
/// backend composes it with the transform it draws the shape with, because
/// that is where the two must agree.
#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    pub kind: GradientKind,
    /// At least two stops, sorted by offset, the first at 0.0 and the last at
    /// 1.0. `/Function` output is sampled uniformly and then runs of one colour
    /// are merged, so a two-colour axial gradient has two stops and a ten-step
    /// `/FunctionType 0` has ten.
    pub stops: Vec<Stop>,
    /// `[before, after]`: what `/Extend` said about each end.
    pub extend: [Extend; 2],
    /// `/Matrix`: shading space to user space.
    pub matrix: Transform2F,
}

impl Gradient {
    /// The colour a backend without gradient paint uses: the middle of the
    /// ramp, weighted by how much of the parameter range each stop covers, so
    /// a gradient that is 90% red averages close to red.
    pub fn average_color(&self) -> (f32, f32, f32) {
        let mut sum = (0.0f32, 0.0f32, 0.0f32);
        let mut total = 0.0f32;
        for pair in self.stops.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let w = (b.offset - a.offset).max(0.0);
            if w == 0.0 {
                continue;
            }
            for (s, (ca, cb)) in [
                (&mut sum.0, (a.color.0, b.color.0)),
                (&mut sum.1, (a.color.1, b.color.1)),
                (&mut sum.2, (a.color.2, b.color.2)),
            ] {
                *s += w * 0.5 * (ca + cb);
            }
            total += w;
        }
        if total > 0.0 {
            (sum.0 / total, sum.1 / total, sum.2 / total)
        } else {
            self.stops.first().map(|s| s.color).unwrap_or((0.0, 0.0, 0.0))
        }
    }

}
