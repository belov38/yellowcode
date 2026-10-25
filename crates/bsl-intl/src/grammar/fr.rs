use super::{Grammar, Part, SubjectShape, TreeLayout, texts, top};
use crate::data::SplNode;
use crate::words::{digits3, triads};

struct Fr {
    units: Vec<String>,
    teens: Vec<String>,
    tens: Vec<String>,
    hundreds: Vec<String>,
    groups: Vec<Vec<String>>,
    and: String,
}

pub(super) fn build(tree: &SplNode) -> Option<Box<dyn Grammar>> {
    let tree = top(tree, 9)?;
    Some(Box::new(Fr {
        units: texts(&tree[0], 10)?,
        teens: texts(&tree[1], 6)?,
        tens: texts(&tree[2], 6)?,
        hundreds: texts(&tree[3], 2)?,
        groups: tree[4..8]
            .iter()
            .map(|node| texts(node, 2))
            .collect::<Option<_>>()?,
        and: texts(&tree[8], 1)?.remove(0),
    }))
}

impl Fr {
    fn below100(&self, n: usize) -> String {
        match n {
            0..=9 => self.units[n].clone(),
            10 => self.tens[0].clone(),
            11..=16 => self.teens[n - 11].clone(),
            17..=19 => format!("{}-{}", self.tens[0], self.units[n - 10]),
            20..=69 => {
                let (d, u) = (n / 10, n % 10);
                match u {
                    0 => self.tens[d - 1].clone(),
                    1 => format!("{} {} {}", self.tens[d - 1], self.and, self.units[1]),
                    _ => format!("{}-{}", self.tens[d - 1], self.units[u]),
                }
            }
            70..=79 => {
                let separator = if n == 71 { " " } else { "-" };
                format!("{}{separator}{}", self.tens[5], self.below100(n - 60))
            }
            _ => {
                let base = format!("{}-{}", self.units[4], self.tens[1]);
                if n == 80 {
                    format!("{base}s")
                } else {
                    format!("{base}-{}", self.below100(n - 80))
                }
            }
        }
    }
    fn triad(&self, n: usize) -> String {
        let (h, _, _) = digits3(n);
        let remainder = n % 100;
        let mut out = Vec::new();
        if h == 1 {
            out.push(self.hundreds[0].clone());
        } else if h > 1 {
            let form = usize::from(remainder == 0);
            out.push(format!("{} {}", self.units[h], self.hundreds[form]));
        }
        if remainder != 0 {
            out.push(self.below100(remainder));
        }
        out.join(" ")
    }
}

impl Grammar for Fr {
    fn spell(&self, n: u64, _gender: u8, _extra: &[String]) -> String {
        if n == 0 {
            return self.units[0].clone();
        }
        let triads = triads(n);
        let mut out = Vec::new();
        for (i, group) in self.groups.iter().enumerate().rev() {
            let k = triads[i + 1];
            if k == 0 {
                continue;
            }
            if i == 0 && k == 1 {
                out.push(group[0].clone());
            } else {
                let form = usize::from(i != 0 && k != 1);
                out.push(format!("{} {}", self.triad(k), group[form]));
            }
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
            int_forms: 2,
            frac_forms: 2,
            gendered: false,
            extra: 0,
        }
    }
    fn form(&self, n: u64, _part: Part) -> usize {
        usize::from(n != 0 && n % 10 != 1)
    }
}
