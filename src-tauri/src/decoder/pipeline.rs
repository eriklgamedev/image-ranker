//! Called exclusively by the bounded worker, never by a production parent.
use super::*;
use image::{
    metadata::Orientation, ColorType, DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits,
};
use std::io::{BufReader, BufWriter, Write};

pub(super) fn execute(
    mut source: File,
    output: &mut File,
    request: &Request,
    mut progress: impl FnMut(&str) -> Result<(), String>,
) -> Result<ImageInfo, String> {
    validate_request(request)?;
    stage(request, "hash", &mut progress)?;
    let metadata = source.metadata().map_err(|e| format!("UNREADABLE: {e}"))?;
    if !metadata.is_file() || metadata.len() > MAX_INPUT_BYTES {
        return Err("IMAGE_TOO_LARGE: encoded image exceeds 512 MiB".into());
    }
    let (digest, bytes) = hash_bounded(&mut source, MAX_INPUT_BYTES)?;
    if request
        .expected_sha256
        .as_ref()
        .is_some_and(|expected| expected != &digest)
    {
        return Err("CONTENT_CHANGED: source bytes no longer match this image revision".into());
    }
    let mut result = ImageInfo {
        source_sha256: digest,
        source_bytes: bytes,
        width: 0,
        height: 0,
        output_width: 0,
        output_height: 0,
        output_bytes: 0,
        output_sha256: String::new(),
        pipeline_version: PIPELINE_VERSION.into(),
    };
    let Some(edge) = request.edge else {
        return Ok(result);
    };
    stage(request, "header", &mut progress)?;
    let mut reader = ImageReader::new(BufReader::new(source))
        .with_guessed_format()
        .map_err(|e| format!("DECODE_FAILED: {e}"))?;
    if !matches!(
        reader.format(),
        Some(
            ImageFormat::Jpeg
                | ImageFormat::Png
                | ImageFormat::WebP
                | ImageFormat::Gif
                | ImageFormat::Bmp
                | ImageFormat::Tiff
        )
    ) {
        return Err("UNSUPPORTED_FORMAT: file signature is not a supported image".into());
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_EDGE);
    limits.max_image_height = Some(MAX_EDGE);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("DECODE_FAILED: {e}"))?;
    let (mut width, mut height) = decoder.dimensions();
    if !dimensions_valid(width, height) || decoder.total_bytes() > MAX_DECODE_BYTES {
        return Err(
            "IMAGE_TOO_LARGE: image exceeds the 40 megapixel or 192 MiB pixel budget".into(),
        );
    }
    if !matches!(
        decoder.color_type(),
        ColorType::L8 | ColorType::La8 | ColorType::Rgb8 | ColorType::Rgba8
    ) {
        return Err("UNSUPPORTED_DEPTH: only 8-bit channels are supported".into());
    }
    stage(request, "icc", &mut progress)?;
    let profile = decoder
        .icc_profile()
        .map_err(|e| format!("DECODE_FAILED: {e}"))?
        .filter(|bytes| !bytes.is_empty())
        .map(|bytes| {
            if bytes.len() > MAX_ICC_BYTES {
                return Err("UNSUPPORTED_COLOR_PROFILE: ICC profile exceeds 4 MiB".to_owned());
            }
            moxcms::ColorProfile::new_from_slice_with_options(
                &bytes,
                moxcms::ParsingOptions {
                    max_profile_size: MAX_ICC_BYTES + 1,
                    max_allowed_clut_size: MAX_ICC_BYTES,
                    max_allowed_trc_size: 40_000,
                },
            )
            .map_err(|e| format!("UNSUPPORTED_COLOR_PROFILE: {e}"))
        })
        .transpose()?;
    let orientation = decoder
        .orientation()
        .map_err(|e| format!("DECODE_FAILED: {e}"))?;
    if matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    ) {
        std::mem::swap(&mut width, &mut height);
    }
    stage(request, "decode", &mut progress)?;
    let mut image =
        DynamicImage::from_decoder(decoder).map_err(|e| format!("DECODE_FAILED: {e}"))?;
    if let Some(profile) = profile {
        image = normalize_srgb(image, &profile)?;
    }
    stage(request, "resize", &mut progress)?;
    image.apply_orientation(orientation);
    let image = if image.width() <= edge && image.height() <= edge {
        image
    } else {
        image.thumbnail(edge, edge)
    };
    result.width = width;
    result.height = height;
    result.output_width = image.width();
    result.output_height = image.height();
    stage(request, "encode", &mut progress)?;
    output
        .set_len(0)
        .map_err(|e| format!("CACHE_IO_ERROR: {e}"))?;
    output.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    {
        let mut writer = BufWriter::new(BoundedWriter {
            inner: &mut *output,
            position: 0,
        });
        image
            .write_to(&mut writer, ImageFormat::Png)
            .map_err(|e| format!("CACHE_IO_ERROR: {e}"))?;
        writer.flush().map_err(|e| format!("CACHE_IO_ERROR: {e}"))?;
    }
    output
        .sync_all()
        .map_err(|e| format!("CACHE_IO_ERROR: {e}"))?;
    (result.output_sha256, result.output_bytes) = hash_bounded(output, MAX_OUTPUT_BYTES)?;
    stage(request, "result", &mut progress)?;
    Ok(result)
}

