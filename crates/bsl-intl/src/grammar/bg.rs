use super::{Grammar, Part, SubjectShape, TreeLayout, int, list, text, texts, top};
use crate::data::SplNode;
use crate::words::{digits3, triads};

struct Group {
    one: String,
    many: String,
    gender: u8,
}

struct Bg {
    and: String,
    zero: String,
    one: Vec<String>,
    two: Vec<String>,
    units: Vec<String>,
    teens: Vec<String>,
    tens: Vec<String>,
    hundreds: Vec<String>,
    groups: Vec<Group>,
}

pub(super) fn build(tree: &SplNode) -> Option<Box<dyn Grammar>> {
    let t = top(tree, 12)?;
    let groups = t[8..12]
        .iter()
        .map(|g| match list(g)? {
            [one, many, gender] => Some(Group {
                one: text(one)?.into(),
                many: text(many)?.into(),
                gender: u8::try_from(int(gender)?).ok().filter(|g| *g <= 2)?,
            }),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Box::new(Bg {
        and: texts(&t[0], 1)?.remove(0),
        zero: texts(&t[1], 1)?.remove(0),
        one: texts(&t[2], 4)?,
        two: texts(&t[3], 2)?,
        units: texts(&t[4], 7)?,
        teens: texts(&t[5], 9)?,
        tens: texts(&t[6], 9)?,
        hundreds: texts(&t[7], 9)?,
        groups,
    }))
}

impl Bg {
    fn triad<'a>(&'a self, t: usize, gender: u8, out: &mut Vec<&'a str>) {
        let start = out.len();
        let (h, d, u) = digits3(t);
        if h > 0 {
            out.push(&self.hundreds[h - 1]);
        }
        if d == 1 && u > 0 {
            out.push(&self.teens[u - 1]);
        } else {
            if d > 0 {
                out.push(&self.tens[d - 1]);
            }
            match u {
                0 => {}
                1 => out.push(&self.one[usize::from(gender)]),
                2 => out.push(&self.two[usize::from(gender != 0)]),
                u => out.push(&self.units[u - 3]),
            }
        }
        if out.len() - start >= 2 {
            out.insert(out.len() - 1, &self.and);
        }
    }
}

impl Grammar for Bg {
    fn spell(&self, n: u64, gender: u8, _extra: &[String]) -> String {
        if n == 0 {
            return self.zero.clone();
        }
        let t = triads(n);
        let mut out = Vec::new();
        for (i, g) in self.groups.iter().enumerate().rev() {
            let k = t[i + 1];
            if k == 0 {
                continue;
            }
            if i == 0 && k == 1 {
                out.push(g.one.as_str());
                continue;
            }
            self.triad(k, g.gender, &mut out);
            out.push(if k == 1 { &g.one } else { &g.many });
        }
        self.triad(t[0], gender, &mut out);
        out.join(" ")
    }
    fn layout(&self) -> TreeLayout {
        TreeLayout::Single
    }
    fn subject_shape(&self) -> SubjectShape {
        SubjectShape {
            int_forms: 2,
            frac_forms: 2,
            gendered: true,
            extra: 0,
        }
    }
    fn gender(&self, field: &str) -> u8 {
        crate::words::gender(field)
    }
    fn form(&self, n: u64, _part: Part) -> usize {
        usize::from(n != 1)
    }
    fn joint(&self) -> Option<&str> {
        Some(&self.and)
    }
}
