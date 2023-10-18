use std::collections::HashMap;
use font::{self, GlyphId, TrueTypeFont, CffFont, Type1Font, OpenTypeFont};
use glyphmatcher::FontDb;
use pdf::encoding::BaseEncoding;
use pdf::font::{Font as PdfFont, Widths, CidToGidMap};
use pdf::object::{Resolve, MaybeRef};
use pdf::error::PdfError;
use pdf_encoding::{Encoding, glyphname_to_unicode};
use istring::SmallString;
use crate::font::FontRc;

#[derive(Debug)]
pub enum TextEncoding {
    CID(Option<HashMap<u16, (Option<GlyphId>, SmallString)>>),
    Cmap(HashMap<u16, (GlyphId, Option<SmallString>)>)
}

pub struct FontEntry {
    pub font: FontRc,
    pub pdf_font: MaybeRef<PdfFont>,
    pub encoding: TextEncoding,
    pub widths: Option<Widths>,
    pub is_cid: bool,
    pub name: String,
}


impl FontEntry {
    pub fn build(font: FontRc, pdf_font: MaybeRef<PdfFont>, font_db: Option<&FontDb>, resolve: &impl Resolve) -> Result<FontEntry, PdfError> {
        let mut is_cid = pdf_font.is_cid();

        let name = match pdf_font.data {
            pdf::font::FontData::Type0(ref t0) => t0.descendant_fonts[0].name.as_ref(),
            _ => pdf_font.name.as_ref()
        };

        let encoding = pdf_font.encoding().clone();
        let base_encoding = encoding.as_ref().map(|e| &e.base);
        
        let to_unicode = t!(pdf_font.to_unicode(resolve).transpose());
        let mut font_codepoints = None;

        let font_cmap = font.downcast_ref::<TrueTypeFont>().and_then(|ttf| ttf.cmap.as_ref())
        .or_else(|| font.downcast_ref::<OpenTypeFont>().and_then(|otf| otf.cmap.as_ref()));

        let glyph_unicode: HashMap<GlyphId, SmallString> = {
            if let Some(type1) = font.downcast_ref::<Type1Font>() {
                debug!("Font is Type1");
                font_codepoints = Some(&type1.codepoints);
                type1.unicode_names().map(|(gid, s)| (gid, s.into())).collect()
            } else if let Some(cmap) = font_cmap {
                cmap.items().filter_map(|(cp, gid)| std::char::from_u32(cp).map(|c| (gid, c.into()))).collect()
            } else if let Some(cff) = font.downcast_ref::<CffFont>() {
                cff.unicode_map.iter().map(|(&u, &gid)| (GlyphId(gid as u32), u.into())).collect()
            } else {
                (0..font.num_glyphs())
                    .filter_map(|gid| std::char::from_u32(gid).map(|c| (GlyphId(gid), c.into())))
                    .collect()
            }
        };
        
        debug!("to_unicode: {:?}", to_unicode);
        let build_map = || {
            if let Some(ref to_unicode) = to_unicode {
                let mut num1 = 0;
                // dbg!(font.encoding());
                let mut map: HashMap<_, _> = to_unicode.iter().map(|(cid, s)| {
                    let gid = font.gid_for_codepoint(cid as u32);
                    if gid.is_some() {
                        num1 += 1;
                    }
                    (cid, (gid, s.into()))
                }).collect();
                if let Some(cff) = font.downcast_ref::<CffFont>() {
                    let mut num2 = 0;
                    let map2: HashMap<_, _> = to_unicode.iter().map(|(cid, s)| {
                        let gid = cff.sid_map.get(&cid).map(|&n| GlyphId(n as u32));
                        if gid.is_some() {
                            num2 += 1;
                        }
                        (cid, (gid, s.into()))
                    }).collect();
                    if num2 > num1 {
                        map = map2;
                    }
                }
                Some(map)
            } else if let Some(cmap) = font_cmap {
                Some(cmap.items().map(|(cid, gid)| (cid as u16, (Some(gid), char::from_u32(0xF000 + cid).unwrap().into()))).collect())
            } else if let Some(cff) = font.downcast_ref::<CffFont>() {
                if cff.cid {
                    Some(cff.sid_map.iter().map(|(&sid, &gid)|  (sid as u16, (Some(GlyphId(gid as u32)), char::from_u32(0xF000 + sid as u32).unwrap().into()))).collect())
                } else {
                    Some(cff.codepoint_map.iter().enumerate().filter(|&(_, &gid)| gid != 0).map(|(cid, &gid)| (cid as u16, (Some(GlyphId(gid as u32)), char::from_u32(0xF000 + gid as u32).unwrap().into()))).collect())
                }
            } else {
                None
            }
        };
        
        let mut encoding = if let Some(map) = pdf_font.cid_to_gid_map() {
            is_cid = true;
            debug!("gid to cid map: {:?}", map);
            match map {
                CidToGidMap::Identity => {
                    if let Some(ref to_unicode) = to_unicode {
                        let map = to_unicode.iter().map(|(cid, s)| {
                            (cid, (Some(GlyphId(cid as u32)), s.into()))
                        }).collect();
                        TextEncoding::CID(Some(map))
                    } else {
                        TextEncoding::CID(None)
                    }
                }
                CidToGidMap::Table(ref data) => {
                    let cmap = data.iter().enumerate().map(|(cid, &gid)| {
                        let unicode = match to_unicode {
                            Some(ref u) => u.get(cid as u16).map(|s| s.into()),
                            None => glyph_unicode.get(&GlyphId(gid as u32)).cloned()
                        };
                        (cid as u16, (GlyphId(gid as u32), unicode))
                    }).collect();
                    TextEncoding::Cmap(cmap)
                }
            }
        } else if base_encoding == Some(&BaseEncoding::IdentityH) {
            is_cid = true;
            TextEncoding::CID(build_map())
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
                    if let Some(cff) = font.downcast_ref::<CffFont>() {
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
                TextEncoding::CID(build_map())
            } else {
                TextEncoding::Cmap(cmap)
            }
        };
        
        if let Some(font_db) = font_db {
            if let Some(name) = name {
                let name = name.split("+").nth(1).unwrap_or(name);
                let ps_name = name.split("-").nth(0).unwrap();

                debug!("request font {ps_name}");
                if let Some(map) = font_db.check_font(ps_name, &*font) {
                    if map.len() > 0 {
                        info!("Got good unicode map for {ps_name}");
                    } else {
                        info!("font {ps_name} did not match");
                    }
                    match encoding {
                        TextEncoding::CID(Some(ref mut cmap)) => {
                            for n in 0 .. font.num_glyphs() {
                                use std::collections::hash_map::Entry;
                                match cmap.entry(n as u16) {
                                    Entry::Occupied(mut e) => {
                                        let (gid2, uni) = e.get_mut();
                                        let gid = GlyphId(n);
                                        if let Some(new_uni) = map.get(&gid) {
                                            if new_uni != uni {
                                                debug!("updating {gid:?} from {uni:?} to {new_uni:?}");
                                                *gid2 = Some(gid);
                                                *uni = new_uni.clone();
                                            }
                                        }
                                    }
                                    Entry::Vacant(e) => {
                                        let gid = GlyphId(n);
                                        if let Some(uni) = map.get(&gid) {
                                            e.insert((Some(gid), uni.clone()));
                                        } else {
                                            e.insert((Some(gid), std::char::from_u32(0xF000 + n).unwrap().into()));
                                        }
                                    }
                                }
                            }
                        }
                        TextEncoding::CID(ref mut opt) => {
                            let cmap = (0 .. font.num_glyphs()).map(|n| {
                                let gid = GlyphId(n as u32);
                                map.get(&gid).map(|uni| (n as u16, (Some(gid), uni.clone())))
                                    .unwrap_or_else(|| (n as u16, (Some(gid), std::char::from_u32(0xF000 + n).unwrap().into())))
                            }).collect();
                            *opt = Some(cmap);
                        }
                        TextEncoding::Cmap(ref mut cmap) => {
                            for (cp, (gid, uni)) in cmap.iter_mut() {
                                let good_uni = map.get(gid);
                                // dbg!(&gid, &good_uni, &uni);
                                match (uni.as_mut(), good_uni) {
                                    (Some(uni), Some(good_uni)) if uni != good_uni => {
                                        //println!("mismatching unicode for gid {gid:?}: {good_uni:?} != {uni:?}");
                                        *uni = good_uni.clone();
                                    }
                                    (None, Some(good_uni)) => {
                                        //println!("missing unicode for gid {gid:?} added {good_uni:?}");
                                        *uni = Some(good_uni.clone());
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                } else {
                    info!("missing {ps_name} font");
                }
            }
        }

        let widths = pdf_font.widths(resolve)?;
        let name = pdf_font.name.as_ref().ok_or_else(|| PdfError::Other { msg: "font has no name".into() })?.as_str().into();
        Ok(FontEntry {
            font,
            pdf_font,
            encoding,
            is_cid,
            widths,
            name,
        })
    }
}

impl globalcache::ValueSize for FontEntry {
    fn size(&self) -> usize {
        1 // TODO
    }
}
