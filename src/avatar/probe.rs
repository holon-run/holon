//! Byte-parity adapter and controlled PNG exporter for visual acceptance.
use std::io::{self, BufWriter, Read, Write};

use anyhow::{ensure, Result};
use holon::avatar::{recipe_index, render_png, render_svg, VERSION};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        ensure!(
            args.len() == 4 && args[0] == "--png",
            "usage: avatar_probe --png <id> <expression> <size>"
        );
        let png = render_png(recipe_index(&args[1]), args[2].parse()?, args[3].parse()?)?;
        io::stdout().lock().write_all(&png)?;
        return Ok(());
    }
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let ids: Vec<String> = serde_json::from_str(&input)?;
    let mut output = BufWriter::new(io::stdout().lock());
    for id in ids {
        let index = recipe_index(&id);
        for expression in ["neutral", "focused", "questioning", "waiting", "sleeping"] {
            serde_json::to_writer(
                &mut output,
                &serde_json::json!({
                    "id": id,
                    "index": index,
                    "version": VERSION,
                    "expression": expression,
                    "svg": render_svg(index, expression.parse()?),
                }),
            )?;
            writeln!(output)?;
        }
    }
    Ok(())
}
