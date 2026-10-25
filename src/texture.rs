use crate::value::Fields;
use anyhow::{Result, bail, ensure};
use image::{RgbaImage, imageops};
use rmpv::Value;

pub fn decode(raw: &Value) -> Result<RgbaImage> {
    let width = u32::try_from(raw["m_Width"].integer())?;
    let height = u32::try_from(raw["m_Height"].integer())?;
    ensure!(width > 0 && height > 0, "Empty source texture");

    let count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| anyhow::anyhow!("Texture dimensions overflow"))?;
    let format = raw["m_TextureFormat"].integer();
    let data = raw["image data"].binary();
    let mut rgba = vec![
        0;
        count
            .checked_mul(4)
            .ok_or_else(|| anyhow::anyhow!("Texture size overflow"))?
    ];

    if [10, 11, 12, 25].contains(&format) {
        let block_size = if format == 10 { 8 } else { 16 };
        let columns = (width as usize).div_ceil(4);
        let rows = (height as usize).div_ceil(4);
        ensure!(
            data.len() >= columns * rows * block_size,
            "Truncated BC texture"
        );
        for by in 0..rows {
            for bx in 0..columns {
                let start = (by * columns + bx) * block_size;
                let source = &data[start..start + block_size];
                let mut pixels = [0u32; 16];

                match format {
                    10 => texture2ddecoder::decode_bc1a_block(source, &mut pixels),
                    11 | 12 => {
                        let mut block = [0u8; 16];
                        block.copy_from_slice(source);
                        four_color_block(&mut block[8..]);
                        if format == 11 {
                            texture2ddecoder::decode_bc2_block(&block, &mut pixels);
                        } else {
                            texture2ddecoder::decode_bc3_block(&block, &mut pixels);
                        }
                    }
                    25 => texture2ddecoder::decode_bc7_block(source, &mut pixels),
                    _ => unreachable!(),
                }

                for y in 0..4 {
                    for x in 0..4 {
                        let px = bx * 4 + x;
                        let py = by * 4 + y;
                        if px >= width as usize || py >= height as usize {
                            continue;
                        }

                        let [b, g, r, a] = pixels[y * 4 + x].to_le_bytes();
                        let target = (py * width as usize + px) * 4;
                        rgba[target..target + 4].copy_from_slice(&[r, g, b, a]);
                    }
                }
            }
        }
    } else {
        let stride = match format {
            1 => 1,
            2 | 7 | 13 => 2,
            3 => 3,
            4 | 5 | 14 => 4,
            _ => bail!("Unsupported Unity texture format {format}"),
        };
        ensure!(data.len() >= count * stride, "Truncated raw texture");
        for (source, target) in data[..count * stride]
            .chunks_exact(stride)
            .zip(rgba.as_chunks_mut::<4>().0)
        {
            let pixel = match format {
                1 => [0, 0, 0, source[0]],
                2 | 13 => [
                    source[0] & 15,
                    source[0] >> 4,
                    source[1] & 15,
                    source[1] >> 4,
                ]
                .map(|n| n * 17),
                3 => [source[0], source[1], source[2], 255],
                4 => source.try_into().unwrap(),
                5 => [source[1], source[2], source[3], source[0]],
                14 => [source[2], source[1], source[0], source[3]],
                7 => {
                    let n = u16::from_le_bytes(source.try_into().unwrap());
                    [
                        ((n & 31) * 255 / 31) as u8,
                        (((n >> 5) & 63) * 255 / 63) as u8,
                        (((n >> 11) & 31) * 255 / 31) as u8,
                        255,
                    ]
                }
                _ => unreachable!(),
            };
            target.copy_from_slice(&pixel);
        }
    }

    let mut image = RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| anyhow::anyhow!("Invalid image dimensions"))?;
    imageops::flip_vertical_in_place(&mut image);
    Ok(image)
}
// BC2/3 always use four colours. Normalize endpoint order for the decoder's BC1 helper.
fn four_color_block(block: &mut [u8]) {
    let first = u16::from_le_bytes(block[..2].try_into().unwrap());
    let second = u16::from_le_bytes(block[2..4].try_into().unwrap());

    if first < second {
        block[..2].copy_from_slice(&second.to_le_bytes());
        block[2..4].copy_from_slice(&first.to_le_bytes());
        for code in &mut block[4..] {
            *code ^= 0x55;
        }
    } else if first == second {
        for code in &mut block[4..] {
            let mut output = 0;
            for i in 0..4 {
                let n = (*code >> (i * 2)) & 3;
                output |= if n == 3 { 0 } else { n } << (i * 2);
            }
            *code = output;
        }
    }
}

pub fn encode(image: &RgbaImage) -> Result<Vec<u8>> {
    let mut config =
        webp::WebPConfig::new().map_err(|_| anyhow::anyhow!("Cannot initialize WebP encoder"))?;
    config.lossless = 1;
    config.quality = 80.0;
    config.method = 1;
    config.exact = 1;

    let encoded = webp::Encoder::from_rgba(image.as_raw(), image.width(), image.height())
        .encode_advanced(&config)
        .map_err(|e| anyhow::anyhow!("WebP encoding failed: {e:?}"))?;
    Ok(encoded.to_vec())
}
