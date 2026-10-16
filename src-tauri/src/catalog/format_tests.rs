//! Deterministic encoded fixtures exercise the real isolated worker. Expected
//! orientation coordinates are independent of image::apply_orientation.
use super::*;
use image::{ExtendedColorType, ImageEncoder, Rgb, RgbImage};

fn exif(orientation: u8) -> Vec<u8> {
    vec![
        b'I',
        b'I',
        42,
        0,
        8,
        0,
        0,
        0,
        1,
        0,
        0x12,
        0x01,
        3,
        0,
        1,
        0,
        0,
        0,
        orientation,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]
}

// Minimal uncompressed RGB8 TIFF writer for independent orientation/page fixtures.
// No production decoder/parser code is used to construct the expected pixels.
fn tiff(pages: &[(&RgbImage, u8)], icc: Option<&[u8]>) -> Vec<u8> {
    let mut bytes = vec![b'I', b'I', 42, 0, 8, 0, 0, 0];
    let mut previous_next = None;
    for (pixels, orientation) in pages {
        let start = bytes.len();
        if let Some(next) = previous_next {
            bytes[next..next + 4].copy_from_slice(&(start as u32).to_le_bytes());
        }
        let count = 11 + usize::from(icc.is_some());
        let bits = start + 2 + count * 12 + 4;
        let data = bits + 6;
        let profile = data + pixels.as_raw().len();
        let mut tags = vec![
            (256u16, 4u16, 1u32, pixels.width()),
            (257, 4, 1, pixels.height()),
            (258, 3, 3, bits as u32),
            (259, 3, 1, 1),
            (262, 3, 1, 2),
            (273, 4, 1, data as u32),
            (274, 3, 1, u32::from(*orientation)),
            (277, 3, 1, 3),
            (278, 4, 1, pixels.height()),
            (279, 4, 1, pixels.as_raw().len() as u32),
            (284, 3, 1, 1),
        ];
        if let Some(icc) = icc {
            tags.push((34675, 7, icc.len() as u32, profile as u32));
        }
        bytes.extend((count as u16).to_le_bytes());
        for (tag, kind, count, value) in tags {
            bytes.extend(tag.to_le_bytes());
            bytes.extend(kind.to_le_bytes());
            bytes.extend(count.to_le_bytes());
            bytes.extend(value.to_le_bytes());
        }
        previous_next = Some(bytes.len());
        bytes.extend(0u32.to_le_bytes());
        bytes.extend([8, 0, 8, 0, 8, 0]);
        bytes.extend(pixels.as_raw());
        if let Some(icc) = icc {
            bytes.extend(icc);
        }
    }
    bytes
}

fn fingerprint_files(root: &Path) -> HashMap<PathBuf, String> {
    fs::read_dir(root)
        .unwrap()
        .map(|item| {
            let path = item.unwrap().path().canonicalize().unwrap();
            let hash = format!("{:x}", Sha256::digest(fs::read(&path).unwrap()));
            (path, hash)
        })
        .collect()
}

#[test]
fn embedded_srgb_and_all_orientations_work_for_png_jpeg_webp_and_tiff_without_source_changes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sources");
    fs::create_dir(&root).unwrap();
    let colors = [
        [180, 40, 30],
        [30, 170, 50],
        [30, 50, 180],
        [170, 160, 30],
        [30, 170, 160],
        [170, 30, 160],
    ];
    let pixels = RgbImage::from_fn(48, 32, |x, y| Rgb(colors[(y / 16 * 3 + x / 16) as usize]));
    let icc = moxcms::ColorProfile::new_srgb().encode().unwrap();
    for orientation in 1..=8 {
        let metadata = exif(orientation);
        let mut png = image::codecs::png::PngEncoder::new(
            File::create(root.join(format!("{orientation}.png"))).unwrap(),
        );
        png.set_icc_profile(icc.clone()).unwrap();
        png.set_exif_metadata(metadata.clone()).unwrap();
        png.write_image(pixels.as_raw(), 48, 32, ExtendedColorType::Rgb8)
            .unwrap();
        let mut jpeg = image::codecs::jpeg::JpegEncoder::new_with_quality(
            File::create(root.join(format!("{orientation}.jpg"))).unwrap(),
            100,
        );
        jpeg.set_icc_profile(icc.clone()).unwrap();
        jpeg.set_exif_metadata(metadata.clone()).unwrap();
        jpeg.write_image(pixels.as_raw(), 48, 32, ExtendedColorType::Rgb8)
            .unwrap();
        let mut webp = image::codecs::webp::WebPEncoder::new_lossless(
            File::create(root.join(format!("{orientation}.webp"))).unwrap(),
        );
        webp.set_icc_profile(icc.clone()).unwrap();
        webp.set_exif_metadata(metadata).unwrap();
        webp.write_image(pixels.as_raw(), 48, 32, ExtendedColorType::Rgb8)
            .unwrap();
        fs::write(
            root.join(format!("{orientation}.tiff")),
            tiff(&[(&pixels, orientation)], Some(&icc)),
        )
        .unwrap();
    }
    let before = fingerprint_files(&root);
    let mut catalog = Catalog::new(temp.path().join("cache"));
    let report = catalog.scan(vec![root.clone()], false).unwrap();
    assert_eq!(report.images.len(), 32, "{:?}", report.ignored);
    for record in report.images {
        let orientation: u8 = record.original_name[..1].parse().unwrap();
        let expected_size = if orientation >= 5 { (32, 48) } else { (48, 32) };
        assert_eq!(
            (record.width, record.height),
            expected_size,
            "{}",
            record.original_name
        );
        let preview = image::open(record.preview_path).unwrap().into_rgb8();
        for (index, color) in colors.iter().enumerate() {
            let x = (index % 3) as u32 * 16 + 8;
            let y = (index / 3) as u32 * 16 + 8;
            let (dx, dy) = match orientation {
                1 => (x, y),
                2 => (47 - x, y),
                3 => (47 - x, 31 - y),
                4 => (x, 31 - y),
                5 => (y, x),
                6 => (31 - y, x),
                7 => (31 - y, 47 - x),
                8 => (y, 47 - x),
                _ => unreachable!(),
            };
            let actual = preview.get_pixel(dx, dy).0;
            let tolerance = if record.original_name.ends_with(".jpg") {
                3
            } else {
                1
            };
            assert!(
                actual
                    .iter()
                    .zip(color)
                    .all(|(a, b)| a.abs_diff(*b) <= tolerance),
                "{} cell {index}: {actual:?} expected {color:?}",
                record.original_name
            );
        }
        assert_eq!(record.content_revision, before[Path::new(&record.path)]);
    }
    assert_eq!(fingerprint_files(&root), before);
}

