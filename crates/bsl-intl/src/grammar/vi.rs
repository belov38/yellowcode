use super::{Grammar, SubjectShape, TreeLayout, texts, top};
use crate::data::SplNode;
use crate::words::{digits3, triads};

struct Vi {
    units: Vec<String>,
    tens_hundreds: Vec<String>,
    groups: Vec<String>,
    special: Vec<String>,
}
pub(super) fn build(tree: &SplNode) -> Option<Box<dyn Grammar>> {
    let tree = top(tree, 4)?;
    Some(Box::new(Vi {
        units: texts(&tree[0], 10)?,
        tens_hundreds: texts(&tree[1], 2)?,
        groups: texts(&tree[2], 4)?,
        special: texts(&tree[3], 4)?,
    }))
}
impl Vi {
    fn triad<'a>(&'a self, n: usize, full: bool, out: &mut Vec<&'a str>) {
        let (h, d, u) = digits3(n);
        let start = out.len();
        if h != 0 || full {
            out.push(&self.units[h]);
            out.push(&self.tens_hundreds[1]);
        }
        match d {
            0 => {
                if u != 0 {
                    if out.len() != start {
                        out.push(&self.special[2]);
                    }
                    out.push(&self.units[u]);
                }
            }
            1 => {
                out.push(&self.special[3]);
                if u == 5 {
                    out.push(&self.special[0]);
                } else if u != 0 {
                    out.push(&self.units[u]);
                }
            }
            _ => {
                out.push(&self.units[d]);
                out.push(&self.tens_hundreds[0]);
                if u == 1 {
                    out.push(&self.special[1]);
                } else if u == 5 {
                    out.push(&self.special[0]);
                } else if u != 0 {
                    out.push(&self.units[u]);
                }
            }
        }
    }
}
impl Grammar for Vi {
    fn spell(&self, n: u64, _gender: u8, _extra: &[String]) -> String {
        if n == 0 {
            return self.units[0].clone();
        }
        let mut out = Vec::new();
        for (i, k) in triads(n).into_iter().enumerate().rev() {
            if k == 0 {
                continue;
            }
            self.triad(k, !out.is_empty(), &mut out);
            if i != 0 {
                out.push(&self.groups[i - 1]);
            }
        }
        out.join(" ")
    }
    fn fraction_words(&self, digits: &str, gender: u8, extra: &[String]) -> String {
        let rest = digits.trim_start_matches('0');
        let mut out = vec![self.units[0].clone(); digits.len() - rest.len()];
        if !rest.is_empty() {
            out.push(self.spell(rest.parse().unwrap_or(0), gender, extra));
        }
        out.join(" ")
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
