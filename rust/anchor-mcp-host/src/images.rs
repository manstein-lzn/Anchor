use base64::{Engine, engine::general_purpose::STANDARD};
use image::{
    ImageDecoder, ImageFormat, ImageReader, Limits,
    codecs::{png::PngDecoder, webp::WebPDecoder},
};
use std::io::Cursor;

use crate::McpHostError;

const MIB: usize = 1024 * 1024;
const MAX_IMAGES: usize = 8;
const MAX_IMAGE_BYTES: usize = 10 * MIB;
const MAX_IMAGE_TOTAL_BYTES: usize = 20 * MIB;
const MAX_PIXELS: u64 = 20_000_000;
const MAX_DECODE_BYTES: u64 = 128 * MIB as u64;

#[derive(Default)]
pub struct ImageBudget {
    count: usize,
    bytes: usize,
}

impl ImageBudget {
    pub fn validate(&mut self, data: &str, mime_type: &str) -> Result<Vec<u8>, McpHostError> {
        if !matches!(mime_type, "image/png" | "image/jpeg" | "image/webp") {
            return Err(McpHostError::Encode(
                "only static PNG, JPEG and WebP tool images are supported".into(),
            ));
        }
        if self.count >= MAX_IMAGES || data.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
            return Err(McpHostError::Encode(
                "MCP image result exceeds its size/count limit".into(),
            ));
        }
        let bytes = STANDARD
            .decode(data)
            .map_err(|_| McpHostError::Encode("MCP image data is not canonical base64".into()))?;
        if bytes.is_empty()
            || bytes.len() > MAX_IMAGE_BYTES
            || bytes.len() > MAX_IMAGE_TOTAL_BYTES - self.bytes
        {
            return Err(McpHostError::Encode(
                "MCP image result exceeds its size/count limit".into(),
            ));
        }
        validate_image_bytes(&bytes, mime_type).map_err(McpHostError::Encode)?;
        self.count += 1;
        self.bytes += bytes.len();
        Ok(bytes)
    }
}

pub fn validate_image_bytes(bytes: &[u8], media_type: &str) -> Result<(), String> {
    let format = image::guess_format(bytes).map_err(|_| "invalid image attachment".to_owned())?;
    let mime_type = match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::WebP => "image/webp",
        _ => return Err("only static PNG, JPEG and WebP images are supported".into()),
    };
    if mime_type != media_type {
        return Err("image attachment MIME differs from its bytes".into());
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_PIXELS as u32);
    limits.max_image_height = Some(MAX_PIXELS as u32);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    let decoder: Box<dyn ImageDecoder + '_> = match format {
        ImageFormat::Png => {
            let decoder = PngDecoder::with_limits(Cursor::new(bytes), limits.clone())
                .map_err(|_| "image attachment cannot be decoded within its limits".to_owned())?;
            if decoder
                .is_apng()
                .map_err(|_| "invalid PNG attachment".to_owned())?
            {
                return Err("animated image attachments are not supported".into());
            }
            Box::new(decoder)
        }
        ImageFormat::WebP => {
            let mut decoder = WebPDecoder::new(Cursor::new(bytes))
                .map_err(|_| "image attachment cannot be decoded within its limits".to_owned())?;
            decoder
                .set_limits(limits.clone())
                .map_err(|_| "image attachment cannot be decoded within its limits".to_owned())?;
            if decoder.has_animation() {
                return Err("animated image attachments are not supported".into());
            }
            Box::new(decoder)
        }
        _ => {
            let mut header = ImageReader::with_format(Cursor::new(bytes), format);
            header.limits(limits.clone());
            Box::new(
                header.into_decoder().map_err(|_| {
                    "image attachment cannot be decoded within its limits".to_owned()
                })?,
            )
        }
    };
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err("image attachment exceeds the 20 million pixel limit".into());
    }
    drop(decoder);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|_| "image attachment cannot be fully decoded within its limits".to_owned())?;
    Ok(())
}

#[cfg(test)]
mod tests;
