use std::collections::{HashMap, HashSet};
use font::{Font, CffFont, Encoder, FontVariant, GlyphId, OpenTypeFont, TrueTypeFont, Type1Font};

#[cfg(feature="glyphmatcher")]
use glyphmatcher::FontDb;

use itertools::Itertools;
use pdf::encoding::BaseEncoding;
use pdf::font::{Font as PdfFont, Widths, CidToGidMap};
use pdf::object::{Resolve, MaybeRef};
use pdf::error::PdfError;
use pdf_encoding::{Encoding, glyphname_to_unicode};
use istring::SmallString;
use crate::font::FontRc;

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
            if let Some(encoding) = encoding {
                for (&cp, name) in encoding.differences.iter() {
                    let uni = glyphname_to_unicode(name);
                    let gid = font.gid_for_name(&name).or_else(||
                        uni.and_then(|s| s.chars().next()).and_then(|cp| font.gid_for_unicode_codepoint(cp as u32))
                    ).or_else(||
                        font.gid_for_codepoint(cp)
                    ).unwrap_or(GlyphId(cp));

                    let unicode = uni.map(|s| s.into())
                        .or_else(|| std::char::from_u32(0xf000 + gid.0).map(SmallString::from));

                    debug!("{} -> gid {:?}, unicode {:?}", cp, gid, unicode);
                    cmap.insert(cp as u16, (gid, unicode));
                }
            } else {
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
