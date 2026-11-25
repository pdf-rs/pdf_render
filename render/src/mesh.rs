//! Mesh shadings (§8.7.4.5.5 to §8.7.4.5.8), types 4 to 7.
//!
//! A mesh shading is not a function of a parameter, so it is not a
//! [`Gradient`](crate::shading::Gradient): it is a *stream* of vertices, each
//! a point in shading space plus a colour of its own, grouped into triangles
//! (type 4), rows of a lattice (type 5) or patches with Bézier edges
//! (types 6 and 7). The colour between vertices is interpolated, so this
//! module's job ends with shapes that carry a colour per corner; the renderer
//! turns those into flat-coloured triangles by subdividing until the colour
//! across one is under a just-visible step.
//!
//! Bit layout (§8.7.4.5.2, Table 84). Each vertex is
//! `BitsPerFlag` flag bits, then `BitsPerCoordinate` bits for x and for y,
//! then `BitsPerComponent` bits per colour component. The flag is decoded at
//! every vertex of a type 4 stream — every implementation reads it there and
//! writers emit it (this one included) — though only the first vertex of a
//! triangle gives it meaning. A type 5 lattice has no flags at all in its
//! stream, whatever `/BitsPerFlag` says: that is what mupdf and pdf.js both
//! read, and what producers write.
//!
//! `/Decode` is `[xmin xmax ymin ymax]` followed by `[c0min c0max c1min
//! c1max …]`: the raw samples map linearly onto those ranges, and the number
//! of colour components is `(decode.len() - 4) / 2`.

use pathfinder_geometry::vector::Vector2F;

/// The most colour components a vertex can hold: /DeviceN is capped here and
/// nothing in the wild comes close.
pub const MAX_COMPONENTS: usize = 8;

/// A point in shading space and its colour, in the shading's own colour
/// space. Only the first `components` entries of `comps` are meaningful.
#[derive(Debug, Clone, Copy)]
pub struct Vertex {
    pub point: Vector2F,
    pub comps: [f32; MAX_COMPONENTS],
}

impl Default for Vertex {
    fn default() -> Self {
        Vertex {
            point: Vector2F::new(0.0, 0.0),
            comps: [0.0; MAX_COMPONENTS],
        }
    }
}

/// What a stream is laid out as: the parameters from the shading dictionary.
#[derive(Debug, Clone)]
pub struct Params {
    /// 4 (free-form Gouraud), 5 (lattice-form Gouraud), 6 (Coons patch) or
    /// 7 (tensor-product patch).
    pub shading_type: u8,
    pub bits_per_coordinate: u32,
    pub bits_per_component: u32,
    pub bits_per_flag: u32,
    /// `/Decode`, as written: four coordinates then a pair per component.
    pub decode: Vec<f32>,
    /// Type 5 only: `/VerticesPerRow`.
    pub vertices_per_row: usize,
}

impl Params {
    /// How many colour components a vertex carries.
    pub fn components(&self) -> usize {
        self.decode.len().saturating_sub(4) / 2
    }

    /// The bounds `/Decode` gives a coordinate or a component.
    fn range(&self, i: usize) -> (f32, f32) {
        let min = self.decode.get(2 * i).copied().unwrap_or(0.0);
        let max = self.decode.get(2 * i + 1).copied().unwrap_or(1.0);
        (min, max)
    }
}

/// One piece of a mesh, with its corner colours.
#[derive(Debug, Clone)]
pub enum Shape {
    Triangle([Vertex; 3]),
    /// Types 6 and 7, reduced to the four corner vertices of the patch: the
    /// Bézier edges between them are drawn as straight lines, which is a
    /// coarser curve than the file asks for but the right colour and roughly
    /// the right area.
    Quad([Vertex; 4]),
}

impl Shape {
    pub fn vertices(&self) -> &[Vertex] {
        match self {
            Shape::Triangle(v) => v,
            Shape::Quad(v) => v,
        }
    }
}

