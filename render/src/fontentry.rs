use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use font::{Font, CffFont, Encoder, FontVariant, GlyphId};

#[cfg(feature="glyphmatcher")]
use glyphmatcher::FontDb;

use itertools::Itertools;
use log::{debug, info, warn};
use pdf::encoding::BaseEncoding;
use pdf::font::{Font as PdfFont, Widths, CidToGidMap};
use pdf::object::{Resolve, MaybeRef};
use pdf::error::PdfError;
use pdf::t;
use pdf_encoding::{Encoding, glyphname_to_unicode};
use istring::SmallString;
use crate::font::FontRc;

/// The Adobe Symbol code for the character a glyph name stands for: `/bullet`
/// is U+2022, which Symbol keeps at code 0xb7.
fn symbol_code(unicode: Option<&str>) -> Option<u32> {
    unicode
        .and_then(|s| s.chars().next())
        .and_then(|c| Encoding::AdobeSymbol.reverse_map().and_then(|m| m.get(c as u32)))
        .map(|code| code as u32)
}

/// The glyph name pdf_encoding knows for a character. The free clones of the
/// standard fonts carry glyph names but often no unicode map, so a name is the
/// only handle on a glyph.
fn glyphname_for_char(c: char) -> Option<&'static str> {
    static BY_CHAR: OnceLock<HashMap<u32, &'static str>> = OnceLock::new();
    let map = BY_CHAR.get_or_init(|| {
        pdf_encoding::GLYPH_LIST.iter()
            .filter_map(|&(name, unicode)| unicode.chars().next().map(|c| (c as u32, name)))
            .collect()
    });
    map.get(&(c as u32)).copied()
}

/// The built-in encoding of a standard font that has no `/Encoding` entry in
/// the PDF: Symbol and ZapfDingbats keep their glyphs at PostScript codes that
/// are not ASCII, so a byte of the text has to go through the encoding.
fn builtin_standard_encoding(name: Option<&str>) -> Option<Encoding> {
    let name = name?;
    let ps_name = name.split('+').last().unwrap_or(name);
    match ps_name {
        "Symbol" => Some(Encoding::AdobeSymbol),
        "ZapfDingbats" | "ZapfDingbatsStd" => Some(Encoding::AdobeZdingbat),
        _ => None,
    }
}

/// The glyph name the ZapfDingbats built-in encoding gives a code. Those glyphs
/// are named `a1`..`a191`, and no glyph list turns such a name into a character,
/// so the name has to come from the code itself: the encoding assigns them in
/// name order.
fn zdingbat_glyph_name(b: u8) -> Option<String> {
    match b {
        0x20 => Some("space".to_string()),
        0x21 ..= 0x7e => Some(format!("a{}", b as u32 - 0x20)),
        0xa1 ..= 0xfe => Some(format!("a{}", b as u32 - 60)),
        _ => None,
    }
}

/// Map every byte of `enc` to a glyph of `font`, going through the glyph name
/// where the font program has no unicode map of its own.
fn fill_builtin_cmap<E: Encoder + 'static>(
    cmap: &mut HashMap<u16, (GlyphId, Option<SmallString>)>,
    font: &FontRc<E>,
    enc: Encoding,
) {
    let Some(forward) = enc.forward_map() else { return };
    for b in 0 ..= 255u8 {
        let Some(c) = forward.get(b) else { continue };
        let gid = font.gid_for_unicode_codepoint(c as u32)
            .or_else(|| glyphname_for_char(c).and_then(|n| font.gid_for_name(n)))
            .or_else(|| match enc {
                Encoding::AdobeZdingbat => zdingbat_glyph_name(b).and_then(|n| font.gid_for_name(&n)),
                _ => None,
            });
        if let Some(gid) = gid {
            cmap.insert(b as u16, (gid, Some(c.into())));
        }
    }
}

pub struct FontEntry<E: Encoder> {
    pub font: FontRc<E>,
    pub pdf_font: MaybeRef<PdfFont>,
    pub cmap: HashMap<u16, (GlyphId, Option<SmallString>)>,
    pub widths: Option<Widths>,
    pub is_cid: bool,
    pub name: String,
}


