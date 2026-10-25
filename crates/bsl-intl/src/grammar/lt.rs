use super::baltic::{Baltic, Kind};
use super::{Grammar, Part, SubjectShape, TreeLayout, int, list, text, top};
use crate::data::SplNode;
use crate::words::triads;

struct Lt {
    common: Baltic,
    groups: Vec<Vec<String>>,
}
fn form_index(n: u64) -> usize {
    if n % 10 == 1 && n % 100 != 11 {
        0
    } else if (2..=9).contains(&(n % 10)) && !(11..=19).contains(&(n % 100)) {
        1
    } else {
        2
    }
}
pub(super) fn build(tree: &SplNode) -> Option<Box<dyn Grammar>> {
    let tree = top(tree, 17)?;
    let groups = tree[13..17]
        .iter()
        .map(|node| {
            let [one, few, many, gender] = list(node)? else {
                return None;
            };
            int(gender)?;
            Some(vec![
                text(one)?.to_string(),
                text(few)?.to_string(),
                text(many)?.to_string(),
            ])
        })
        .collect::<Option<_>>()?;
    Some(Box::new(Lt {
        common: Baltic::parse(tree, Kind::Lithuanian)?,
        groups,
    }))
}
impl Grammar for Lt {
    fn spell(&self, n: u64, gender: u8, _extra: &[String]) -> String {
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
            self.common.triad(k, out.is_empty(), 0, true, &mut out);
            out.push(&self.groups[i - 1][form_index(k as u64)]);
        }
        if triads[0] != 0 {
            self.common
                .triad(triads[0], out.is_empty(), gender, true, &mut out);
        }
        out.join(" ")
    }
    fn layout(&self) -> TreeLayout {
        TreeLayout::Single
    }
    fn subject_shape(&self) -> SubjectShape {
        SubjectShape {
            int_forms: 3,
            frac_forms: 3,
            gendered: true,
            extra: 0,
        }
    }
    fn gender(&self, field: &str) -> u8 {
        u8::from(matches!(field, "F" | "f"))
    }
    fn form(&self, n: u64, _part: Part) -> usize {
        form_index(n)
    }
}
