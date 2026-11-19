use super::{
    anthropic_image_tokens, conservative_image_reserve_tokens, estimate_image_tokens,
    inline_image_dimensions, openai_image_tokens,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use image::{DynamicImage, ImageFormat};
use std::io::Cursor;

#[test]
fn small_images_use_dimensions_instead_of_a_worst_case_reserve() {
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::new_rgb8(200, 200)
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    let encoded = STANDARD.encode(bytes.into_inner());
    assert_eq!(inline_image_dimensions(&encoded), Some((200, 200)));
    assert_eq!(estimate_image_tokens(&encoded), 64);
    assert_eq!(
        anthropic_image_tokens("claude-sonnet-4-6", Some((200, 200))),
        64
    );
    assert_eq!(openai_image_tokens("gpt-4o", None, Some((200, 200))), 255);
    assert_eq!(openai_image_tokens("gpt-4o-mini", Some("low"), None), 2_833);
    assert_eq!(
        openai_image_tokens("gpt-4.1-mini", None, Some((200, 200))),
        80
    );
}

#[test]
fn known_model_rules_preserve_detail_and_resolution_tiers() {
    assert_eq!(
        openai_image_tokens("gpt-4o-2024-08-06", Some("low"), None),
        85
    );
    assert_eq!(
        openai_image_tokens("gpt-4o", Some("high"), Some((1024, 1024))),
        765
    );
    assert_eq!(
        openai_image_tokens("gpt-4o-mini", None, Some((1024, 1024))),
        25_501
    );
    assert_eq!(
        openai_image_tokens("gpt-4.1-mini", None, Some((1024, 1024))),
        1_659
    );
    assert_eq!(
        openai_image_tokens("gpt-5.4", Some("high"), Some((4096, 4096))),
        3_000
    );
    assert_eq!(
        anthropic_image_tokens("claude-sonnet-4-6", Some((4096, 4096))),
        1_568
    );
    assert_eq!(
        anthropic_image_tokens("claude-opus-4-7", Some((4096, 4096))),
        4_784
    );
}

#[test]
fn unknown_and_unreadable_images_keep_a_bounded_heuristic() {
    assert_eq!(inline_image_dimensions(&"A".repeat(4_000_000)), None);
    assert_eq!(inline_image_dimensions("not base64"), None);
    assert_eq!(
        estimate_image_tokens("not base64"),
        conservative_image_reserve_tokens()
    );
    assert_eq!(openai_image_tokens("compatible-alias", None, None), 2_048);
    assert_eq!(
        openai_image_tokens("gpt-4o-custom", None, Some((200, 200))),
        64
    );
}

#[test]
fn header_probe_supports_delivery_formats_and_ignores_payload_tail() {
    for format in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::WebP] {
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::new_rgb8(200, 100)
            .write_to(&mut bytes, format)
            .unwrap();
        let mut bytes = bytes.into_inner();
        assert_eq!(
            inline_image_dimensions(&STANDARD.encode(&bytes)),
            Some((200, 100)),
            "{format:?}"
        );
        bytes.resize(2_000_000, 0);
        let encoded = STANDARD.encode(bytes);
        assert_eq!(inline_image_dimensions(&encoded), Some((200, 100)));
        assert_eq!(estimate_image_tokens(&encoded), 32);
    }
}

#[test]
fn header_probe_handles_truncation_and_the_prefix_boundary() {
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::new_rgb8(200, 100)
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    let mut bytes = bytes.into_inner();
    let encoded = STANDARD.encode(&bytes);
    for len in [0, 1, 2, 3, 4, 12, 24] {
        assert_eq!(inline_image_dimensions(&encoded[..len]), None, "{len}");
    }

    // The bounded base64 prefix decodes to the largest aligned length
    // below 64 KiB; invalid data after it must never be examined.
    bytes.resize(64 * 1024 / 3 * 3, 0);
    let mut encoded = STANDARD.encode(bytes);
    assert_eq!(encoded.len(), 64 * 1024 / 3 * 4);
    encoded.push('!');
    assert_eq!(inline_image_dimensions(&encoded), Some((200, 100)));

    // A scalar straddling the byte boundary is malformed base64, not a
    // string-slicing panic.
    encoded.truncate(encoded.len() - 2);
    encoded.push('\u{1f5bc}');
    assert_eq!(inline_image_dimensions(&encoded), None);
    assert_eq!(
        estimate_image_tokens(&encoded),
        conservative_image_reserve_tokens()
    );
}
