use std::collections::HashMap;
use font::{self, Font, GlyphId};
use pdf::encoding::BaseEncoding;
use pdf::font::{Font as PdfFont, Widths, ToUnicodeMap};
use pdf::object::{Resolve, RcRef};
use pdf::error::PdfError;
use pdf_encoding::{Encoding, glyphname_to_unicode};
use std::rc::Rc;

#[derive(Debug)]
pub enum TextEncoding {
    CID,
    Cmap(HashMap<u16, GlyphId>)
}

pub struct FontEntry {
    pub font: Rc<dyn Font>,
    pub pdf_font: RcRef<PdfFont>,
    pub encoding: TextEncoding,
    pub widths: Option<Widths>,
    pub is_cid: bool,
    pub name: String,
    pub to_unicode: Option<ToUnicodeMap>,
}
impl FontEntry {
    pub fn build(font: Rc<dyn Font>, pdf_font: RcRef<PdfFont>, resolve: &impl Resolve) -> Result<FontEntry, PdfError> {
        let mut is_cid = pdf_font.is_cid();
        let encoding = pdf_font.encoding().clone();
        let base_encoding = encoding.as_ref().map(|e| &e.base);

        let mut to_unicode = t!(pdf_font.to_unicode().transpose());
        let encoding = if let Some(map) = pdf_font.cid_to_gid_map() {
            is_cid = true;
            let cmap = map.iter().enumerate().map(|(cid, &gid)| (cid as u16, GlyphId(gid as u32))).collect();
            TextEncoding::Cmap(cmap)
        } else if base_encoding == Some(&BaseEncoding::IdentityH) {
            is_cid = true;
            TextEncoding::CID
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
            if let (Some(e), false) = (source_encoding, to_unicode.is_some()) {
                let decoder = e.forward_map().ok_or(PdfError::Other { msg: format!("no forward map on encoding {:?}", e)})?;
                to_unicode = Some(ToUnicodeMap::create((0..=255).filter_map(|b| decoder.get(b).map(|c| (b as u16, c.to_string())))));
            }

            let font_encoding = font.encoding();
            debug!("{:?} -> {:?}", source_encoding, font_encoding);
            if let (Some(source), Some(to_unicode)) = (source_encoding.as_ref(), to_unicode.as_mut()) {
                let encoder = source.to(Encoding::Unicode).unwrap();
                for cid in 0 .. 256 {
                    if let Some(c) = encoder.translate(cid as u32) {
                        let c = std::char::from_u32(c).unwrap();
                        to_unicode.insert(cid, c.into());
                    }
                }
            }
            match (source_encoding, font_encoding) {
                (Some(source), Some(dest)) => {
                    if let Some(transcoder) = source.to(dest) {
                        for b in 0 .. 256 {
                            if let Some(gid) = transcoder.translate(b).and_then(|cp| font.gid_for_codepoint(cp)) {
                                cmap.insert(b as u16, gid);
                                //debug!("{} -> {:?}", b, gid);
                            }
                        }
                    }
                },
                (Some(source), None) => {
                    if let Some(encoder) = source.to(Encoding::Unicode) {
                        for b in 0 .. 256 {
                            if let Some(gid) = encoder.translate(b as u32).and_then(|c| font.gid_for_unicode_codepoint(c)) {
                                cmap.insert(b, gid);
                                //debug!("{} -> {:?}", b, gid);
                            }
                        }
                    }
                }
                _ => {
                    warn!("can't translate from text encoding {:?} to font encoding {:?}", base_encoding, font_encoding);
                    
                    // assuming same encoding
                    for cp in 0 .. 256 {
                        if let Some(gid) = font.gid_for_codepoint(cp) {
                            cmap.insert(cp as u16, gid);
                        }
                    }
                }
            }
            if let Some(encoding) = encoding {
                for (&cp, name) in encoding.differences.iter() {
                    //debug!("{} -> {}", cp, name);
                    match font.gid_for_name(&name) {
                        Some(gid) => {
                            cmap.insert(cp as u16, gid);
                        }
                        None => info!("no glyph for name {}", name)
                    }
                    if let Some(ref mut to_unicode) = to_unicode {
                        let u = glyphname_to_unicode(name).or_else(|| name.find(".").and_then(|i| glyphname_to_unicode(&name[..i])));
                        if let Some(unicode) = u {
                            to_unicode.insert(cp as u16, unicode.into());
                        }
                    }
                }
            }
            //debug!("cmap: {:?}", cmap);
            //debug!("to_unicode: {:?}", to_unicode);
            if cmap.is_empty() {
                TextEncoding::CID
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
            to_unicode,
        })
    }
}
