use std::io::Write;

/// Language of the labels in the output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum Lang {
    Ru,
    En,
}

/// When to colour the text output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum Color {
    Auto,
    Always,
    Never,
}

impl Color {
    pub fn enabled(self, tty: bool) -> bool {
        match self {
            Color::Always => true,
            Color::Never => false,
            Color::Auto => tty && std::env::var_os("NO_COLOR").is_none(),
        }
    }
}

pub fn write_json(out: &mut dyn Write, v: &serde_json::Value) -> crate::Result<()> {
    serde_json::to_writer_pretty(&mut *out, v)?;
    writeln!(out)?;
    Ok(())
}