struct BoundedWriter<'a> {
    inner: &'a mut File,
    position: u64,
}
impl Write for BoundedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > MAX_OUTPUT_BYTES.saturating_sub(self.position) {
            return Err(std::io::Error::other("DECODE_OUTPUT_LIMIT"));
        }
        let count = self.inner.write(bytes)?;
        self.position += count as u64;
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
impl Seek for BoundedWriter<'_> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        let target = match position {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::Current(n) => self.position as i128 + n as i128,
            SeekFrom::End(n) => self.inner.metadata()?.len() as i128 + n as i128,
        };
        if target < 0 || target > MAX_OUTPUT_BYTES as i128 {
            return Err(std::io::Error::other("DECODE_OUTPUT_LIMIT"));
        }
        self.position = self.inner.seek(SeekFrom::Start(target as u64))?;
        Ok(self.position)
    }
}

fn stage(
    request: &Request,
    stage: &str,
    progress: &mut impl FnMut(&str) -> Result<(), String>,
) -> Result<(), String> {
    progress(stage)?;
    #[cfg(any(test, feature = "e2e"))]
    {
        if let Some(hold) = request
            .fixture_hold
            .as_ref()
            .filter(|hold| hold.stage == stage)
        {
            // This explicit child acknowledgment is sent only after reaching
            // the requested real pipeline boundary. Parent timeout/cancel still
            // uses the ordinary job termination and exact-handle reap path.
            progress(&hold.status())?;
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        if request.fault.as_deref() == Some(stage) {
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        if request.fault.as_deref() == Some("memory") && stage == "hash" {
            let mut allocation = Vec::<u8>::new();
            return match allocation.try_reserve_exact(WORKER_MEMORY_BYTES as usize + 1) {
                Err(_) => Err("DECODE_MEMORY_LIMIT: oversized allocation was refused".into()),
                Ok(_) => Err("DECODE_LIMIT_NOT_ENFORCED: fixture unexpectedly allocated beyond the job limit".into()),
            };
        }
        if request.fault.as_deref() == Some("crash") && stage == "decode" {
            std::process::exit(71);
        }
    }
    let _ = (request, stage);
    Ok(())
}

pub(crate) fn normalize_srgb(
    image: DynamicImage,
    profile: &moxcms::ColorProfile,
) -> Result<DynamicImage, String> {
    use moxcms::{DataColorSpace, Layout};
    let (width, height) = (image.width(), image.height());
    let (mut data, layout, channels) = match image {
        DynamicImage::ImageRgb8(buffer) => (buffer.into_raw(), Layout::Rgb, 3usize),
        DynamicImage::ImageRgba8(buffer) => (buffer.into_raw(), Layout::Rgba, 4),
        DynamicImage::ImageLuma8(buffer) => (buffer.into_raw(), Layout::Gray, 1),
        DynamicImage::ImageLumaA8(buffer) => (buffer.into_raw(), Layout::GrayAlpha, 2),
        _ => return Err("UNSUPPORTED_DEPTH: ICC conversion requires 8-bit channels".into()),
    };
    if !matches!(
        (profile.color_space, channels),
        (DataColorSpace::Rgb, 3 | 4) | (DataColorSpace::Gray, 1 | 2)
    ) {
        return Err("UNSUPPORTED_COLOR_PROFILE: profile channels do not match pixels".into());
    }
    let output_channels = if channels == 2 || channels == 4 { 4 } else { 3 };
    let output_layout = if output_channels == 4 {
        Layout::Rgba
    } else {
        Layout::Rgb
    };
    let transform = profile
        .create_transform_8bit(
            layout,
            &moxcms::ColorProfile::new_srgb(),
            output_layout,
            moxcms::TransformOptions::default(),
        )
        .map_err(|e| format!("UNSUPPORTED_COLOR_PROFILE: {e}"))?;
    let source_stride = width as usize * channels;
    let output_stride = width as usize * output_channels;
    if channels == output_channels {
        let mut row = vec![0u8; output_stride];
        for pixels in data.chunks_exact_mut(source_stride) {
            transform
                .transform(pixels, &mut row)
                .map_err(|e| format!("COLOR_CONVERSION_FAILED: {e}"))?;
            pixels.copy_from_slice(&row);
        }
    } else {
        let mut converted = vec![0u8; output_stride * height as usize];
        for (source, target) in data
            .chunks_exact(source_stride)
            .zip(converted.chunks_exact_mut(output_stride))
        {
            transform
                .transform(source, target)
                .map_err(|e| format!("COLOR_CONVERSION_FAILED: {e}"))?;
        }
        data = converted;
    }
    if output_channels == 4 {
        Ok(DynamicImage::ImageRgba8(
            image::RgbaImage::from_raw(width, height, data).ok_or("Invalid normalized buffer")?,
        ))
    } else {
        Ok(DynamicImage::ImageRgb8(
            image::RgbImage::from_raw(width, height, data).ok_or("Invalid normalized buffer")?,
        ))
    }
}
