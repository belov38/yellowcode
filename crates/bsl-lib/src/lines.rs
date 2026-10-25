struct Ranges<'a> {
    s: &'a [u16],
    i: usize,
    start: usize,
    split: bool,
    done: bool,
}

impl Iterator for Ranges<'_> {
    type Item = (usize, usize);

    fn next(&mut self) -> Option<(usize, usize)> {
        if self.done {
            return None;
        }
        while let Some(&u) = self.s.get(self.i) {
            let sep = match u {
                0x0A => 1,
                0x0D if self.s.get(self.i + 1) == Some(&0x0A) => 2,
                0x0D => 1,
                _ => {
                    self.i += 1;
                    continue;
                }
            };
            let r = (self.start, self.i);
            self.i += sep;
            self.start = self.i;
            self.split = true;
            return Some(r);
        }
        self.done = true;
        (self.start < self.s.len() || !self.split).then_some((self.start, self.s.len()))
    }
}

fn ranges(s: &[u16]) -> Ranges<'_> {
    Ranges {
        s,
        i: 0,
        start: 0,
        split: false,
        done: false,
    }
}

pub fn line_count(s: &[u16]) -> i64 {
    ranges(s).count() as i64
}

pub fn line(s: &[u16], n: i64) -> &[u16] {
    let Some(idx) = usize::try_from(n).ok().and_then(|n| n.checked_sub(1)) else {
        return &[];
    };
    ranges(s)
        .nth(idx)
        .and_then(|(a, b)| s.get(a..b))
        .unwrap_or(&[])
}
