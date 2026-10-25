use super::{Grammar, SubjectShape, TreeLayout, texts, top};
use crate::data::SplNode;
use crate::words::{digits3, triads};

struct Hu {
    units: Vec<String>,
    tens: Vec<String>,
    prefixes: Vec<String>,
    groups: Vec<String>,
}

pub(super) fn build(tree: &SplNode) -> Option<Box<dyn Grammar>> {
    let tree = top(tree, 4)?;
    Some(Box::new(Hu {
        units: texts(&tree[0], 10)?,
        tens: texts(&tree[1], 9)?,
        prefixes: texts(&tree[2], 2)?,
        groups: texts(&tree[3], 5)?,
    }))
}

impl Hu {
    fn triad(&self, n: usize) -> String {
        let (h, d, u) = digits3(n);
        let mut out = String::new();
        if h != 0 {
            out.push_str(&self.units[h]);
            out.push_str(&self.groups[0]);
        }
        if (d == 1 || d == 2) && u != 0 {
            out.push_str(&self.prefixes[d - 1]);
            out.push_str(&self.units[u]);
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

impl Grammar for Hu {
    fn spell(&self, n: u64, _gender: u8, _extra: &[String]) -> String {
        if n == 0 {
            return self.units[0].clone();
        }
        let triads = triads(n);
        let mut parts = Vec::new();
        for (i, k) in triads.into_iter().enumerate().rev() {
            if k == 0 {
                continue;
            }
            let mut part = self.triad(k);
            if i != 0 {
                part.push_str(&self.groups[i]);
            }
            parts.push(part);
        }
        parts.join(if n > 2000 { "-" } else { "" })
    }
    fn fraction_digits(&self, digits: &str) -> String {
        digits.parse::<u64>().unwrap_or(0).to_string()
    }
    fn layout(&self) -> TreeLayout {
        TreeLayout::Single
    }
    fn subject_shape(&self) -> SubjectShape {
        SubjectShape {
            int_forms: 1,
            frac_forms: 1,
            gendered: false,
            extra: 0,
        }
    }
}
