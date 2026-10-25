use crate::error::{LibError, msg};

fn wrong(pos: usize) -> LibError {
    LibError::new(msg::TEMPLATE_WRONG).with_suffix(pos.to_string())
}

fn digit(c: Option<&u16>) -> Option<u32> {
    match c {
        Some(&c @ 0x30..=0x39) => Some(u32::from(c - 0x30)),
        _ => None,
    }
}

pub fn template(fmt: &[u16], args: &[Vec<u16>]) -> Result<Vec<u16>, LibError> {
    let mut out = Vec::with_capacity(fmt.len());
    let mut max_used = 0usize;
    let mut i = 0;
    while let Some(&c) = fmt.get(i) {
        if c != 0x25 {
            out.push(c);
            i += 1;
            continue;
        }
        let pct = i;
        let (num, end, last_digit) = match fmt.get(i + 1) {
            Some(0x25) => {
                out.push(0x25);
                i += 2;
                continue;
            }
            Some(0x28) => {
                let mut j = i + 2;
                let mut n: usize = 0;
                while let Some(d) = digit(fmt.get(j)) {
                    n = n.saturating_mul(10).saturating_add(d as usize);
                    j += 1;
                }
                if j == i + 2 || fmt.get(j) != Some(&0x29) {
                    return Err(wrong(pct + 1));
                }
                (n, j + 1, j)
            }
            c1 if digit(c1).is_some() => {
                let mut j = i + 1;
                let mut n: usize = 0;
                while let Some(d) = digit(fmt.get(j)) {
                    n = n.saturating_mul(10).saturating_add(d as usize);
                    j += 1;
                }
                (n, j, j)
            }
            _ => return Err(wrong(pct + 1)),
        };
        if !(1..=10).contains(&num) {
            return Err(wrong(last_digit));
        }
        max_used = max_used.max(num);
        if let Some(a) = args.get(num - 1) {
            out.extend_from_slice(a);
        }
        i = end;
    }
    if args.len() > max_used {
        return Err(LibError::new(msg::TEMPLATE_TOOMANY));
    }
    Ok(out)
}