/// Read a mesh stream. Unreadable or truncated data ends the stream, as it
/// does in mupdf: a short stream is a bad page, not a lost document.
pub fn parse(data: &[u8], p: &Params) -> Vec<Shape> {
    parse_reported(data, p).0
}

/// As [`parse`], and also how many vertex flags were out of range: a stream
/// that was written without its flag bits at all reads as garbage, and a
/// count above zero is how the caller notices (see `paint_mesh`).
pub fn parse_reported(data: &[u8], p: &Params) -> (Vec<Shape>, usize) {
    match p.shading_type {
        4 => free_form(data, p),
        5 => (lattice(data, p), 0),
        6 => (patches(data, p, 12, [0, 3, 6, 9]), 0),
        7 => (patches(data, p, 16, [0, 3, 15, 12]), 0),
        _ => (Vec::new(), 0),
    }
}

/// A stream of triangles, §8.7.4.5.5.
///
/// The flag of a triangle's first vertex says which vertices it takes from
/// the triangle before it: 0 none (three new vertices follow), 1 the second
/// and third, 2 the first and third.
fn free_form(data: &[u8], p: &Params) -> (Vec<Shape>, usize) {
    let mut out = Vec::new();
    let mut bad_flags = 0;
    let mut bits = Bits::new(data);
    let mut prev: Option<[Vertex; 3]> = None;
    while !bits.eof() {
        let Some((mut flag, first)) = vertex(&mut bits, p, true) else {
            break;
        };
        // The first triangle of a stream has nothing to share with, whatever
        // its flag claims.
        if prev.is_none() && flag != 0 {
            bad_flags += 1;
            flag = 0;
        }
        let tri = match (flag, prev) {
            (1, Some(t)) => [t[1], t[2], first],
            (2, Some(t)) => [t[0], t[2], first],
            _ => {
                if flag > 2 {
                    bad_flags += 1;
                }
                let Some((_, b)) = vertex(&mut bits, p, true) else {
                    break;
                };
                let Some((_, c)) = vertex(&mut bits, p, true) else {
                    break;
                };
                [first, b, c]
            }
        };
        out.push(Shape::Triangle(tri));
        prev = Some(tri);
    }
    (out, bad_flags)
}

/// Rows of `/VerticesPerRow` vertices; each pair of rows is a strip of quads,
/// §8.7.4.5.6.
fn lattice(data: &[u8], p: &Params) -> Vec<Shape> {
    let n = p.vertices_per_row;
    if n < 2 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut bits = Bits::new(data);
    let mut prev: Vec<Vertex> = Vec::new();
    loop {
        let mut row = Vec::with_capacity(n);
        for _ in 0..n {
            let Some((_, v)) = vertex(&mut bits, p, false) else {
                return out;
            };
            row.push(v);
        }
        if prev.len() == n {
            for i in 0..n - 1 {
                out.push(Shape::Quad([prev[i], prev[i + 1], row[i + 1], row[i]]));
            }
        }
        prev = row;
        if bits.eof() {
            break;
        }
    }
    out
}

