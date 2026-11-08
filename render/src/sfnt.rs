//! Where a TrueType glyph's outline has to be moved before it is drawn.
//!
//! `hmtx` gives every glyph a left side bearing, and OpenType says it is the
//! glyph's `xMin` (OT §hmtx: "the left side bearing ... must be equal to
//! xMin"). Fonts written by Type 1 conversion tools do not always keep that
//! promise: the outline sits at `x = 0` while `hmtx` still carries the `hsbw`
//! side bearing of the original charstring. Every rasterizer in use compen-
//! sates. FreeType does it in `ttgload.c`, where `pp1.x` is set to
//! `bbox.xMin - hmtx.lsb` and the outline is translated by `-pp1.x` —
//! "translate array so that (0,0) is the glyph's origin" — and Acrobat is
//! named in the comment as the reason the behaviour is unconditional. Poppler
//! and pdf.js inherit it through FreeType, so a viewer that uses the raw
//! outline coordinates instead puts such glyphs a whole side bearing away from
//! where the author saw them.
//!
//! `nasa.pdf` page 35 (Equation 11) is the case that found this: /TT7, a
//! converted subset of SymbolMT, has the pieces of the big braces at
//! `xMin = 0` with `hmtx` side bearings of 420 and 28 units. Drawing them as
//! they are leaves the stems of the left brace 0.86 mm left of the hooks.
//!
//! Only the `glyf` table is read, and only to the two bytes that hold `xMin`
//! of each glyph; CFF fonts draw their charstrings with the side bearing already
//! applied, so nothing is returned for them.

use std::collections::HashMap;

use font::GlyphId;
use pdf::font::Font as PdfFont;
use pdf::object::Resolve;

/// Per glyph id, the x offset in font units that has to be added to the
/// outline before it is placed at the pen position. Glyphs whose `xMin` matches
/// `hmtx` are not in the map.
pub fn glyph_x_shifts(font: &PdfFont, resolve: &impl Resolve) -> HashMap<GlyphId, f32> {
    match font.embedded_data(resolve) {
        Some(Ok(data)) => shifts_from_program(&data).unwrap_or_default(),
        _ => HashMap::new(),
    }
}

fn be16(d: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([d[at], d[at + 1]])
}

fn bei16(d: &[u8], at: usize) -> i16 {
    be16(d, at) as i16
}

fn be32(d: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
}

/// The table directory entry `(offset, len)` of `tag`, if the font has one.
fn table(font: &[u8], tag: &[u8; 4]) -> Option<(usize, usize)> {
    if font.len() < 12 {
        return None;
    }
    let n = be16(font, 4) as usize;
    for i in 0 .. n {
        let rec = 12 + i * 16;
        if rec + 16 > font.len() {
            return None;
        }
        if &font[rec .. rec + 4] == tag {
            let off = be32(font, rec + 8) as usize;
            let len = be32(font, rec + 12) as usize;
            if off.checked_add(len)? > font.len() {
                return None;
            }
            return Some((off, len));
        }
    }
    None
}

