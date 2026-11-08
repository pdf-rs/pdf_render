//! Stroke to fill, in one place: every backend that cannot stroke natively
//! (`svg`, `pathfinder`, `scene`) goes through here.
//!
//! Two ways this went wrong in the writing, both found on nasa.pdf page 35,
//! where the fraction bars of Equations (11)–(13) are dashed strokes:
//!
//! * `OutlineDash` does nothing until `dash()` is called. Building one and
//!   taking `into_outline()` yields a stray contour near the origin, so the
//!   line vanished from the page and a speck appeared at the top-left corner.
//! * `[] 0 d` — an empty dash array — is how a content stream says "solid
//!   again". It is not a dash pattern; dashing with it drops the path.

use pathfinder_content::{dash::OutlineDash, outline::Outline, stroke::OutlineStrokeToFill};

use crate::backend::Stroke;

/// The outline of `outline` stroked with `stroke`, as a fill.
pub fn stroke_to_fill(outline: &Outline, stroke: &Stroke) -> Outline {
    let dashes = match stroke.dash_pattern {
        Some((ref pattern, phase)) if pattern.iter().any(|&v| v > 0.0) => Some((pattern, phase)),
        _ => None,
    };
    let dashed;
    let source = match dashes {
        Some((pattern, phase)) => {
            let mut dasher = OutlineDash::new(outline, pattern, phase);
            dasher.dash();
            dashed = dasher.into_outline();
            &dashed
        }
        None => outline,
    };
    let mut stroker = OutlineStrokeToFill::new(source, stroke.style);
    stroker.offset();
    stroker.into_outline()
}

#[cfg(test)]
mod tests {
    use super::*;
    use font::Pen;
    use pathfinder_content::stroke::StrokeStyle;
    use pathfinder_geometry::vector::vec2f;

    fn line() -> Outline {
        let mut b = font::pathfinder_impl::PathBuilder::new();
        b.move_to(vec2f(10.0, 20.0));
        b.line_to(vec2f(30.0, 20.0));
        b.finish()
    }

    fn stroke(dash_pattern: Option<(Vec<f32>, f32)>) -> Stroke {
        Stroke { dash_pattern, style: StrokeStyle { line_width: 1.0, ..Default::default() } }
    }

    fn bounds(o: &Outline) -> Option<(f32, f32, f32, f32)> {
        let r = o.bounds();
        (!r.is_empty()).then(|| (r.min_x(), r.min_y(), r.max_x(), r.max_y()))
    }

    /// A butt cap does not lengthen the line; the stroke widens it by 1.
    #[test]
    fn a_solid_line_strokes_to_a_bar() {
        for pattern in [None, Some((Vec::new(), 0.0)), Some((vec![0.0, 0.0], 0.0))] {
            let (x0, y0, x1, y1) =
                bounds(&stroke_to_fill(&line(), &stroke(pattern.clone()))).expect("solid bar");
            assert_eq!((x0, x1), (10.0, 30.0), "{pattern:?}");
            assert!((y0 - 19.5).abs() < 0.01 && (y1 - 20.5).abs() < 0.01, "{pattern:?}: {y0}..{y1}");
        }
    }

    #[test]
    fn a_real_dash_pattern_dashes_in_place() {
        let dashed = stroke_to_fill(&line(), &stroke(Some((vec![2.0, 2.0], 0.0))));
        let (x0, y0, x1, y1) = bounds(&dashed).expect("dashes");
        // Still on the line, never at the origin: the old code left a contour
        // at (0, 0) and nothing else.
        assert!(x0 >= 9.0 && x1 <= 31.0 && y0 >= 19.0 && y1 <= 21.0, "{x0} {y0} {x1} {y1}");
        // Five 2-long dashes over 20 units, so the union is not one bar.
        assert!(dashed.contours().len() > 1, "{} contour(s)", dashed.contours().len());
    }
}
