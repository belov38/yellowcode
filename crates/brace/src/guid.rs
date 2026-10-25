use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Guid([u8; 16]);

impl Guid {
    pub const NIL: Guid = Guid([0; 16]);

    pub fn parse(s: &str) -> Option<Guid> {
        let b = s.as_bytes();
        if b.len() != 36 {
            return None;
        }
        let mut out = [0u8; 16];
        let mut slots = out.iter_mut();
        let mut hi: Option<u8> = None;
        for (i, &c) in b.iter().enumerate() {
            if matches!(i, 8 | 13 | 18 | 23) {
                if c != b'-' {
                    return None;
                }
                continue;
            }
            let d = u8::try_from(char::from(c).to_digit(16)?).ok()?;
            match hi.take() {
                None => hi = Some(d),
                Some(h) => *slots.next()? = (h << 4) | d,
            }
        }
        Some(Guid(out))
    }

    pub fn from_bytes(b: [u8; 16]) -> Guid {
        Guid(b)
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    pub fn from_bytes_le(b: [u8; 16]) -> Guid {
        Guid(swap_fields(b))
    }

    pub fn to_bytes_le(&self) -> [u8; 16] {
        swap_fields(self.0)
    }

    pub fn is_nil(&self) -> bool {
        self.0 == [0; 16]
    }
}

fn swap_fields(b: [u8; 16]) -> [u8; 16] {
    [
        b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13],
        b[14], b[15],
    ]
}

impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, b) in self.0.iter().enumerate() {
            if matches!(i, 4 | 6 | 8 | 10) {
                f.write_str("-")?;
            }
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}
