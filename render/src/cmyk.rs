//! DeviceCMYK → RGB.
//!
//! PDF defines no display conversion for `DeviceCMYK`: it is a device ink
//! space, and every viewer runs it through a default CMYK ICC profile. The
//! naive `1 - min(1, c + k)` is *not* that, and gets the primaries visibly
//! wrong — solid cyan comes out (0, 255, 255) where both poppler and
//! ghostscript show (0, 173, 239), magenta (255, 0, 255) where they show
//! (236, 0, 140). A CMYK gradient therefore looked nothing like okular's.
//!
//! We do not want an ICC engine in here: lcms is C, and we build for wasm32
//! and for aarch64 android. The transform is smooth, so we measure it once and
//! interpolate. `cmyk_lut.bin` is a 9⁴ grid of RGB triples read off poppler
//! (work/rex/mkcmylut.py); quadrilinear interpolation reproduces it to a mean
//! of 0.2/255 over held-out probes, worst 0.7/255.
//!
//! Poppler is the reference because it is what pdfcmp compares against and
//! what okular shows. Ghostscript's `iccprofiles/default_cmyk.icc` is a
//! different default (a real press sheet): the two agree exactly on the pure
//! inks but differ in the interior — mean 7/255, worst 67/255 at 3/4 cyan,
//! where gs is the darker. A PDF that carries its own CMYK profile in an
//! output intent should be honoured instead of this table, one day.

const N: usize = 9;
const LUT: &[u8] = include_bytes!("cmyk_lut.bin");

#[inline]
fn cell(v: f32) -> (usize, f32) {
    let f = v.clamp(0.0, 1.0) * (N - 1) as f32;
    let i = (f as usize).min(N - 2);
    (i, f - i as f32)
}

#[inline]
fn at(i: [usize; 4]) -> [f32; 3] {
    let base = ((i[0] * N + i[1]) * N + i[2]) * N + i[3];
    let o = base * 3;
    [
        LUT[o] as f32,
        LUT[o + 1] as f32,
        LUT[o + 2] as f32,
    ]
}

/// CMYK in 0..=1 to linear-free sRGB in 0..=1.
pub fn to_rgb(c: f32, m: f32, y: f32, k: f32) -> [f32; 3] {
    let (ci, ct) = cell(c);
    let (mi, mt) = cell(m);
    let (yi, yt) = cell(y);
    let (ki, kt) = cell(k);
    let t = [ct, mt, yt, kt];
    let base = [ci, mi, yi, ki];
    let mut acc = [0.0f32; 3];
    for corner in 0..16usize {
        let mut w = 1.0f32;
        let mut i = base;
        for d in 0..4 {
            if (corner >> d) & 1 == 1 {
                w *= t[d];
                i[d] += 1;
            } else {
                w *= 1.0 - t[d];
            }
        }
        let s = at(i);
        acc[0] += w * s[0];
        acc[1] += w * s[1];
        acc[2] += w * s[2];
    }
    [
        (acc[0] / 255.0).clamp(0.0, 1.0),
        (acc[1] / 255.0).clamp(0.0, 1.0),
        (acc[2] / 255.0).clamp(0.0, 1.0),
    ]
}

/// The same, for 8-bit samples (image data, indexed palettes).
pub fn to_rgb_u8([c, m, y, k]: [u8; 4]) -> [u8; 3] {
    let [r, g, b] = to_rgb(
        c as f32 / 255.0,
        m as f32 / 255.0,
        y as f32 / 255.0,
        k as f32 / 255.0,
    );
    [
        (r * 255.0 + 0.5) as u8,
        (g * 255.0 + 0.5) as u8,
        (b * 255.0 + 0.5) as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_ghostscript_primaries() {
        // values read back from gs: `1 0 0 0 k ... re f` etc.
        let cases = [
            ([1.0, 0.0, 0.0, 0.0], [0u8, 173, 238]),
            ([0.0, 1.0, 0.0, 0.0], [236, 0, 140]),
            ([0.0, 0.0, 1.0, 0.0], [255, 242, 0]),
            ([0.0, 0.0, 0.0, 0.0], [255, 255, 255]),
        ];
        for (cmyk, want) in cases {
            let got = to_rgb_u8([
                (cmyk[0] * 255.0) as u8,
                (cmyk[1] * 255.0) as u8,
                (cmyk[2] * 255.0) as u8,
                (cmyk[3] * 255.0) as u8,
            ]);
            for ch in 0..3 {
                assert!(
                    (got[ch] as i32 - want[ch] as i32).abs() <= 2,
                    "cmyk {:?}: got {:?}, gs {:?}",
                    cmyk,
                    got,
                    want
                );
            }
        }
    }
}
