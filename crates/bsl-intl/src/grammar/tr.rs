use super::{Capital, Grammar, SubjectShape, TreeLayout, texts, top};
use crate::data::SplNode;
use crate::words::{digits3, triads};

struct Tr {
    units: Vec<String>,
    tens: Vec<String>,
    groups: Vec<String>,
}

pub(super) fn build(tree: &SplNode) -> Option<Box<dyn Grammar>> {
    let tree = top(tree, 4)?;
    texts(&tree[3], 2)?;
    Some(Box::new(Tr {
        units: texts(&tree[0], 10)?,
        tens: texts(&tree[1], 9)?,
        groups: texts(&tree[2], 5)?,
    }))
}

fn capital(word: &str) -> String {
    let mut chars = word.chars();
    let mut out = String::new();
    match chars.next() {
        Some('i') => out.push('İ'),
        Some('ı') => out.push('I'),
        Some(c) => out.extend(c.to_uppercase()),
        None => {}
    }
    out.push_str(chars.as_str());
    out
}

fn lower(word: &str) -> String {
    word.chars()
        .flat_map(|c| match c {
            'İ' => 'i'.to_lowercase(),
            'I' => 'ı'.to_lowercase(),
            c => c.to_lowercase(),
        })
        .collect()
}

impl Tr {
    fn triad<'a>(&'a self, n: usize, out: &mut Vec<&'a str>) {
        let (h, d, u) = digits3(n);
        if h != 0 {
            if h != 1 {
                out.push(&self.units[h]);
            }
            out.push(&self.groups[0]);
        }
        if d != 0 {
            out.push(&self.tens[d - 1]);
        }
        if u != 0 {
            out.push(&self.units[u]);
        }
    }
    fn words(&self, n: u64) -> Vec<&str> {
        if n == 0 {
            return vec![&self.units[0]];
        }
        let triads = triads(n);
        let mut out = Vec::new();
        for i in (1..triads.len()).rev() {
            let k = triads[i];
            if k == 0 {
                continue;
            }
            if i != 1 || k != 1 {
                self.triad(k, &mut out);
            }
            out.push(&self.groups[i]);
        }
        if triads[0] != 0 {
            self.triad(triads[0], &mut out);
        }
        out
    }
}

impl Grammar for Tr {
    fn spell(&self, n: u64, _gender: u8, extra: &[String]) -> String {
        let words = self.words(n);
        if extra.first().is_some_and(|value| value == "Separate") {
            words.into_iter().map(lower).collect::<Vec<_>>().join(" ")
        } else {
            words.into_iter().map(capital).collect()
        }
    }
    fn layout(&self) -> TreeLayout {
        TreeLayout::Comma
    }
    fn subject_shape(&self) -> SubjectShape {
        SubjectShape {
            int_forms: 1,
            frac_forms: 1,
            gendered: false,
            extra: 1,
        }
    }
    fn capital(&self) -> Capital {
        Capital::Turkic
    }
}