impl<E: Encoder + 'static> FontEntry<E> {
    pub fn build(font: FontRc<E>, pdf_font: MaybeRef<PdfFont>,
        #[cfg(feature="glyphmatcher")]
        font_db: Option<&FontDb>, resolve: &impl Resolve, require_unique_unicode: bool) -> Result<FontEntry<E>, PdfError> {
        let mut is_cid = pdf_font.is_cid();

        let name = match pdf_font.data {
            pdf::font::FontData::Type0(ref t0) => t0.descendant_fonts[0].name.as_ref(),
            _ => pdf_font.name.as_ref()
        };

        let encoding = pdf_font.encoding().clone();
        let base_encoding = encoding.as_ref().map(|e| &e.base);

        let to_unicode = t!(pdf_font.to_unicode(resolve).transpose());
        let mut font_codepoints = None;

        let font_cmap = match *font {
            FontVariant::TrueType(ref ttf) => ttf.cmap.as_ref(),
            FontVariant::OpenType(ref otf) => otf.cmap.as_ref(),
            _ => None
        };

        let glyph_unicode: HashMap<GlyphId, SmallString> = {
            if let FontVariant::Type1(ref type1) = *font {
                debug!("Font is Type1");
                font_codepoints = Some(&type1.codepoints);
                type1.unicode_names().map(|(gid, s)| (gid, s.into())).collect()
            } else if let Some(cmap) = font_cmap {
                cmap.items().filter_map(|(cp, gid)| std::char::from_u32(cp).map(|c| (gid, c.into()))).collect()
            } else if let FontVariant::Cff(ref cff) = *font {
                cff.unicode_map.iter().map(|(&u, &gid)| (GlyphId(gid as u32), u.into())).collect()
            } else {
                (0..font.num_glyphs())
                    .filter_map(|gid| std::char::from_u32(gid).map(|c| (GlyphId(gid), c.into())))
                    .collect()
            }
        };

        debug!("to_unicode: {:?}", to_unicode);
        let build_map = || -> HashMap<u16, (GlyphId, Option<SmallString>)> {
            if let Some(ref to_unicode) = to_unicode {
                let mut num1 = 0;
                // dbg!(font.encoding());
                let mut map: HashMap<_, _> = to_unicode.iter().map(|(cid, s)| {
                    let gid = font.gid_for_codepoint(cid as u32);
                    if gid.is_some() {
                        num1 += 1;
                    }
                    (cid, (gid.unwrap_or(GlyphId(cid as u32)), Some(s.into())))
                }).collect();
                if let FontVariant::Cff(ref cff) = *font {
                    let mut num2 = 0;
                    let map2: HashMap<_, _> = to_unicode.iter().map(|(cid, s)| {
                        let gid = cff.sid_map.get(&cid).map(|&n| GlyphId(n as u32));
                        if gid.is_some() {
                            num2 += 1;
                        }
                        (cid, (gid.unwrap_or(GlyphId(cid as u32)), Some(s.into())))
                    }).collect();
                    if num2 > num1 {
                        map = map2;
                    }
                }
                map
            } else if let Some(cmap) = font_cmap {
                cmap.items().map(|(cid, gid)| (cid as u16, (gid, None))).collect()
            } else if let FontVariant::Cff(ref cff) = *font {
                if cff.cid {
                    cff.sid_map.iter().map(|(&sid, &gid)|  (sid as u16, (GlyphId(gid as u32), None))).collect()
                } else {
                    cff.codepoint_map.iter().enumerate().filter(|&(_, &gid)| gid != 0).map(|(cid, &gid)| (cid as u16, (GlyphId(gid as u32), None))).collect()
                }
            } else {
                Default::default()
            }
        };

        let mut cmap = if let Some(map) = pdf_font.cid_to_gid_map() {
            is_cid = true;
            debug!("gid to cid map: {:?}", map);
            match map {
                CidToGidMap::Identity => {
                    let mut map: HashMap<_, _> = (0 .. font.num_glyphs()).map(|n| (n as u16, (GlyphId(n as u32), None))).collect();
                    if let Some(ref to_unicode) = to_unicode {
                        for (cid, s) in to_unicode.iter() {
                            if let Some((gid, uni)) = map.get_mut(&cid) {
                                *uni = Some(s.into());
                            }
                        }
                    }
                    map
                }
                CidToGidMap::Table(ref data) => {
                    data.iter().enumerate().map(|(cid, &gid)| {
                        let unicode = match to_unicode {
                            Some(ref u) => u.get(cid as u16).map(|s| s.into()),
                            None => glyph_unicode.get(&GlyphId(gid as u32)).cloned()
                        };
                        (cid as u16, (GlyphId(gid as u32), unicode))
                    }).collect()
                }
            }
        } else if base_encoding == Some(&BaseEncoding::IdentityH) {
            is_cid = true;
            build_map()
        } else {
            let mut cmap = HashMap::<u16, (GlyphId, Option<SmallString>)>::new();

            let source_encoding = match base_encoding {
                Some(BaseEncoding::StandardEncoding) => Some(Encoding::AdobeStandard),
                Some(BaseEncoding::SymbolEncoding) => Some(Encoding::AdobeSymbol),
                Some(BaseEncoding::WinAnsiEncoding) => Some(Encoding::WinAnsiEncoding),
                Some(BaseEncoding::MacRomanEncoding) => Some(Encoding::MacRomanEncoding),
                Some(BaseEncoding::MacExpertEncoding) => Some(Encoding::AdobeExpert),
                ref e => {
                    warn!("unsupported pdf encoding {:?}", e);
                    None
                }
            };

            let font_encoding = font.encoding();
            debug!("{:?} -> {:?}", source_encoding, font_encoding);

            match (source_encoding, font_encoding) {
                (Some(source), Some(dest)) => {
                    if let Some(transcoder) = source.to(dest) {
                        let forward = source.forward_map().unwrap();
                        for b in 0 .. 256 {
                            if let Some(gid) = transcoder.translate(b).and_then(|cp| font.gid_for_codepoint(cp)) {
                                cmap.insert(b as u16, (gid, forward.get(b as u8).map(|c| c.into())));
                                //debug!("{} -> {:?}", b, gid);
                            }
                        }
                    }
                },
                (Some(enc), None) => {
                    if let Some(encoder) = enc.to(Encoding::Unicode) {
                        for b in 0 .. 256 {
                            let unicode = encoder.translate(b as u32);
                            if let Some(gid) = unicode.and_then(|c| font.gid_for_unicode_codepoint(c)) {
                                cmap.insert(b, (gid, unicode.and_then(std::char::from_u32).map(|c| c.into())));
                                debug!("{} -> {:?}", b, gid);
                            }
                        }
                    }
                }
                _ => {
                    if let FontVariant::Cff(ref cff) = *font {
                        for (cp, &gid) in cff.codepoint_map.iter().enumerate() {
                            let gid = GlyphId(gid as u32);
                            let unicode = glyph_unicode.get(&gid).cloned();
                            cmap.insert(cp as u16, (gid, unicode));
                        }
                    } else {
                        warn!("can't translate from text encoding {:?} to font encoding {:?}", base_encoding, font_encoding);
                    }
                    // assuming same encoding


                }
            }
            // Symbol and the free clones of it put their glyphs at PostScript
            // codes (bullet at 0xb7, not at U+2022), and the clones often carry
            // no glyph names at all. A `/Differences` entry like `/1 /bullet`
            // therefore misses every lookup above; go through the Symbol code
            // for the character the name stands for.
            let symbolic = encoding.map(|e| e.base == BaseEncoding::SymbolEncoding).unwrap_or(false)
                || name.map(|n| n.as_str() == "Symbol").unwrap_or(false);
            if let Some(encoding) = encoding {
                for (&cp, name) in encoding.differences.iter() {
                    let uni = glyphname_to_unicode(name);
                    let mut gid = font.gid_for_name(&name).or_else(||
                        uni.and_then(|s| s.chars().next()).and_then(|cp| font.gid_for_unicode_codepoint(cp as u32))
                    ).or_else(||
                        font.gid_for_codepoint(cp)
                    );
                    if gid.is_none() && symbolic {
                        gid = symbol_code(uni).and_then(|code| font.gid_for_codepoint(code));
                    }
                    let gid = gid.unwrap_or(GlyphId(cp));

                    let unicode = uni.map(|s| s.into())
                        .or_else(|| std::char::from_u32(0xf000 + gid.0).map(SmallString::from));

                    debug!("{} -> gid {:?}, unicode {:?}", cp, gid, unicode);
                    cmap.insert(cp as u16, (gid, unicode));
                }
            } else {
                // A font with no /Encoding uses its built-in one. Symbol and
                // ZapfDingbats keep their glyphs at codes that are not ASCII,
                // and their free clones leave nothing above that maps a byte
                // to a glyph (no unicode map, all-zero codepoints), so build
                // the map from the standard encoding of those two.
                let mut from_builtin = false;
                if let Some(enc) = builtin_standard_encoding(name.map(|n| n.as_str())) {
                    cmap.clear();
                    fill_builtin_cmap(&mut cmap, &font, enc);
                    from_builtin = !cmap.is_empty();
                    debug!("built-in {:?} encoding gave {} glyphs", enc, cmap.len());
                }
                if !from_builtin {
                    if let Some(ref u) = to_unicode {
                        debug!("using to_unicode to build cmap");
                        for (cp, unicode) in u.iter() {
                            if let Some(gid) = font.gid_for_unicode_codepoint(cp as u32) {
                                cmap.insert(cp as u16, (gid, Some(unicode.into())));
                            }
                        }
                    } else if let Some(codepoints) = font_codepoints {
                        for (&cp, &gid) in codepoints.iter() {
                            cmap.insert(cp as u16, (GlyphId(gid), glyph_unicode.get(&GlyphId(gid)).cloned()));
                        }
                    } else {
                        debug!("assuming text has unicode codepoints");
                        for (&gid, unicode) in glyph_unicode.iter() {
                            if let Some(cp) = unicode.chars().next() {
                                cmap.insert(cp as u16, (gid, Some(unicode.clone())));
                            }
                        }
                    }
                }
            }


            if cmap.len() == 0 {
                is_cid = true;
                build_map()
            } else {
                cmap
            }
        };

        #[cfg(feature="glyphmatcher")]
        if let Some(font_db) = font_db {
            if let Some(name) = name {
                let ps_name = name.split("+").nth(1).unwrap_or(name);

                info!("request font {ps_name} ({})", name.as_str());
                if let Some(map) = font_db.check_font(ps_name, &*font) {
                    if map.len() > 0 {
                        info!("Got good unicode map for {ps_name}");
                    } else {
                        info!("font {ps_name} did not match");
                    }
                    for (cp, (gid, uni)) in cmap.iter_mut() {
                        let good_uni = map.get(gid);
                        match (uni.as_mut(), good_uni) {
                            (Some(uni), Some(good_uni)) if uni != good_uni => {
                                // println!("mismatching unicode for gid {gid:?}: {good_uni:?} != {uni:?}");
                                *uni = good_uni.clone();
                            }
                            (None, Some(good_uni)) => {
                                // println!("missing unicode for gid {gid:?} added {good_uni:?}");
                                *uni = Some(good_uni.clone());
                            }
                            (Some(uni), None) => {
                                // println!("glyph {} missing (has uni {:?})", gid.0, uni);
                            }
                            _ => {}
                        }
                    }
                } else {
                    info!("missing {ps_name} font");
                }
            }
        }

        let widths = pdf_font.widths(resolve)?;
        let name = pdf_font.name.as_ref().ok_or_else(|| PdfError::Other { msg: "font has no name".into() })?.as_str().into();

        if require_unique_unicode {
            let mut next_code = 0xE000;
            let mut by_gid: Vec<_> = cmap.values_mut().collect();
            by_gid.sort_unstable_by_key(|t| t.0.0);

            let reserved_in_used: HashSet<u32> = by_gid.iter().map(|(gid, _)| gid.0).filter(|gid| (0xE000 .. 0xF800).contains(gid)).collect();

            if reserved_in_used.len() > 0 {
                info!("gid in privated use area: {}", reserved_in_used.iter().format(", "));
            }

            let mut rev_map = HashMap::new();
            for (gid, uni_o) in by_gid.iter_mut() {
                if let Some(uni) = uni_o {
                    use std::collections::hash_map::Entry;

                    match rev_map.entry(uni.clone()) {
                        Entry::Vacant(e) => {
                            e.insert(*gid);
                        }
                        Entry::Occupied(e) => {
                            info!("Duplicate unicode {uni:?} for {gid:?} and {:?}", e.get());
                            *uni_o = None;
                        }
                    }
                }
            }

            for (gid, uni) in by_gid.iter_mut() {
                if uni.is_none() && !(*font).is_empty_glyph(*gid) {
                    *uni = Some(std::char::from_u32(next_code).unwrap().into());

                    next_code += 1;
                    while reserved_in_used.contains(&next_code) {
                        next_code += 1;
                    }

                    if next_code >= 0xF8000 {
                        warn!("too many unmapped glpyhs in {:?}", font.name().postscript_name);
                        break;
                    }
                }
            }
            if next_code > 0xE000 {
                info!("mapped {} glyphs in private use area", next_code - 0xE000);
            }

        }

        Ok(FontEntry {
            font,
            pdf_font,
            cmap,
            is_cid,
            widths,
            name,
        })
    }
}