/// Read `glyf`, `loca`, `head` and `hmtx` of an sfnt font program and report
/// the glyphs whose outline is not where their side bearing says it is.
///
/// A font that is not sfnt TrueType (CFF, Type 1, a bare collection, a broken
/// table) gives an empty map: nothing is shifted then, which is the behaviour
/// before this existed.
pub fn shifts_from_program(font: &[u8]) -> Option<HashMap<GlyphId, f32>> {
    // 0x00010000 = TrueType outlines, 'true' = Apple's older tag for the same.
    if &font[0 .. 4] != &[0, 1, 0, 0] && &font[0 .. 4] != b"true" {
        return None;
    }
    let (head, _) = table(font, b"head")?;
    let (maxp, _) = table(font, b"maxp")?;
    let (hhea, _) = table(font, b"hhea")?;
    let (hmtx, hmtx_len) = table(font, b"hmtx")?;
    let (loca, _) = table(font, b"loca")?;
    let (glyf, glyf_len) = table(font, b"glyf")?;
    if head + 54 > font.len() || maxp + 6 > font.len() || hhea + 36 > font.len() {
        return None;
    }
    let long_loca = bei16(font, head + 50) != 0;
    let num_glyphs = be16(font, maxp + 4) as usize;
    let num_hmetrics = be16(font, hhea + 34) as usize;

    let mut shifts = HashMap::new();
    for gid in 0 .. num_glyphs {
        // loca has num_glyphs + 1 entries.
        let (start, end) = if long_loca {
            let at = loca + gid * 4;
            if at + 8 > font.len() { break; }
            (be32(font, at) as usize, be32(font, at + 4) as usize)
        } else {
            let at = loca + gid * 2;
            if at + 4 > font.len() { break; }
            (be16(font, at) as usize * 2, be16(font, at + 2) as usize * 2)
        };
        if end <= start || end > glyf_len || start + 4 > glyf_len {
            continue;
        }
        // Glyphs past `numberOfHMetrics` keep only their advance (from the last
        // long metric) — their left side bearing still has its own entry, in an
        // array of int16 right after the long metrics (OT §hmtx). Reading the
        // last long metric's bearing for those glyphs is what put the middle
        // piece of the closing brace in Equation 11 of `nasa.pdf` page 35 a
        // side bearing too far left: gid 190 has lsb 420 in the trailing array,
        // while `numberOfHMetrics` is 190 and the last long metric says 28.
        let lsb = if gid < num_hmetrics {
            bei16(font, hmtx + gid * 4 + 2) as f32
        } else {
            let rel = num_hmetrics * 4 + (gid - num_hmetrics) * 2;
            if rel + 2 > hmtx_len {
                break;
            }
            bei16(font, hmtx + rel) as f32
        };
        let x_min = bei16(font, glyf + start + 2) as f32;
        let shift = lsb - x_min;
        if shift != 0.0 {
            shifts.insert(GlyphId(gid as u32), shift);
        }
    }
    Some(shifts)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-glyph sfnt with `glyf` at `x_min`, `hmtx` lsb `lsb`.
    fn program(x_min: i16, lsb: i16) -> Vec<u8> {
        // 4 tables: head, maxp, hhea, hmtx, loca, glyf -> keep it to 6
        let tags: [&[u8; 4]; 6] = [b"head", b"maxp", b"hhea", b"hmtx", b"loca", b"glyf"];
        let mut tables: Vec<Vec<u8>> = Vec::new();
        let mut head = vec![0u8; 54];
        head[50] = 0; // short loca
        head[18] = 8; // unitsPerEm high byte
        tables.push(head);
        let mut maxp = vec![0u8; 6];
        maxp[4] = 0;
        maxp[5] = 2; // numGlyphs = 2: glyph 0 and 1
        tables.push(maxp);
        let mut hhea = vec![0u8; 36];
        hhea[34] = 0;
        hhea[35] = 2;
        tables.push(hhea);
        let mut hmtx = vec![0u8; 8];
        hmtx[0..2].copy_from_slice(&500i16.to_be_bytes()); // advance of gid 0
        hmtx[2..4].copy_from_slice(&0i16.to_be_bytes());
        hmtx[4..6].copy_from_slice(&500i16.to_be_bytes());
        hmtx[6..8].copy_from_slice(&lsb.to_be_bytes());
        tables.push(hmtx);
        // glyph 0 empty, glyph 1 at x_min
        let mut loca = vec![0u8; 6];
        loca[0] = 0;
        loca[1] = 0; // gid 0: 0..0
        loca[2] = 0;
        loca[3] = 5; // gid 1: 5..10, in 2-byte words, so byte offsets 10..20
        loca[4] = 0;
        loca[5] = 10;
        tables.push(loca);
        let mut glyf = vec![0u8; 20];
        glyf[10 + 2 .. 10 + 4].copy_from_slice(&x_min.to_be_bytes());
        tables.push(glyf);

        assemble(&tags, tables)
    }

    /// Wrap `tables` in an sfnt header.
    fn assemble(tags: &[&[u8; 4]], tables: Vec<Vec<u8>>) -> Vec<u8> {
        let n = tags.len();
        let mut out = Vec::new();
        out.extend_from_slice(&[0, 1, 0, 0]);
        out.extend_from_slice(&(n as u16).to_be_bytes());
        out.extend_from_slice(&[0; 6]);
        let mut offset = 12 + n * 16;
        for (tag, data) in tags.iter().zip(tables.iter()) {
            out.extend_from_slice(*tag);
            out.extend_from_slice(&[0; 4]); // checksum
            out.extend_from_slice(&(offset as u32).to_be_bytes());
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            offset += data.len();
        }
        for data in tables {
            out.extend_from_slice(&data);
        }
        out
    }

    /// A three-glyph sfnt with one long metric: glyphs 1 and 2 carry only a
    /// left side bearing, in the int16 array after the long metrics
    /// (`numberOfHMetrics = 1`, OT §hmtx). Both outlines sit at `xMin = 0`.
    fn program_trailing(lsb: [i16; 2]) -> Vec<u8> {
        let tags: [&[u8; 4]; 6] = [b"head", b"maxp", b"hhea", b"hmtx", b"loca", b"glyf"];
        let mut head = vec![0u8; 54];
        head[50] = 0; // short loca
        head[18] = 8; // unitsPerEm 2048
        let mut maxp = vec![0u8; 6];
        maxp[4] = 0;
        maxp[5] = 3; // numGlyphs = 3
        let mut hhea = vec![0u8; 36];
        hhea[34] = 0;
        hhea[35] = 1; // numberOfHMetrics = 1
        let mut hmtx = Vec::new();
        hmtx.extend_from_slice(&500i16.to_be_bytes()); // advance of glyph 0
        hmtx.extend_from_slice(&0i16.to_be_bytes());
        for l in lsb {
            hmtx.extend_from_slice(&l.to_be_bytes());
        }
        // gid 1: 2-byte words 5..10, gid 2: 10..15 -> a 30-byte glyf table.
        let mut loca = vec![0u8; 8];
        loca[3] = 5;
        loca[5] = 10;
        loca[7] = 15;
        let mut glyf = vec![0u8; 30];
        glyf[12..14].copy_from_slice(&0i16.to_be_bytes());
        glyf[22..24].copy_from_slice(&0i16.to_be_bytes());
        assemble(&tags, vec![head, maxp, hhea, hmtx, loca, glyf])
    }

    #[test]
    fn side_bearing_mismatch_is_reported() {
        let shifts = shifts_from_program(&program(0, 420)).unwrap();
        assert_eq!(shifts.get(&GlyphId(1)), Some(&420.0));
        assert_eq!(shifts.get(&GlyphId(0)), None);
    }

    #[test]
    fn matching_side_bearing_is_not_reported() {
        assert!(shifts_from_program(&program(420, 420)).unwrap().is_empty());
    }

    #[test]
    fn trailing_side_bearings_are_read() {
        // gid 1 and 2 have no long metric; their bearings are in the array
        // after it, not in the last long metric.
        let shifts = shifts_from_program(&program_trailing([420, 0])).unwrap();
        assert_eq!(shifts.get(&GlyphId(1)), Some(&420.0));
        assert_eq!(shifts.get(&GlyphId(2)), None);
    }

    #[test]
    fn cff_is_left_alone() {
        let mut f = program(0, 420);
        f[0..4].copy_from_slice(b"OTTO");
        assert!(shifts_from_program(&f).is_none());
    }

    #[test]
    fn truncated_program_is_left_alone() {
        let f = program(0, 420);
        assert!(shifts_from_program(&f[.. 20]).is_none());
    }
}
