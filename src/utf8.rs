use encoding_rs::{CoderResult, Decoder, UTF_8};

pub(crate) struct IncrementalUtf8LossyDecoder {
    decoder: Decoder,
    finished: bool,
}

impl IncrementalUtf8LossyDecoder {
    pub(crate) fn new() -> Self {
        Self {
            decoder: UTF_8.new_decoder_without_bom_handling(),
            finished: false,
        }
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) -> String {
        debug_assert!(!self.finished, "cannot decode bytes after stream finish");
        if self.finished || bytes.is_empty() {
            return String::new();
        }
        self.decode(bytes, false)
    }

    pub(crate) fn finish(&mut self) -> String {
        if self.finished {
            return String::new();
        }
        self.finished = true;
        self.decode(&[], true)
    }

    fn decode(&mut self, mut bytes: &[u8], last: bool) -> String {
        let initial_capacity = self
            .decoder
            .max_utf8_buffer_length(bytes.len())
            .unwrap_or_else(|| bytes.len().saturating_mul(3).saturating_add(3))
            .max(3);
        let mut output = String::with_capacity(initial_capacity);

        loop {
            let (result, read, _) = self.decoder.decode_to_string(bytes, &mut output, last);
            bytes = &bytes[read..];
            match result {
                CoderResult::InputEmpty => {
                    debug_assert!(bytes.is_empty());
                    return output;
                }
                CoderResult::OutputFull => {
                    output.reserve(bytes.len().saturating_mul(3).saturating_add(3));
                }
            }
        }
    }
}

impl Default for IncrementalUtf8LossyDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for IncrementalUtf8LossyDecoder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IncrementalUtf8LossyDecoder")
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
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
}