/// Types 6 and 7: a patch of 12 (Coons) or 16 (tensor-product) control
/// points, of which `corners` are the four that meet its neighbours.
///
/// The flag says how much of the patch is shared with the one before it: 0
/// nothing (all points, four corners' colours), 1, 2 or 3 share an edge, and
/// then only the remaining points and the other two corner colours are in the
/// stream. The mapping is mupdf's, which is what documents are calibrated
/// against.
fn patches(data: &[u8], p: &Params, points: usize, corners: [usize; 4]) -> Vec<Shape> {
    let mut out = Vec::new();
    let mut bits = Bits::new(data);
    let mut prev: Option<(Vec<Vertex>, [Vertex; 4])> = None;
    while !bits.eof() {
        let Some(flag) = bits.read(p.bits_per_flag) else {
            break;
        };
        let start = if flag == 0 { 0 } else { 4 };
        let mut v = vec![Vertex::default(); points];
        let mut ok = true;
        for slot in v.iter_mut().take(points).skip(start) {
            match coordinate_pair(&mut bits, p) {
                Some(point) => slot.point = point,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            break;
        }
        // Colours live on the corners, not on the control points between them.
        let mut c = [Vertex::default(); 4];
        let first_colour = if flag == 0 { 0 } else { 2 };
        for slot in c.iter_mut().take(4).skip(first_colour) {
            if !components(&mut bits, p, slot) {
                return out;
            }
        }
        match (flag, &prev) {
            (0, _) => {}
            (1, Some((pv, pc))) if pv.len() == points => {
                v[0..4].copy_from_slice(&pv[3..7]);
                c[0] = pc[1];
                c[1] = pc[2];
            }
            (2, Some((pv, pc))) if pv.len() == points => {
                v[0] = pv[6];
                v[1] = pv[7];
                v[2] = pv[8];
                v[3] = pv[9];
                c[0] = pc[2];
                c[1] = pc[3];
            }
            (3, Some((pv, pc))) if pv.len() == points => {
                v[0] = pv[9];
                v[1] = pv[10];
                v[2] = pv[11];
                v[3] = pv[0];
                c[0] = pc[3];
                c[1] = pc[0];
            }
            // A flag that points at a patch we do not have: the stream is
            // out of step, and guessing would paint nonsense.
            _ => return out,
        }
        let quad = [v[corners[0]], v[corners[1]], v[corners[2]], v[corners[3]]];
        out.push(Shape::Quad(quad));
        prev = Some((v, c));
    }
    out
}

/// A corner's colour, in the renderer's rgb; the fourth slot is unused and
/// only keeps the type a round size.
type Colours = [f32; 4];

/// Where subdivision stops, in shading space: the area and the altitude (the
/// thinnest dimension) a *painted* triangle may have at the smallest.
///
/// A mesh asks to be split until its colour is flat, and that alone can drive
/// a dense mesh down to triangles far below a pixel — vertices a fraction of
/// a pixel apart, slivers with no area. A rasteriser does not like those, and
/// the eye cannot see the colour they carry, so the caller converts a device
/// floor into these two numbers (by the determinant, and by its square root
/// for a length) and a triangle whose quarters would fall below the floor is
/// painted flat as it stands. The loss is precision in the colour, never in
/// the paint's coverage.
#[derive(Debug, Clone, Copy)]
pub struct Limit {
    pub area: f32,
    pub altitude: f32,
}

impl Limit {
    /// No floor at all.
    pub const NONE: Limit = Limit {
        area: 0.0,
        altitude: 0.0,
    };
}

/// Split `shape` into triangles whose corners differ by no more than
/// `max_spread` in any channel, so that painting each one flat does not show
/// as banding. `colour` gives the rgb of each corner in the same order.
///
/// `budget` caps the total number of triangles: a pathological mesh must not
/// take a page down with it.
pub fn subdivide(
    shape: &Shape,
    colour: &[Colours],
    max_spread: f32,
    limit: Limit,
    budget: usize,
) -> Vec<([Vector2F; 3], [[f32; 3]; 3])> {
    let mut out = Vec::new();
    match shape {
        Shape::Triangle(_) => {
            let v = shape.vertices();
            let rgb = [rgb_of(&colour[0]), rgb_of(&colour[1]), rgb_of(&colour[2])];
            let mut pts = [v[0].point, v[1].point, v[2].point];
            let mut cols = rgb;
            tri(
                &mut pts,
                &mut cols,
                max_spread.min(1.0),
                5,
                limit,
                budget,
                &mut out,
            );
        }
        Shape::Quad(_) => {
            let v = shape.vertices();
            let rgb = [
                rgb_of(&colour[0]),
                rgb_of(&colour[1]),
                rgb_of(&colour[2]),
                rgb_of(&colour[3]),
            ];
            let mut budget = budget;
            for (i, j, k) in [(0, 1, 2), (0, 2, 3)] {
                let mut pts = [v[i].point, v[j].point, v[k].point];
                let mut cols = [rgb[i], rgb[j], rgb[k]];
                let before = out.len();
                tri(
                    &mut pts,
                    &mut cols,
                    max_spread.min(1.0),
                    5,
                    limit,
                    budget,
                    &mut out,
                );
                budget = budget.saturating_sub(out.len() - before);
            }
        }
    }
    out
}

fn rgb_of(c: &[f32; 4]) -> [f32; 3] {
    [c[0], c[1], c[2]]
}

fn tri(
    pts: &mut [Vector2F; 3],
    cols: &mut [[f32; 3]; 3],
    max_spread: f32,
    depth: u32,
    limit: Limit,
    budget: usize,
    out: &mut Vec<([Vector2F; 3], [[f32; 3]; 3])>,
) {
    let spread = spread(*cols);
    if spread <= max_spread || depth == 0 || out.len() >= budget || below(pts, limit) {
        out.push((*pts, *cols));
        return;
    }
    // Four-way split: each edge halved, so the spread halves with it.
    let m = |a: Vector2F, b: Vector2F| (a + b) * 0.5;
    let mix = |a: [f32; 3], b: [f32; 3]| {
        [
            (a[0] + b[0]) * 0.5,
            (a[1] + b[1]) * 0.5,
            (a[2] + b[2]) * 0.5,
        ]
    };
    let ab = (m(pts[0], pts[1]), mix(cols[0], cols[1]));
    let bc = (m(pts[1], pts[2]), mix(cols[1], cols[2]));
    let ca = (m(pts[2], pts[0]), mix(cols[2], cols[0]));
    for (p, c) in [
        ([pts[0], ab.0, ca.0], [cols[0], ab.1, ca.1]),
        ([ab.0, pts[1], bc.0], [ab.1, cols[1], bc.1]),
        ([ca.0, bc.0, pts[2]], [ca.1, bc.1, cols[2]]),
        ([ab.0, bc.0, ca.0], [ab.1, bc.1, ca.1]),
    ] {
        let mut p = p;
        let mut c = c;
        tri(&mut p, &mut c, max_spread, depth - 1, limit, budget, out);
    }
}

/// Whether splitting this triangle would make pieces below the size floor. A
/// four-way split quarters the area and halves the altitude, so a triangle
/// within four times the floor's area, or twice its altitude, is left whole:
/// the smallest triangle painted is then never under the floor.
fn below(p: &[Vector2F; 3], limit: Limit) -> bool {
    if limit.area <= 0.0 && limit.altitude <= 0.0 {
        return false;
    }
    let a = area(p);
    if limit.area > 0.0 && a <= 4.0 * limit.area {
        return true;
    }
    if limit.altitude <= 0.0 {
        return false;
    }
    let longest = [(0, 1), (1, 2), (2, 0)]
        .iter()
        .map(|&(i, j)| {
            let d = p[j] - p[i];
            (d.x() * d.x() + d.y() * d.y()).sqrt()
        })
        .fold(0.0f32, f32::max);
    longest > 0.0 && 2.0 * a / longest <= 2.0 * limit.altitude
}

/// The area of a triangle, unsigned.
fn area(p: &[Vector2F; 3]) -> f32 {
    let d = |i: usize, j: usize| p[j] - p[i];
    let (ab, ac) = (d(0, 1), d(0, 2));
    (ab.x() * ac.y() - ab.y() * ac.x()).abs() * 0.5
}

fn spread(c: [[f32; 3]; 3]) -> f32 {
    let mut worst = 0.0f32;
    for ch in 0..3 {
        let a = c[0][ch];
        let b = c[1][ch];
        let d = c[2][ch];
        let hi = a.max(b).max(d);
        let lo = a.min(b).min(d);
        worst = worst.max(hi - lo);
    }
    worst
}

/// MSB-first bit reader over a decoded stream.
struct Bits<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits { data, bit: 0 }
    }

    fn eof(&self) -> bool {
        self.bit >= self.data.len() * 8
    }

    fn read(&mut self, n: u32) -> Option<u32> {
        if n == 0 {
            return Some(0);
        }
        if n > 32 || self.bit + n as usize > self.data.len() * 8 {
            return None;
        }
        let mut value = 0u32;
        let mut left = n as usize;
        while left > 0 {
            let byte = self.data[self.bit / 8] as u32;
            let offset = (self.bit % 8) as u32;
            let take = (8 - offset).min(left as u32);
            let chunk = (byte >> (8 - offset - take)) & ((1u32 << take) - 1);
            value = (value << take) | chunk;
            self.bit += take as usize;
            left -= take as usize;
        }
        Some(value)
    }
}

