use super::{Capital, Grammar, SubjectShape, TreeLayout, texts, top};
use crate::data::SplNode;
use crate::words::triads;

struct Ka {
    endings: Vec<String>,
    units: Vec<String>,
    twenties: Vec<String>,
    hundreds: Vec<String>,
    groups: Vec<String>,
}

pub(super) fn build(tree: &SplNode) -> Option<Box<dyn Grammar>> {
    let tree = top(tree, 5)?;
    Some(Box::new(Ka {
        endings: texts(&tree[0], 2)?,
        units: texts(&tree[1], 20)?,
        twenties: texts(&tree[2], 4)?,
        hundreds: texts(&tree[3], 9)?,
        groups: texts(&tree[4], 4)?,
    }))
}
impl Ka {
    fn below100(&self, n: usize) -> String {
        if n < 20 {
            return self.units[n].clone();
        }
        let (group, remainder) = (n / 20, n % 20);
        if remainder == 0 {
            format!("{}{}", self.twenties[group - 1], self.endings[0])
        } else {
            format!(
                "{}{}{}",
                self.twenties[group - 1],
                self.endings[1],
                self.units[remainder]
            )
        }
    }
    fn triad(&self, n: usize) -> String {
        let (hundreds, remainder) = (n / 100, n % 100);
        if hundreds == 0 {
            self.below100(remainder)
        } else if remainder == 0 {
            format!("{}{}", self.hundreds[hundreds - 1], self.endings[0])
        } else {
            format!(
                "{} {}",
                self.hundreds[hundreds - 1],
                self.below100(remainder)
            )
        }
    }
}
impl Grammar for Ka {
    fn spell(&self, n: u64, _gender: u8, _extra: &[String]) -> String {
        if n == 0 {
            return self.units[0].clone();
        }
        let triads = triads(n);
        let mut out = Vec::new();
        for i in (1..triads.len()).rev() {
            let k = triads[i];
            if k == 0 {
                continue;
            }
            let suffix = if triads[..i].iter().all(|&n| n == 0) {
                self.endings[0].as_str()
            } else {
                ""
            };
            out.push(format!("{} {}{suffix}", self.triad(k), self.groups[i - 1]));
        }
        if triads[0] != 0 {
            out.push(self.triad(triads[0]));
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
    fn joint(&self) -> Option<&str> {
        Some(&self.endings[1])
    }
    fn capital(&self) -> Capital {
        Capital::Keep
    }
}
