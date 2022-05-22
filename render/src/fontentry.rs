use std::collections::HashMap;
use font::{self, GlyphId, TrueTypeFont, CffFont, Type1Font, OpenTypeFont};
use pdf::encoding::BaseEncoding;
use pdf::font::{Font as PdfFont, Widths, CidToGidMap};
use pdf::object::{Resolve, RcRef};
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
    pub pdf_font: RcRef<PdfFont>,
    pub encoding: TextEncoding,
    pub widths: Option<Widths>,
    pub is_cid: bool,
    pub name: String,
}
impl FontEntry {
    pub fn build(font: FontRc, pdf_font: RcRef<PdfFont>, resolve: &impl Resolve) -> Result<FontEntry, PdfError> {
        let mut is_cid = pdf_font.is_cid();
        let encoding = pdf_font.encoding().clone();
        let base_encoding = encoding.as_ref().map(|e| &e.base);
        
        let to_unicode = t!(pdf_font.to_unicode(resolve).transpose());
        let mut font_codepoints = None;
        let glyph_unicode: HashMap<GlyphId, SmallString> = 
        if let Some(type1) = font.downcast_ref::<Type1Font>() {
            debug!("Font is Type1");
            font_codepoints = Some(&type1.codepoints);
            type1.unicode_names().map(|(gid, s)| (gid, s.into())).collect()
        } else if let Some(cmap) = font.downcast_ref::<TrueTypeFont>().and_then(|ttf| ttf.cmap.as_ref())
            .or_else(|| font.downcast_ref::<OpenTypeFont>().and_then(|otf| otf.cmap.as_ref())) {
            cmap.items().filter_map(|(cp, gid)| std::char::from_u32(cp).map(|c| (gid, c.into()))).collect()
        } else if let Some(cff) = font.downcast_ref::<CffFont>() {
            cff.unicode_map.iter().map(|(&u, &gid)| (GlyphId(gid as u32), u.into())).collect()
        } else {
            (0..font.num_glyphs())
                .filter_map(|gid| std::char::from_u32(gid).map(|c| (GlyphId(gid), c.into())))
                .collect()
        };
        
        debug!("to_unicode: {:?}", to_unicode);
        let build_map = || {
            if let Some(ref to_unicode) = to_unicode {
                let map = to_unicode.iter().map(|(cid, s)| {
                    let gid = font.gid_for_codepoint(cid as u32);
                    (cid, (gid, s.into()))
                }).collect();
                Some(map)
            } else {
                None
            }
        };
        
        let encoding = if let Some(map) = pdf_font.cid_to_gid_map() {
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
                    match font.gid_for_name(&name) {
                        Some(gid) => {
                            let unicode = glyphname_to_unicode(name).map(|s| s.into())
                                .or_else(|| std::char::from_u32(0xf000 + gid.0).map(SmallString::from));
                            
                            debug!("{} -> gid {:?}, unicode {:?}", cp, gid, unicode);
                            cmap.insert(cp as u16, (gid, unicode));
                        }
                        None => info!("no glyph for name {}", name)
                    }
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
            
            debug!("cmap: {:?}", &cmap);

            if cmap.len() == 0 {
                TextEncoding::CID(build_map())
            } else {
                TextEncoding::Cmap(cmap)
            }
        };
        
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
