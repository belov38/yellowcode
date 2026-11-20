use super::fail;
use crate::Result;

pub(super) fn check_depth(text: &[u8], entry: &str, check: &dyn Fn() -> Result<()>) -> Result<()> {
    let mut depth = 0usize;
    let mut quoted = false;
    let mut bytes = text.iter().copied().enumerate().peekable();
    while let Some((index, byte)) = bytes.next() {
        if index.is_multiple_of(4096) {
            check()?;
        }
        if quoted {
            if byte == b'"' {
                if bytes.peek().is_some_and(|(_, next)| *next == b'"') {
                    bytes.next();
                } else {
                    quoted = false;
                }
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'{' => {
                if depth == 128 {
                    return Err(fail(entry, "brace nesting exceeds reader depth budget 128"));
                }
                depth += 1;
            }
            b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    check()
}
