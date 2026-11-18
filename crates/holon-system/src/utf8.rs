use encoding_rs::{CoderResult, Decoder, UTF_8};

pub struct IncrementalUtf8LossyDecoder {
    decoder: Decoder,
    finished: bool,
}

impl IncrementalUtf8LossyDecoder {
    pub fn new() -> Self {
        Self {
            decoder: UTF_8.new_decoder_without_bom_handling(),
            finished: false,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> String {
        debug_assert!(!self.finished, "cannot decode bytes after stream finish");
        if self.finished || bytes.is_empty() {
            return String::new();
        }
        self.decode(bytes, false)
    }

    pub fn finish(&mut self) -> String {
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

/// Truncate text to a maximum character count.
pub fn truncate_text(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}...", text.chars().take(max).collect::<String>())
    }
}

#[cfg(test)]
mod tests;
