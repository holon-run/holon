use super::*;

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/agent-avatar-golden.json"
    ))
    .unwrap()
}

#[test]
fn js_reference_svg_bytes_and_unicode_hashes() {
    let fixture = fixture();
    assert_eq!(fixture["version"], VERSION);
    for case in fixture["cases"].as_array().unwrap() {
        let index = recipe_index(case["id"].as_str().unwrap());
        assert_eq!(index, case["index"].as_u64().unwrap() as usize);
        for (name, svg) in case["svg"].as_object().unwrap() {
            assert_eq!(
                render_svg(index, name.parse().unwrap()),
                svg.as_str().unwrap()
            );
        }
    }
}

#[test]
fn strict_expression_and_bounded_png_input() {
    assert_eq!(Expression::default(), Expression::Neutral);
    for value in ["", "Neutral", "unknown", "<script>"] {
        assert!(value.parse::<Expression>().is_err());
    }
    for size in [0, 513, u32::MAX] {
        assert!(render_png(0, Expression::Neutral, size).is_err());
    }
    assert_eq!(
        render_svg(usize::MAX, Expression::Neutral),
        render_svg(usize::MAX % TOTAL, Expression::Neutral)
    );
}

#[test]
fn png_pixels_match_js_svg_colors_alpha_and_contours_at_every_size() {
    // Independently rasterize the JS-produced fixture SVG (not render_svg), then
    // compare decoded RGBA. This detects palette, geometry, expression and scale
    // differences, not merely equal identity indices or a valid PNG header.
    for case in fixture()["cases"].as_array().unwrap() {
        for (name, svg) in case["svg"].as_object().unwrap() {
            let tree = resvg::usvg::Tree::from_str(
                svg.as_str().unwrap(),
                &resvg::usvg::Options::default(),
            )
            .unwrap();
            for size in [32, 64, 128, 256, 512] {
                let mut expected = resvg::tiny_skia::Pixmap::new(size, size).unwrap();
                resvg::render(
                    &tree,
                    resvg::tiny_skia::Transform::from_scale(
                        size as f32 / 100.0,
                        size as f32 / 100.0,
                    ),
                    &mut expected.as_mut(),
                );
                let png = render_png(
                    case["index"].as_u64().unwrap() as usize,
                    name.parse().unwrap(),
                    size,
                )
                .unwrap();
                assert!(png.len() < 2 * 1024 * 1024);
                let actual = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
                    .unwrap()
                    .into_rgba8();
                assert_eq!(actual.dimensions(), (size, size));
                assert_eq!(actual.as_raw(), expected.data(), "{name}, size={size}");
                // Current artwork deliberately has an opaque palette background;
                // transparency must not appear through scaling or PNG encoding.
                assert!(actual.pixels().all(|pixel| pixel[3] == 255));
                let corner = *actual.get_pixel(0, 0);
                assert_eq!(*actual.get_pixel(size - 1, size - 1), corner);
                let contour_pixels = actual.pixels().filter(|pixel| **pixel != corner).count();
                assert!(contour_pixels > (size * size / 4) as usize);
                assert!(contour_pixels < (size * size * 9 / 10) as usize);
                // At 32px, antialiasing can blend every pixel of a thin stroke.
                // Exact RGBA equality above remains the palette/contour oracle.
                assert!(actual
                    .pixels()
                    .any(|p| p[0] <= 0x24 + 32 && p[1] <= 0x34 + 32 && p[2] <= 0x42 + 32));
            }
        }
    }
}