impl<E: Encoder> globalcache::ValueSize for FontEntry<E> {
    fn size(&self) -> usize {
        1 // TODO
    }
}

#[cfg(test)]
mod tests {
    use super::symbol_code;
    use pdf_encoding::Encoding;

    #[test]
    fn a_glyph_name_goes_through_the_symbol_code() {
        // `/bullet` is U+2022; Symbol and its clones keep it at 0xb7, and the
        // clones carry no glyph names, so only the code leads to a glyph.
        assert_eq!(symbol_code(Some("\u{2022}")), Some(0xb7));
        assert_eq!(symbol_code(Some("\u{3b1}")), Some(0x61)); // alpha
        // Not a Symbol character: nothing to translate.
        assert_eq!(symbol_code(Some("A")), None);
        assert_eq!(symbol_code(None), None);
    }

    #[test]
    fn the_symbolic_standard_fonts_use_their_builtin_encoding() {
        assert_eq!(super::builtin_standard_encoding(Some("Symbol")), Some(Encoding::AdobeSymbol));
        assert_eq!(super::builtin_standard_encoding(Some("ABCDEF+Symbol")), Some(Encoding::AdobeSymbol));
        assert_eq!(super::builtin_standard_encoding(Some("ZapfDingbats")), Some(Encoding::AdobeZdingbat));
        // The other standard fonts are not symbolic: their bytes are text.
        assert_eq!(super::builtin_standard_encoding(Some("Helvetica")), None);
        assert_eq!(super::builtin_standard_encoding(None), None);
    }

    #[test]
    fn zapfdingbats_glyphs_are_named_after_their_code() {
        // The encoding assigns a1..a94 to 0x21..0x7e and a101.. to 0xa1..
        assert_eq!(super::zdingbat_glyph_name(0x20).as_deref(), Some("space"));
        assert_eq!(super::zdingbat_glyph_name(0x21).as_deref(), Some("a1"));
        assert_eq!(super::zdingbat_glyph_name(0x61).as_deref(), Some("a65"));
        assert_eq!(super::zdingbat_glyph_name(0x7e).as_deref(), Some("a94"));
        assert_eq!(super::zdingbat_glyph_name(0xa1).as_deref(), Some("a101"));
        assert_eq!(super::zdingbat_glyph_name(0xfe).as_deref(), Some("a194"));
        assert_eq!(super::zdingbat_glyph_name(0x90), None);
    }
}
