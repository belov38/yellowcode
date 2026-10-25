use super::{Capital, Grammar, SubjectShape, TreeLayout, texts, top};
use crate::data::SplNode;

struct Zh {
    digits: Vec<String>,
    positions: Vec<String>,
    groups: Vec<String>,
}

pub(super) fn build(tree: &SplNode) -> Option<Box<dyn Grammar>> {
    let tree = top(tree, 4)?;
    texts(&tree[1], 1)?;
    Some(Box::new(Zh {
        digits: texts(&tree[0], 10)?,
        positions: texts(&tree[2], 3)?,
        groups: texts(&tree[3], 3)?,
    }))
}

impl Grammar for Zh {
    fn spell(&self, mut n: u64, _gender: u8, _extra: &[String]) -> String {
        if n == 0 {
            return self.digits[0].clone();
        }
        let mut groups = [0; 4];
        for group in &mut groups {
            *group = (n % 10000) as usize;
            n /= 10000;
        }
        let top = groups.iter().rposition(|&n| n != 0).unwrap_or(0);
        let mut out = String::new();
        let mut emitted = false;
        for i in (0..=top).rev() {
            let group = groups[i];
            if group == 0 {
                continue;
            }
            let digits = [group / 1000, group / 100 % 10, group / 10 % 10, group % 10];
            let lower_zero = groups[..i].iter().all(|&n| n == 0);
            let mut part = String::new();
            let mut pending = emitted && digits[0] == 0;
            for (pos, digit) in digits.into_iter().enumerate() {
                if digit == 0 {
                    if !part.is_empty() {
                        pending = true;
                    }
                    continue;
                }
                if pending {
                    part.push_str(&self.digits[0]);
                    pending = false;
                }
                let unit = if pos < 3 {
                    &self.positions[2 - pos]
                } else {
                    ""
                };
                if !(pos == 2
                    && digit == 1
                    && i == top
                    && part.is_empty()
                    && lower_zero
                    && !emitted)
                {
                    part.push_str(&self.digits[digit]);
                }
                part.push_str(unit);
            }
            out.push_str(&part);
            if i != 0 {
                out.push_str(&self.groups[i - 1]);
            }
            emitted = true;
        }
        out
    }
    fn fraction_digits(&self, digits: &str) -> String {
        digits
            .bytes()
            .map(|digit| self.digits[usize::from(digit - b'0')].as_str())
            .collect()
    }
    fn fraction_words(&self, digits: &str, _gender: u8, _extra: &[String]) -> String {
        self.fraction_digits(digits)
    }
    fn layout(&self) -> TreeLayout {
        TreeLayout::Units
    }
    fn subject_shape(&self) -> SubjectShape {
        SubjectShape {
            int_forms: 1,
            frac_forms: 2,
            gendered: false,
            extra: 0,
        }
    }
    fn capital(&self) -> Capital {
        Capital::Keep
    }
}
