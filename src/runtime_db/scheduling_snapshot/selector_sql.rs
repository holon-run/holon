//! Count borrowed SQL fragments before allocating an owning selector.
use super::*;
use std::fmt::{self, Write};

pub(super) fn borrowed_fragment(
    format: impl Fn(&mut fmt::Formatter<'_>) -> fmt::Result,
) -> impl fmt::Display {
    struct Fragment<F>(F);
    impl<F: Fn(&mut fmt::Formatter<'_>) -> fmt::Result> fmt::Display for Fragment<F> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            (self.0)(f)
        }
    }
    Fragment(format)
}

pub(super) fn format_selector(
    material: &mut Material<'_, '_>,
    arguments: fmt::Arguments<'_>,
) -> Result<String> {
    #[derive(Default)]
    struct Length(usize);
    impl Write for Length {
        fn write_str(&mut self, text: &str) -> fmt::Result {
            self.0 = self.0.checked_add(text.len()).ok_or(fmt::Error)?;
            Ok(())
        }
    }
    let mut length = Length::default();
    fmt::write(&mut length, arguments).context("Scheduling selector length overflow")?;
    material.charge(
        length
            .0
            .checked_add(std::mem::size_of::<String>())
            .context("Scheduling selector capacity overflow")?,
        1,
    )?;
    let mut sql = String::with_capacity(length.0);
    fmt::write(&mut sql, arguments).context("Scheduling selector formatting failed")?;
    Ok(sql)
}

struct Literal<'a>(&'a str);
impl fmt::Display for Literal<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("'")?;
        for (index, part) in self.0.split('\'').enumerate() {
            if index > 0 {
                f.write_str("''")?;
            }
            f.write_str(part)?;
        }
        f.write_str("'")
    }
}

pub(super) struct Inventories<'a>(pub &'a [SchedulingContinuityCheckpoint]);
impl fmt::Display for Inventories<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("NULL");
        }
        for (index, manifest) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(",")?;
            }
            write!(f, "{}", Literal(&manifest.inventory_id))?;
        }
        Ok(())
    }
}

pub(super) struct Ranks<'a>(pub &'a [SchedulingContinuityCheckpoint]);
impl fmt::Display for Ranks<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("WHEN '' THEN -1");
        }
        for (index, manifest) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(" ")?;
            }
            write!(f, "WHEN {} THEN {index}", Literal(&manifest.inventory_id))?;
        }
        Ok(())
    }
}

pub(super) struct Limit(pub Option<usize>);
impl fmt::Display for Limit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(limit) => write!(f, " LIMIT {limit}"),
            None => Ok(()),
        }
    }
}
