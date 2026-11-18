use super::*;

#[test]
fn typed_json_cell_counts_are_stable_across_chunks() -> Result<()> {
    use serde_json::{json, Value};
    use std::io::Write;
    fn cells(value: &Value) -> usize {
        1 + match value {
            Value::Object(fields) => fields.values().map(cells).sum::<usize>(),
            Value::Array(values) => values.iter().map(cells).sum::<usize>(),
            _ => 0,
        }
    }
    for value in [
        json!(null),
        json!(false),
        json!(-1.25e-100),
        json!("汉🦀"),
        json!({"": [], "nested": [null, true, 42, {"key\"": "\"{,:}"}]}),
        json!({"metadata": {"nested": [[], {}, "", false, -2.0e20]}}),
    ] {
        let payload = serde_json::to_string_pretty(&value)?;
        let mut whole = StructuredMaterialCounter::default();
        whole.write_all(payload.as_bytes())?;
        assert_eq!(whole.cells, cells(&value));
        assert_eq!(whole.cells_peak, whole.cells);
        for size in 1..=17 {
            let mut chunks = StructuredMaterialCounter::default();
            for bytes in payload.as_bytes().chunks(size) {
                chunks.write_all(bytes)?;
            }
            assert_eq!(
                (chunks.bytes, chunks.slots, chunks.cells, chunks.cells_peak),
                (whole.bytes, whole.slots, whole.cells, whole.cells_peak),
            );
        }
    }
    Ok(())
}

#[test]
fn completion_json_shape_counts_across_writer_chunks_not_body_punctuation() -> Result<()> {
    use serde_json::json;
    use std::io::Write;
    let value = json!({"body":"\\\"[{:}],".repeat(100), "nested":[{}, [], true]});
    let text = serde_json::to_string(&value)?;
    let mut whole = StructuredMaterialCounter::default();
    whole.write_all(text.as_bytes())?;
    let mut chunks = StructuredMaterialCounter::default();
    for byte in text.as_bytes() {
        chunks.write_all(&[*byte])?;
    }
    assert_eq!((whole.bytes, whole.slots), (chunks.bytes, chunks.slots));
    assert!(!chunks.quoted && !chunks.escaped);
    let mut short = StructuredMaterialCounter::default();
    serde_json::to_writer(&mut short, &json!({"body":"", "nested":[{}, [], true]}))?;
    assert_eq!(short.slots, whole.slots);
    Ok(())
}