fn sample(raw: u32, bits: u32, (min, max): (f32, f32)) -> f32 {
    let scale = if bits >= 32 {
        1.0 / u32::MAX as f32
    } else {
        1.0 / ((1u32 << bits) - 1) as f32
    };
    min + raw as f32 * scale * (max - min)
}

fn coordinate_pair(bits: &mut Bits, p: &Params) -> Option<Vector2F> {
    let n = p.bits_per_coordinate;
    let x = sample(bits.read(n)?, n, p.range(0));
    let y = sample(bits.read(n)?, n, p.range(1));
    Some(Vector2F::new(x, y))
}

fn components(bits: &mut Bits, p: &Params, v: &mut Vertex) -> bool {
    let n = p.components().min(MAX_COMPONENTS);
    for i in 0..n {
        let Some(raw) = bits.read(p.bits_per_component) else {
            return false;
        };
        v.comps[i] = sample(raw, p.bits_per_component, p.range(2 + i));
    }
    true
}

fn vertex(bits: &mut Bits, p: &Params, with_flag: bool) -> Option<(u32, Vertex)> {
    let flag = if with_flag {
        bits.read(p.bits_per_flag)?
    } else {
        0
    };
    let point = coordinate_pair(bits, p)?;
    let mut v = Vertex {
        point,
        comps: [0.0; MAX_COMPONENTS],
    };
    if !components(bits, p, &mut v) {
        return None;
    }
    Some((flag, v))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(shading_type: u8) -> Params {
        Params {
            shading_type,
            bits_per_coordinate: 8,
            bits_per_component: 8,
            bits_per_flag: 8,
            decode: vec![0.0, 255.0, 0.0, 255.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
            vertices_per_row: 2,
        }
    }

    /// One triangle: three vertices, each flag/xy/rgb byte.
    #[test]
    fn free_form_triangle() {
        let mut data = Vec::new();
        for (f, x, y, rgb) in [
            (0u8, 0u8, 0u8, [255u8, 0, 0]),
            (0, 255, 0, [0, 255, 0]),
            (0, 0, 255, [0, 0, 255]),
        ] {
            data.push(f);
            data.push(x);
            data.push(y);
            data.extend_from_slice(&rgb);
        }
        let shapes = parse(&data, &params(4));
        assert_eq!(shapes.len(), 1);
        let Shape::Triangle(v) = &shapes[0] else {
            panic!("not a triangle")
        };
        assert_eq!(v[1].point, Vector2F::new(255.0, 0.0));
        assert_eq!(v[1].comps[1], 1.0);
        assert_eq!(v[2].point, Vector2F::new(0.0, 255.0));
        assert_eq!(v[2].comps[2], 1.0);
    }

    /// A flag of 1 shares the previous triangle's second and third vertices.
    #[test]
    fn free_form_shared_edge() {
        let mut data = Vec::new();
        let mut push = |f: u8, x: u8, y: u8| {
            data.push(f);
            data.push(x);
            data.push(y);
            data.extend_from_slice(&[0, 0, 0]);
        };
        push(0, 0, 0);
        push(0, 10, 0);
        push(0, 0, 10);
        push(1, 20, 20);
        let shapes = parse(&data, &params(4));
        assert_eq!(shapes.len(), 2);
        let Shape::Triangle(v) = &shapes[1] else {
            panic!("not a triangle")
        };
        assert_eq!(v[0].point, Vector2F::new(10.0, 0.0));
        assert_eq!(v[1].point, Vector2F::new(0.0, 10.0));
    }

    /// Two rows of a lattice make one quad.
    #[test]
    fn lattice_rows() {
        let mut data = Vec::new();
        for (x, y) in [(0u8, 0u8), (100, 0), (0, 100), (100, 100)] {
            data.push(x);
            data.push(y);
            data.extend_from_slice(&[0, 0, 0]);
        }
        let shapes = parse(&data, &params(5));
        assert_eq!(shapes.len(), 1);
    }

    /// A triangle already inside the size floor is painted as one piece,
    /// however far apart its colours are: splitting it would only make
    /// vertices that sit a fraction of a pixel from each other.
    #[test]
    fn subdivision_stops_at_the_size_floor() {
        let shape = Shape::Triangle([
            Vertex {
                point: Vector2F::new(0.0, 0.0),
                ..Default::default()
            },
            Vertex {
                point: Vector2F::new(1.0, 0.0),
                ..Default::default()
            },
            Vertex {
                point: Vector2F::new(0.0, 1.0),
                ..Default::default()
            },
        ]);
        let colours = [
            [0.0, 0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
        ];
        let floor = Limit {
            area: 1.0,
            altitude: 0.5,
        };
        assert_eq!(subdivide(&shape, &colours, 0.01, floor, 100_000).len(), 1);
        // Without the floor the same triangle is split, and split again.
        assert!(subdivide(&shape, &colours, 0.01, Limit::NONE, 100_000).len() > 1);
    }

    /// A sliver has area enough to look respectable but no altitude: it is one
    /// piece, because every split of it is a thinner sliver in its place.
    #[test]
    fn a_sliver_is_not_split() {
        let shape = Shape::Triangle([
            Vertex {
                point: Vector2F::new(0.0, 0.0),
                ..Default::default()
            },
            Vertex {
                point: Vector2F::new(1000.0, 0.0),
                ..Default::default()
            },
            Vertex {
                point: Vector2F::new(500.0, 0.1),
                ..Default::default()
            },
        ]);
        let colours = [
            [0.0, 0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ];
        let floor = Limit {
            area: 1.0,
            altitude: 0.5,
        };
        assert_eq!(subdivide(&shape, &colours, 0.01, floor, 100_000).len(), 1);
    }

    /// A four-way split quarters the area, so a triangle whose quarters would
    /// fall below the floor is left whole: every piece painted from a triangle
    /// big enough to be split is above the floor's area.
    #[test]
    fn no_painted_piece_falls_below_the_floor() {
        let shape = Shape::Triangle([
            Vertex {
                point: Vector2F::new(0.0, 0.0),
                ..Default::default()
            },
            Vertex {
                point: Vector2F::new(40.0, 0.0),
                ..Default::default()
            },
            Vertex {
                point: Vector2F::new(0.0, 20.0),
                ..Default::default()
            },
        ]);
        let colours = [
            [0.0, 0.0, 0.0, 0.0],
            [1.0, 1.0, 0.0, 0.0],
            [0.0, 1.0, 1.0, 0.0],
        ];
        let floor = Limit {
            area: 1.0,
            altitude: 0.5,
        };
        let pieces = subdivide(&shape, &colours, 0.01, floor, 100_000);
        assert!(pieces.len() > 1);
        for (points, cols) in &pieces {
            let a = area(points);
            assert!(
                a > floor.area || spread(*cols) <= 0.01,
                "a piece of area {a} was painted below the floor"
            );
        }
    }
}
