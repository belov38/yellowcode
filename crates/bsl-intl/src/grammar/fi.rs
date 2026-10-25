use super::{Grammar, Part, SubjectShape, TreeLayout, texts, top};
use crate::data::SplNode;
use crate::words::{digits3, triads};

struct Fi {
    units: Vec<String>,
    teens: Vec<String>,
    tens: Vec<String>,
    hundreds: Vec<String>,
    groups: Vec<Vec<String>>,
}

pub(super) fn build(tree: &SplNode) -> Option<Box<dyn Grammar>> {
    let tree = top(tree, 8)?;
    Some(Box::new(Fi {
        units: texts(&tree[0], 10)?,
        teens: texts(&tree[1], 9)?,
        tens: texts(&tree[2], 9)?,
        hundreds: texts(&tree[3], 9)?,
        groups: tree[4..8]
            .iter()
            .map(|node| texts(node, 2))
            .collect::<Option<_>>()?,
    }))
}

impl Fi {
    fn triad(&self, n: usize) -> String {
        let (h, d, u) = digits3(n);
        let mut out = String::new();
        if h != 0 {
            out.push_str(&self.hundreds[h - 1]);
        }
        if d == 1 && u != 0 {
            out.push_str(&self.teens[u - 1]);
        } else {
            if d != 0 {
                out.push_str(&self.tens[d - 1]);
            }
            if u != 0 {
                out.push_str(&self.units[u]);
            }
        }
        out
    }
}

impl Grammar for Fi {
    fn spell(&self, n: u64, _gender: u8, _extra: &[String]) -> String {
        if n == 0 {
            return self.units[0].clone();
        }
        let triads = triads(n);
        let mut out = String::new();
        for (i, group) in self.groups.iter().enumerate().rev() {
            let k = triads[i + 1];
            if k == 0 {
                continue;
            }
            out.push_str(&self.triad(k));
            if i != 3 {
                out.push_str(&group[1]);
            }
        }
        if triads[0] != 0 {
            out.push_str(&self.triad(triads[0]));
        }
        out
    }
    fn layout(&self) -> TreeLayout {
        TreeLayout::Spaced { round: false }
    }
    fn subject_shape(&self) -> SubjectShape {
        SubjectShape {
            int_forms: 2,
            frac_forms: 2,
            gendered: false,
            extra: 0,
        }
    }
    fn form(&self, n: u64, part: Part) -> usize {
        let one = if part == Part::FractionDigits {
            n % 10 == 1
        } else {
            n == 1
        };
        usize::from(!one)
    }
}
