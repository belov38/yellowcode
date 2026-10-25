use super::baltic::{Baltic, Kind};
use super::{Grammar, Part, SubjectShape, TreeLayout, texts, top};
use crate::data::SplNode;
use crate::words::triads;

struct Lv {
    common: Baltic,
    groups: Vec<Vec<String>>,
}
pub(super) fn build(tree: &SplNode) -> Option<Box<dyn Grammar>> {
    let tree = top(tree, 19)?;
    texts(&tree[13], 10)?;
    texts(&tree[14], 1)?;
    Some(Box::new(Lv {
        common: Baltic::parse(tree, Kind::Latvian)?,
        groups: tree[15..19]
            .iter()
            .map(|node| texts(node, 2))
            .collect::<Option<_>>()?,
    }))
}
impl Lv {
    fn number(&self, n: u64, gender: u8, one_hundred: bool) -> String {
        if n == 0 {
            return self.common.zero();
        }
        let triads = triads(n);
        let mut out = Vec::new();
        for i in (1..triads.len()).rev() {
            let k = triads[i];
            if k == 0 {
                continue;
            }
            self.common
                .triad(k, out.is_empty(), 0, one_hundred, &mut out);
            out.push(&self.groups[i - 1][usize::from(k % 10 != 1)]);
        }
        if triads[0] != 0 {
            self.common
                .triad(triads[0], out.is_empty(), gender, one_hundred, &mut out);
        }
        out.join(" ")
    }
}
impl Grammar for Lv {
    fn spell(&self, n: u64, gender: u8, extra: &[String]) -> String {
        self.number(n, gender, extra.first().is_none_or(|value| value != "N"))
    }
    fn fraction_words(&self, digits: &str, gender: u8, extra: &[String]) -> String {
        self.number(
            digits.parse().unwrap_or(0),
            gender,
            extra.get(1).is_none_or(|value| value != "N"),
        )
    }
    fn layout(&self) -> TreeLayout {
        TreeLayout::Spaced { round: true }
    }
    fn subject_shape(&self) -> SubjectShape {
        SubjectShape {
            int_forms: 3,
            frac_forms: 3,
            gendered: true,
            extra: 2,
        }
    }
    fn gender(&self, field: &str) -> u8 {
        u8::from(matches!(field, "S" | "s"))
    }
    fn form(&self, n: u64, _part: Part) -> usize {
        if n % 10 == 1 && n % 100 != 11 {
            0
        } else if n.is_multiple_of(10) {
            2
        } else {
            1
        }
    }
}
