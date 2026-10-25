use super::{Grammar, Part, SubjectShape, TreeLayout, int, list, text, texts, top};
use crate::data::SplNode;
use crate::words::{digits3, triads};

struct Group {
    one: String,
    many: String,
    feminine: bool,
}
struct Ro {
    and: String,
    zero: String,
    one: Vec<String>,
    two: Vec<String>,
    units: Vec<String>,
    teens: Vec<Vec<String>>,
    tens: Vec<String>,
    hundreds: Vec<String>,
    groups: Vec<Group>,
}
pub(super) fn build(tree: &SplNode) -> Option<Box<dyn Grammar>> {
    let tree = top(tree, 13)?;
    let teens = list(&tree[5])?;
    if teens.len() != 9 {
        return None;
    }
    let teens = teens
        .iter()
        .enumerate()
        .map(|(i, node)| {
            if i == 1 {
                texts(node, 2)
            } else {
                let word = text(node)?.to_string();
                Some(vec![word.clone(), word])
            }
        })
        .collect::<Option<_>>()?;
    let groups = tree[8..13]
        .iter()
        .map(|node| {
            let [one, many, gender] = list(node)? else {
                return None;
            };
            Some(Group {
                one: text(one)?.to_string(),
                many: text(many)?.to_string(),
                feminine: int(gender)? == 1,
            })
        })
        .collect::<Option<_>>()?;
    Some(Box::new(Ro {
        and: texts(&tree[0], 1)?.remove(0),
        zero: texts(&tree[1], 1)?.remove(0),
        one: texts(&tree[2], 4)?,
        two: texts(&tree[3], 2)?,
        units: texts(&tree[4], 7)?,
        teens,
        tens: texts(&tree[6], 9)?,
        hundreds: texts(&tree[7], 9)?,
        groups,
    }))
}
impl Ro {
    fn triad(&self, n: usize, feminine: bool, leading: bool, whole_one: bool) -> String {
        let (h, d, u) = digits3(n);
        let mut out = Vec::new();
        if h == 1 {
            let prefix = &self.one[if leading { 1 } else { 3 }];
            out.push(format!("{prefix} {}", self.hundreds[0]));
        } else if h > 1 {
            out.push(self.hundreds[h - 1].clone());
        }
        if d == 1 && u != 0 {
            out.push(self.teens[u - 1][usize::from(feminine)].clone());
        } else {
            let unit = match u {
                0 => None,
                1 => Some(
                    &self.one[if feminine {
                        1
                    } else if whole_one {
                        2
                    } else {
                        0
                    }],
                ),
                2 => Some(&self.two[usize::from(feminine)]),
                _ => Some(&self.units[u - 3]),
            };
            if d != 0 {
                let mut tens = self.tens[d - 1].clone();
                if let Some(unit) = unit {
                    tens.push(' ');
                    tens.push_str(&self.and);
                    tens.push(' ');
                    tens.push_str(unit);
                }
                out.push(tens);
            } else if let Some(unit) = unit {
                out.push(unit.clone());
            }
        }
        out.join(" ")
    }
}
impl Grammar for Ro {
    fn spell(&self, n: u64, gender: u8, _extra: &[String]) -> String {
        if n == 0 {
            return self.zero.clone();
        }
        let triads = triads(n);
        let mut out = Vec::new();
        for i in (1..triads.len()).rev() {
            let k = triads[i];
            if k == 0 {
                continue;
            }
            let group = &self.groups[i - 1];
            if k == 1 {
                let one = &self.one[if group.feminine {
                    if out.is_empty() { 1 } else { 3 }
                } else {
                    2
                }];
                out.push(format!("{one} {}", group.one));
            } else {
                out.push(format!(
                    "{} {}",
                    self.triad(k, true, out.is_empty(), false),
                    group.many
                ));
            }
        }
        if triads[0] != 0 {
            out.push(self.triad(triads[0], gender == 1, out.is_empty(), n == 1));
        }
        out.join(" ")
    }
    fn layout(&self) -> TreeLayout {
        TreeLayout::Spaced { round: true }
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
        u8::from(matches!(field, "F" | "f"))
    }
    fn form(&self, n: u64, _part: Part) -> usize {
        usize::from(n != 1)
    }
}
