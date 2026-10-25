use brace::Node;
use mdschema::PropSpec;

pub fn find_collection<'n>(node: &'n Node, class: &str) -> Option<&'n [Node]> {
    for e in node.as_list()? {
        let Some(l) = e.as_list() else { continue };
        let (Some(head), Some(cnt)) = (l.first(), l.get(1)) else {
            continue;
        };
        if head.text() != Some(class) || cnt.is_list() {
            continue;
        }
        let Some(n) = cnt.text().and_then(|t| t.trim().parse::<usize>().ok()) else {
            continue;
        };
        let items = l.get(2..)?;
        return Some(items.get(..n.min(items.len())).unwrap_or(items));
    }
    None
}

pub fn find_header(tree: &Node, uuid: &str) -> Option<Vec<usize>> {
    let is_header = |n: &Node| {
        n.as_list().is_some_and(|l| {
            l.len() >= 5
                && matches!(l.get(2), Some(Node::Str(_)))
                && l.get(1)
                    .and_then(Node::as_list)
                    .is_some_and(|h| h.len() == 3 && h.get(2).and_then(Node::text) == Some(uuid))
        })
    };
    if is_header(tree) {
        return Some(Vec::new());
    }
    let mut stack: Vec<(&[Node], usize)> = vec![(tree.as_list()?, 0)];
    while let Some((l, i)) = stack.last_mut() {
        let Some(c) = l.get(*i) else {
            stack.pop();
            if let Some((_, j)) = stack.last_mut() {
                *j += 1;
            }
            continue;
        };
        if is_header(c) {
            return Some(stack.iter().map(|(_, j)| *j).collect());
        }
        match c.as_list() {
            Some(cl) => stack.push((cl, 0)),
            None => *i += 1,
        }
    }
    None
}

pub(crate) fn prop_path(spec: &PropSpec, hdr: Option<&[usize]>) -> Option<Vec<usize>> {
    if let Some(p) = &spec.path {
        return Some(p.clone());
    }
    let (rel, cut) = if let Some(r) = &spec.hp {
        (r, 1)
    } else if let Some(r) = &spec.hpp {
        (r, 2)
    } else if let Some(r) = &spec.rel_header {
        (r, 1)
    } else {
        return None;
    };
    let h = hdr?;
    let mut out = h.get(..h.len().saturating_sub(cut))?.to_vec();
    out.extend_from_slice(rel);
    Some(out)
}
