use std::sync::OnceLock;

static TABLE: &[u8] = include_bytes!("../data/case.bin");

fn rd(off: usize) -> u32 {
    TABLE
        .get(off..off + 4)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map_or(0, u32::from_le_bytes)
}

fn layout() -> (usize, usize, usize, usize) {
    static L: OnceLock<(usize, usize, usize, usize)> = OnceLock::new();
    *L.get_or_init(|| {
        let nm = rd(4) as usize;
        let ro = 8usize.saturating_add(nm.saturating_mul(16));
        (8, nm, ro.saturating_add(4), rd(ro) as usize)
    })
}

fn mapping(cp: u32) -> (u32, u32, u32) {
    let (base, n, _, _) = layout();
    let (mut lo, mut hi) = (0usize, n);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let off = base + mid * 16;
        let k = rd(off);
        if k == cp {
            return (rd(off + 4), rd(off + 8), rd(off + 12));
        }
        if k < cp {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    (cp, cp, cp)
}

fn is_letter(cp: u32) -> bool {
    let (_, _, base, n) = layout();
    let (mut lo, mut hi) = (0usize, n);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let off = base + mid * 8;
        if cp < rd(off) {
            hi = mid;
        } else if cp > rd(off + 4) {
            lo = mid + 1;
        } else {
            return true;
        }
    }
    false
}

fn code_points(s: &[u16]) -> impl Iterator<Item = u32> + '_ {
    char::decode_utf16(s.iter().copied()).map(|r| match r {
        Ok(c) => c as u32,
        Err(e) => e.unpaired_surrogate() as u32,
    })
}

fn push(out: &mut Vec<u16>, cp: u32) {
    match char::from_u32(cp) {
        Some(c) => {
            let mut b = [0u16; 2];
            out.extend_from_slice(c.encode_utf16(&mut b));
        }
        None => out.push(cp as u16),
    }
}

fn map_all(s: &[u16], pick: fn((u32, u32, u32)) -> u32) -> Vec<u16> {
    let mut out = Vec::with_capacity(s.len());
    for cp in code_points(s) {
        push(&mut out, pick(mapping(cp)));
    }
    out
}

pub fn upper(s: &[u16]) -> Vec<u16> {
    map_all(s, |m| m.0)
}

pub fn lower(s: &[u16]) -> Vec<u16> {
    map_all(s, |m| m.1)
}

pub fn title(s: &[u16]) -> Vec<u16> {
    let mut out = Vec::with_capacity(s.len());
    let mut in_word = false;
    for cp in code_points(s) {
        let word = cp == 0x5F || is_letter(cp);
        let m = mapping(cp);
        push(
            &mut out,
            if !word {
                cp
            } else if in_word {
                m.1
            } else {
                m.2
            },
        );
        in_word = word;
    }
    out
}
