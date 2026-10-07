use super::*;

fn encoded(format: ImageFormat) -> (Vec<u8>, String) {
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut bytes, format)
        .unwrap();
    let bytes = bytes.into_inner();
    let encoded = STANDARD.encode(&bytes);
    (bytes, encoded)
}

#[test]
fn static_images_validate_without_reencoding_the_original_bytes() {
    for (format, mime_type) in [
        (ImageFormat::Png, "image/png"),
        (ImageFormat::Jpeg, "image/jpeg"),
        (ImageFormat::WebP, "image/webp"),
    ] {
        let (bytes, data) = encoded(format);
        assert_eq!(
            ImageBudget::default().validate(&data, mime_type).unwrap(),
            bytes
        );
    }
}

#[test]
fn malformed_mime_base64_and_truncated_images_are_rejected() {
    let (bytes, data) = encoded(ImageFormat::Png);
    for (data, mime_type) in [
        (data.as_str(), "image/jpeg"),
        (data.as_str(), "image/unknown"),
        (data.as_str(), "image/svg+xml"),
        ("https://example.com/image.png", "image/png"),
        ("a===", "image/png"),
        ("", "image/png"),
        ("AAEC", "image/png"),
    ] {
        assert!(ImageBudget::default().validate(data, mime_type).is_err());
    }
    assert!(
        ImageBudget::default()
            .validate(&STANDARD.encode(&bytes[..bytes.len() / 2]), "image/png")
            .is_err()
    );
}

#[test]
fn count_and_aggregate_limits_are_enforced_before_accepting_another_image() {
    let (bytes, data) = encoded(ImageFormat::Png);
    let mut budget = ImageBudget::default();
    for _ in 0..MAX_IMAGES {
        budget.validate(&data, "image/png").unwrap();
    }
    assert!(budget.validate(&data, "image/png").is_err());
    let mut budget = ImageBudget {
        count: 1,
        bytes: MAX_IMAGE_TOTAL_BYTES - bytes.len() + 1,
    };
    assert!(budget.validate(&data, "image/png").is_err());
    assert_eq!(budget.count, 1);
    assert!(
        ImageBudget::default()
            .validate(
                &"A".repeat(MAX_IMAGE_BYTES.div_ceil(3) * 4 + 1),
                "image/png"
            )
            .is_err()
    );
}

#[test]
fn pixel_limits_are_checked_before_decoding_the_full_raster() {
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::new_luma8(4500, 4500)
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    assert!(
        validate_image_bytes(bytes.get_ref(), "image/png")
            .unwrap_err()
            .contains("20 million pixel")
    );
}
