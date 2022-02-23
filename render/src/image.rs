use pdf::object::*;
use pdf::error::PdfError;
use std::path::Path;
use pathfinder_color::ColorU;

pub struct ImageData {
    pub data: Vec<ColorU>,
    pub width: u32,
    pub  height: u32,
}
impl ImageData {
    pub fn rgba_data(&self) -> &[u8] {
        let ptr: *const ColorU = self.data.as_ptr();
        let len = self.data.len();
        unsafe {
            std::slice::from_raw_parts(ptr.cast(), 4 * len)
        }
    }
}

use std::borrow::Cow;
fn resize_alpha(data: &[u8], src_width: u32, src_height: u32, dest_width: u32, dest_height: u32) -> Option<Vec<u8>> {
    use image::{ImageBuffer, imageops::{resize, FilterType}, Luma};

    let src: ImageBuffer<Luma<u8>, &[u8]> = ImageBuffer::from_raw(src_width, src_height, data)?;
    let dest = resize(&src, dest_width, dest_height, FilterType::CatmullRom);

    Some(dest.into_raw())
}

pub fn load_image(image: &ImageXObject, resolve: &impl Resolve) -> Result<ImageData, PdfError> {
    let raw_data = image.image_data()?;

    let pixel_count = image.width as usize * image.height as usize;
    if raw_data.len() % pixel_count != 0 {
        warn!("invalid data length {} bytes for {} pixels", raw_data.len(), pixel_count);
        return Err(PdfError::Other { msg: format!("image data is {} (not a multiple of {}).", raw_data.len(), pixel_count)});
    }
    info!("smask: {:?}", image.smask);

    let mask = t!(image.smask.map(|r| resolve.get(r)).transpose());
    let alpha = match mask {
        Some(ref mask) => {
            let data = t!(mask.data());
            let mask_width = mask.width as usize;
            let mask_height = mask.height as usize;
            let bits = mask_width * mask_height * mask.bits_per_component as usize;
            assert_eq!(data.len(), (bits + 7) / 9);

            let mut alpha: Cow<[u8]> = match mask.bits_per_component {
                1 => data.iter().flat_map(|&b| (0..8).map(move |i| ex(b >> i, 1))).collect::<Vec<u8>>().into(),
                2 => data.iter().flat_map(|&b| (0..4).map(move |i| ex(b >> 2*i, 2))).collect::<Vec<u8>>().into(),
                4 => data.iter().flat_map(|&b| (0..2).map(move |i| ex(b >> 4*i, 4))).collect::<Vec<u8>>().into(),
                8 => data.into(),
                12 => data.chunks_exact(3).flat_map(|c| [c[0], c[1] << 4 | c[2] >> 4]).collect::<Vec<u8>>().into(),
                16 => data.chunks_exact(2).map(|c| c[0]).collect::<Vec<u8>>().into(),
                n => return Err(PdfError::Other { msg: format!("invalid bits per component {}", n)})
            };
            if mask.width != image.width || mask.height != image.height {
                alpha = resize_alpha(&*alpha, mask.width, mask.height, image.width, image.height).unwrap().into();
            }
            alpha
        }
        None => Cow::from(&[][..])
    };
    fn ex(b: u8, bits: u8) -> u8 {
        (((b as u16 + 1) >> (8 - bits)) - 1) as u8
    }
    
    fn resolve_cs(cs: &ColorSpace) -> Option<&ColorSpace> {
        match cs {
            ColorSpace::Icc(icc) => icc.info.info.alternate.as_ref().map(|b| &**b),
            _ => Some(cs),
        }
    }
    let cs = image.color_space.as_ref().and_then(resolve_cs);
    let alpha = alpha.iter().cloned().chain(std::iter::repeat(255));
    let data_ratio = raw_data.len() / pixel_count;
    let data = match data_ratio {
        1 => match cs {
            Some(ColorSpace::DeviceGray) => {
                assert_eq!(raw_data.len(), pixel_count);
                raw_data.iter().zip(alpha).map(|(&g, a)| ColorU { r: g, g: g, b: g, a }).collect()
            }
            Some(ColorSpace::Indexed(ref base, ref lookup)) => {
                match resolve_cs(&**base) {
                    Some(ColorSpace::DeviceRGB) => {
                        raw_data.iter().zip(alpha).map(|(&b, a)| {
                            let off = b as usize * 3;
                            let c = lookup.get(off .. off + 3).unwrap_or(&[0; 3]);
                            ColorU { r: c[0], g: c[1], b: c[2], a }
                        }).collect()
                    }
                    Some(ColorSpace::DeviceCMYK) => {
                        raw_data.iter().zip(alpha).map(|(&b, a)| {
                            let off = b as usize * 4;
                            let c = lookup.get(off .. off + 4).unwrap_or(&[0; 4]);
                            cmyk2color(c.try_into().unwrap(), a)
                        }).collect()
                    }
                    _ => unimplemented!("base cs={:?}", base),
                }
            }
            Some(ColorSpace::Separation(_, ref alt, ref func)) => {
                let mut lut = [[0u8; 3]; 256];

                match resolve_cs(alt) {
                    Some(ColorSpace::DeviceRGB) => {
                        for (i, rgb) in lut.iter_mut().enumerate() {
                            let mut c = [0.; 3];
                            func.apply(&[i as f32 / 255.], &mut c)?;
                            let [r, g, b] = c;
                            *rgb = [(r * 255.) as u8, (g * 255.) as u8, (b * 255.) as u8];
                        }
                    }
                    Some(ColorSpace::DeviceCMYK) => {
                        for (i, rgb) in lut.iter_mut().enumerate() {
                            let mut c = [0.; 4];
                            func.apply(&[i as f32 / 255.], &mut c)?;
                            let [c, m, y, k] = c;
                            *rgb = cmyk2rgb([(c * 255.) as u8, (m * 255.) as u8, (y * 255.) as u8, (k * 255.) as u8]);
                        }
                    }
                    _ => unimplemented!("alt cs={:?}", alt),
                }
                raw_data.iter().zip(alpha).map(|(&b, a)| {
                    let [r, g, b] = lut[b as usize];
                    ColorU { r, g, b, a }
                }).collect()
            }
            None => {
                info!("image has data/pixel ratio of 1, but no colorspace");
                assert_eq!(raw_data.len(), pixel_count);
                raw_data.iter().zip(alpha).map(|(&g, a)| ColorU { r: g, g: g, b: g, a }).collect()
            }
            _ => unimplemented!("cs={:?}", cs),
        }
        3 => {
            if !matches!(cs, Some(ColorSpace::DeviceRGB)) {
                info!("image has data/pixel ratio of 3, but colorspace is {:?}", cs);
            }
            raw_data.chunks_exact(3).zip(alpha).map(|(c, a)| ColorU { r: c[0], g: c[1], b: c[2], a }).collect()
        }
        4 => {
            if !matches!(cs, Some(ColorSpace::DeviceCMYK)) {
                info!("image has data/pixel ratio of 4, but colorspace is {:?}", cs);
            }
            cmyk2color_arr(&raw_data, alpha)
        }
        _ => unimplemented!("data/pixel ratio {}", data_ratio),
    };

    Ok(ImageData { data, width: image.width as u32, height: image.height as u32 })
}
fn cmyk2rgb([c, m, y, k]: [u8; 4]) -> [u8; 3] {
    let (c, m, y, k) = (255 - c, 255 - m, 255 - y, 255 - k);
    let r = 255 - c.saturating_add(k);
    let g = 255 - m.saturating_add(k);
    let b = 255 - y.saturating_add(k);
    [r, g, b]
}
fn cmyk2color([c, m, y, k]: [u8; 4], a: u8) -> ColorU {
    let (c, m, y, k) = (255 - c, 255 - m, 255 - y, 255 - k);
    let r = 255 - c.saturating_add(k);
    let g = 255 - m.saturating_add(k);
    let b = 255 - y.saturating_add(k);
    ColorU::new(r, g, b, a)
}

fn cmyk2color_arr(data: &[u8], alpha: impl Iterator<Item=u8>) -> Vec<ColorU> {
    data.chunks_exact(4).zip(alpha).map(|(c, a)| {
        let mut buf = [0; 4];
        buf.copy_from_slice(c);
        cmyk2color(buf, a)
    }).collect()
}

