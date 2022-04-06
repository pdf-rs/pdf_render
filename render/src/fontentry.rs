use std::collections::HashMap;
use font::{self, Font, GlyphId, TrueTypeFont, CffFont, Type1Font};
use pdf::encoding::BaseEncoding;
use pdf::font::{Font as PdfFont, Widths, ToUnicodeMap, CidToGidMap};
use pdf::object::{Resolve, RcRef};
use pdf::error::PdfError;
use pdf_encoding::{Encoding, glyphname_to_unicode};
use std::sync::Arc;

#[derive(Debug)]
pub enum TextEncoding {
    CID(ToUnicodeMap),
    Cmap(HashMap<u16, (GlyphId, Option<String>)>)
}

pub struct FontEntry {
    pub font: Arc<dyn Font + Sync + Send>,
    pub pdf_font: RcRef<PdfFont>,
    pub encoding: TextEncoding,
    pub widths: Option<Widths>,
    pub is_cid: bool,
    pub name: String,
}
impl FontEntry {
    pub fn build(font: Arc<dyn Font + Sync + Send>, pdf_font: RcRef<PdfFont>, resolve: &impl Resolve) -> Result<FontEntry, PdfError> {
        let mut is_cid = pdf_font.is_cid();
        let encoding = pdf_font.encoding().clone();
        let base_encoding = encoding.as_ref().map(|e| &e.base);
        
        let to_unicode = t!(pdf_font.to_unicode(resolve).transpose()).unwrap_or_else(|| {
            if let Some(type1) = font.downcast_ref::<Type1Font>() {
                ToUnicodeMap::create(type1.unicode_names().map(|(gid, uni)| (gid.0 as u16, uni.into())))
            } else {
                let chars = (0..font.num_glyphs() as u16)
                    .filter_map(|cid| std::char::from_u32(cid as u32).map(|c| (cid, c.to_string())));
                ToUnicodeMap::create(chars)
            }
        });
        dbg!(&to_unicode);
        let encoding = if let Some(map) = pdf_font.cid_to_gid_map() {
            is_cid = true;
            match map {
                CidToGidMap::Identity => TextEncoding::CID(to_unicode),
                CidToGidMap::Table(ref data) => {
                    let cmap = data.iter().enumerate().map(|(cid, &gid)| {
                        let unicode = to_unicode.get(cid as u16).map(|s| s.into());
                        (cid as u16, (GlyphId(gid as u32), unicode))
                    }).collect();
                    TextEncoding::Cmap(cmap)
                }
            }
        } else if base_encoding == Some(&BaseEncoding::IdentityH) {
            is_cid = true;
            TextEncoding::CID(to_unicode)
        } else {
            let mut cmap = HashMap::new();
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
                                cmap.insert(b as u16, (gid, forward.get(b as u8).map(|c| c.to_string())));
                                debug!("{} -> {:?}", b, gid);
                            }
                        }
                    }
                },
                (Some(enc), None) => {
                    if let Some(encoder) = enc.to(Encoding::Unicode) {
                        for b in 0 .. 256 {
                            let unicode = encoder.translate(b as u32);
                            if let Some(gid) = unicode.and_then(|c| font.gid_for_unicode_codepoint(c)) {
                                cmap.insert(b, (gid, unicode.and_then(std::char::from_u32).map(|c| c.to_string())));
                                debug!("{} -> {:?}", b, gid);
                            }
                        }
                    }
                }
                _ => {
                    warn!("can't translate from text encoding {:?} to font encoding {:?}", base_encoding, font_encoding);
                    
                    // assuming same encoding
                    /*
                    for cp in 0 .. 256 {
                        if let Some(gid) = font.gid_for_codepoint(cp) {
                            let unicode = to_unicode.get(cp as u16).and_then(|s| s.chars().next())
                                .or_else(|| std::char::from_u32(0xf000 + cp));
                            cmap.insert(cp as u16, (gid, unicode));
                        }
                    }
                    */
                }
            }
            if let Some(encoding) = encoding {
                for (&cp, name) in encoding.differences.iter() {
                    debug!("{} -> {}", cp, name);
                    match font.gid_for_name(&name) {
                        Some(gid) => {
                            debug!(" -> {:?}", gid);
                            let unicode = to_unicode.get(cp as u16)
                                .or_else(|| glyphname_to_unicode(name))
                                .or_else(|| name.find(".").and_then(|i| glyphname_to_unicode(&name[..i])));
                            
                            cmap.insert(cp as u16, (gid, unicode.map(|s| s.into())));
                        }
                        None => info!("no glyph for name {}", name)
                    }
                }
            }
            
            if cmap.len() == 0 {
                TextEncoding::CID(to_unicode)
            } else {
                TextEncoding::Cmap(cmap)
            }
        };
        
        let widths = pdf_font.widths(resolve)?;
        let name = pdf_font.name.as_ref().ok_or_else(|| PdfError::Other { msg: "font has no name".into() })?.clone();
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
