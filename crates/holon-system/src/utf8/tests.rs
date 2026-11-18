use proptest::prelude::*;

use super::*;

fn decode_chunks(bytes: &[u8], split_points: &[usize]) -> String {
    let mut decoder = IncrementalUtf8LossyDecoder::new();
    let mut output = String::new();
    let mut start = 0;
    for &end in split_points {
        output.push_str(&decoder.push(&bytes[start..end]));
        start = end;
    }
    output.push_str(&decoder.push(&bytes[start..]));
    output.push_str(&decoder.finish());
    output
}

#[test]
fn decoder_preserves_multibyte_characters_across_every_boundary() {
    let text = "ASCII 中文 😀 終";
    let bytes = text.as_bytes();
    for split in 0..=bytes.len() {
        assert_eq!(decode_chunks(bytes, &[split]), text, "split at {split}");
    }
}

#[test]
fn decoder_flushes_incomplete_utf8_with_replacement() {
    assert_eq!(decode_chunks(&[b'a', 0xF0, 0x9F], &[2]), "a�");
}

#[test]
fn decoder_does_not_strip_utf8_bom() {
    assert_eq!(decode_chunks(b"\xEF\xBB\xBFtext", &[1, 2]), "\u{FEFF}text");
}

proptest! {
    #[test]
    fn decoder_matches_whole_stream_lossy_decode(
        bytes in proptest::collection::vec(any::<u8>(), 0..256),
        cuts in proptest::collection::vec(0usize..256, 0..32),
    ) {
        let mut split_points = cuts
            .into_iter()
            .map(|cut| cut.min(bytes.len()))
            .collect::<Vec<_>>();
        split_points.sort_unstable();
        split_points.dedup();
        prop_assert_eq!(
            decode_chunks(&bytes, &split_points),
            String::from_utf8_lossy(&bytes)
        );
    }
}