#[test]
fn gif_first_frame_tiff_first_page_bmp_and_gray_alpha_have_explicit_pixel_semantics() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sources");
    fs::create_dir(&root).unwrap();
    let first = RgbImage::from_pixel(12, 8, Rgb([20, 80, 160]));
    let second = RgbImage::from_pixel(12, 8, Rgb([220, 30, 10]));
    first.save(root.join("baseline.bmp")).unwrap();
    fs::write(
        root.join("pages.tif"),
        tiff(&[(&first, 1), (&second, 1)], None),
    )
    .unwrap();
    {
        let mut gif =
            image::codecs::gif::GifEncoder::new(File::create(root.join("frames.gif")).unwrap());
        for frame in [&first, &second] {
            gif.encode_frame(image::Frame::new(
                DynamicImage::ImageRgb8(frame.clone()).into_rgba8(),
            ))
            .unwrap();
        }
    }
    image::GrayAlphaImage::from_pixel(12, 8, image::LumaA([90, 123]))
        .save(root.join("gray-alpha.png"))
        .unwrap();
    let before = fingerprint_files(&root);
    let mut catalog = Catalog::new(temp.path().join("cache"));
    let report = catalog.scan(vec![root.clone()], false).unwrap();
    assert_eq!(report.images.len(), 4, "{:?}", report.ignored);
    for record in report.images {
        let preview = image::open(&record.preview_path).unwrap().into_rgba8();
        let expected = if record.original_name == "gray-alpha.png" {
            [90, 90, 90, 123]
        } else {
            [20, 80, 160, 255]
        };
        assert!(
            preview.pixels().all(|pixel| pixel.0 == expected),
            "{}",
            record.original_name
        );
    }
    assert_eq!(fingerprint_files(&root), before);
}

#[test]
fn unsupported_depth_invalid_icc_and_corrupt_encoded_files_are_explicit_and_immutable() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sources");
    fs::create_dir(&root).unwrap();
    image::ImageBuffer::from_pixel(4, 4, image::Luma([32768u16]))
        .save(root.join("depth16.png"))
        .unwrap();
    let pixels = RgbImage::from_pixel(4, 4, Rgb([30, 50, 70]));
    let mut png =
        image::codecs::png::PngEncoder::new(File::create(root.join("invalid-icc.png")).unwrap());
    png.set_icc_profile(vec![1, 2, 3, 4]).unwrap();
    png.write_image(pixels.as_raw(), 4, 4, ExtendedColorType::Rgb8)
        .unwrap();
    fs::write(root.join("broken.jpg"), [0xff, 0xd8, 0xff, 0xe0, 0]).unwrap();
    pixels.save(root.join("good.png")).unwrap();
    let before = fingerprint_files(&root);
    let mut catalog = Catalog::new(temp.path().join("cache"));
    let report = catalog.scan(vec![root.clone()], false).unwrap();
    assert_eq!(report.images.len(), 1, "{:?}", report.ignored);
    assert_eq!(report.ignored.len(), 3);
    assert!(report
        .ignored
        .iter()
        .any(|item| item.reason.contains("UNSUPPORTED_DEPTH")));
    assert!(report
        .ignored
        .iter()
        .any(|item| item.reason.contains("UNSUPPORTED_COLOR_PROFILE")));
    assert_eq!(fingerprint_files(&root), before);
}
