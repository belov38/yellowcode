use super::{int, list, text, texts};
use crate::data::SplNode;
use crate::words::digits3;

pub(super) enum Kind {
    Lithuanian,
    Latvian,
}
pub(super) struct Baltic {
    zero: String,
    units: Vec<Vec<String>>,
    teens: Vec<String>,
    tens: Vec<String>,
    hundreds: Vec<String>,
    kind: Kind,
}
impl Baltic {
    pub(super) fn parse(tree: &[SplNode], kind: Kind) -> Option<Self> {
        let hundreds = match kind {
            Kind::Lithuanian => {
                let [one, many, unused, gender] = list(&tree[12])? else {
                    return None;
                };
                text(unused)?;
                int(gender)?;
                vec![text(one)?.to_string(), text(many)?.to_string()]
            }
            Kind::Latvian => texts(&tree[12], 9)?,
        };
        Some(Self {
            zero: texts(&tree[0], 1)?.remove(0),
            units: tree[1..10]
                .iter()
                .map(|node| texts(node, 2))
                .collect::<Option<_>>()?,
            teens: texts(&tree[10], 9)?,
            tens: texts(&tree[11], 9)?,
            hundreds,
            kind,
        })
    }
    pub(super) fn zero(&self) -> String {
        self.zero.clone()
    }
    pub(super) fn triad<'a>(
        &'a self,
        n: usize,
        leading: bool,
        gender: u8,
        one_hundred: bool,
        out: &mut Vec<&'a str>,
    ) {
        let (h, d, u) = digits3(n);
        if h == 1 {
            if leading && one_hundred {
                out.push(&self.units[0][0]);
            }
            out.push(&self.hundreds[0]);
        } else if h > 1 {
            match self.kind {
                Kind::Lithuanian => {
                    out.push(&self.units[h - 1][0]);
                    out.push(&self.hundreds[1]);
                }
                Kind::Latvian => out.push(&self.hundreds[h - 1]),
            }
        }
        if d == 1 && u != 0 {
            out.push(&self.teens[u - 1]);
        } else {
            if d != 0 {
                out.push(&self.tens[d - 1]);
            }
            if u != 0 {
                out.push(&self.units[u - 1][usize::from(gender == 1)]);
            }
        }
    }
}
