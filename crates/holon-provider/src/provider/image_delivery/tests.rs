use super::*;

fn png(alpha: u8) -> Vec<u8> {
    let image = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        2,
        3,
        image::Rgba([1, 2, 3, alpha]),
    ));
    let mut output = Cursor::new(Vec::new());
    image.write_to(&mut output, ImageFormat::Png).unwrap();
    output.into_inner()
}

#[test]
fn image_delivery_validates_and_preserves_original_encoding() {
    let bytes = png(255);
    let output = deliver_image(&bytes, "png").unwrap();
    assert_eq!(output.bytes, bytes);
    assert!(!output.conversion.converted);
    assert_eq!((output.conversion.width, output.conversion.height), (2, 3));
}

#[test]
fn image_delivery_alpha_and_codecs() {
    let transparent = png(100);
    assert_eq!(
        deliver_image(&transparent, "jpeg").unwrap_err().code,
        "image_transparency_not_preserved"
    );
    let webp = deliver_image(&transparent, "webp").unwrap();
    assert_eq!(source_format(&webp.bytes), Some("webp"));
    let back = deliver_image(&webp.bytes, "png").unwrap();
    assert_eq!(
        image::load_from_memory(&back.bytes)
            .unwrap()
            .to_rgba8()
            .get_pixel(0, 0)[3],
        100
    );
    let jpeg = deliver_image(&png(255), "jpeg").unwrap();
    assert_eq!(source_format(&jpeg.bytes), Some("jpeg"));
    assert_eq!(jpeg.conversion.encoding_policy, "jpeg_quality_90_lossy");
}

#[test]
fn image_delivery_exif_orientation_survives_transcoding() {
    let jpeg = deliver_image(&png(255), "jpeg").unwrap().bytes;
    // EXIF IFD0 Orientation=6 (90 degrees clockwise).
    let exif: &[u8] = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
    let mut oriented = jpeg[..2].to_vec();
    oriented.extend_from_slice(&[0xff, 0xe1, 0, 34]);
    oriented.extend_from_slice(exif);
    oriented.extend_from_slice(&jpeg[2..]);
    let unchanged = deliver_image(&oriented, "jpeg").unwrap();
    assert_eq!(unchanged.bytes, oriented);
    assert_eq!(
        (unchanged.conversion.width, unchanged.conversion.height),
        (3, 2)
    );
    let png = deliver_image(&oriented, "png").unwrap();
    assert!(png.conversion.orientation_applied);
    assert_eq!((png.conversion.width, png.conversion.height), (3, 2));
    let actual = image::load_from_memory(&png.bytes).unwrap();
    assert_eq!((actual.width(), actual.height()), (3, 2));
}

#[test]
fn image_delivery_rejects_animation_without_substring_false_positives() {
    let mut animated_png = b"\x89PNG\r\n\x1a\n".to_vec();
    animated_png.extend_from_slice(&[0, 0, 0, 8]);
    animated_png.extend_from_slice(b"acTL");
    animated_png.extend_from_slice(&[0; 12]);
    assert_eq!(
        deliver_image(&animated_png, "png").unwrap_err().code,
        "unsupported_source_image_format"
    );
    let mut animated_webp = b"RIFF\x10\0\0\0WEBP".to_vec();
    animated_webp.extend_from_slice(b"ANIM\x06\0\0\0");
    animated_webp.extend_from_slice(&[0; 6]);
    assert_eq!(
        deliver_image(&animated_webp, "png").unwrap_err().code,
        "unsupported_source_image_format"
    );
    assert!(!animated_image(&png(255), "png").unwrap());
}

#[test]
fn image_delivery_preserves_sixteen_bit_png_and_rejects_lossy_depth_reduction() {
    let image = DynamicImage::ImageRgba16(image::ImageBuffer::from_pixel(
        2,
        3,
        image::Rgba([1000u16, 2000, 3000, u16::MAX]),
    ));
    let mut encoded = Cursor::new(Vec::new());
    image.write_to(&mut encoded, ImageFormat::Png).unwrap();
    let bytes = encoded.into_inner();
    assert_eq!(deliver_image(&bytes, "png").unwrap().bytes, bytes);
    assert_eq!(
        deliver_image(&bytes, "webp").unwrap_err().code,
        "image_conversion_failed"
    );
}

#[test]
fn image_delivery_corruption_and_limits() {
    assert_eq!(
        deliver_image(b"not an image", "png").unwrap_err().code,
        "unsupported_source_image_format"
    );
    assert!(deliver_image(b"\x89PNG\r\n\x1a\nbroken", "png").is_err());
    let bytes = vec![0; MAX_IMAGE_BYTES + 1];
    assert_eq!(
        deliver_image(&bytes, "png").unwrap_err().code,
        "image_resource_limit"
    );
    let mut output = BoundedOutput(Cursor::new(Vec::new()));
    output
        .seek(SeekFrom::Start(MAX_IMAGE_BYTES as u64))
        .unwrap();
    assert!(output.write_all(&[1]).is_err());
}
