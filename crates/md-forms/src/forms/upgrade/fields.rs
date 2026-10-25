use brace::Node;

use super::items::{Env, apply};
use super::{Cx, Refusal, at, int_at};

pub(crate) fn cpl5006(v: &Node) -> Result<Node, Refusal> {
    let l = v.as_list().ok_or(Refusal::Shape("ChoiceParameterLinks"))?;
    let mut out = vec![Node::atom("5006")];
    out.extend(l.iter().skip(1).cloned());
    Ok(Node::list(out))
}

type Link<'a> = (&'a Node, &'a [Node], &'a Node);

fn cpl_links(l: &[Node]) -> Result<Vec<Link<'_>>, Refusal> {
    const W: &str = "ChoiceParameterLinks";
    let n = usize::try_from(int_at(l, 1, W)?).map_err(|_| Refusal::Shape(W))?;
    let mut i = 2;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let k = usize::try_from(int_at(l, i + 1, W)?).map_err(|_| Refusal::Shape(W))?;
        out.push((
            at(l, i, W)?,
            l.get(i + 2..i + 2 + k).ok_or(Refusal::Shape(W))?,
            at(l, i + 2 + k, W)?,
        ));
        i += 3 + k;
    }
    Ok(out)
}

pub(crate) fn cpl5007(v: &Node) -> Result<Node, Refusal> {
    let l = v.as_list().ok_or(Refusal::Shape("ChoiceParameterLinks"))?;
    let mut out = vec![
        Node::atom("5007"),
        at(l, 1, "ChoiceParameterLinks")?.clone(),
    ];
    for (name, paths, mode) in cpl_links(l)? {
        out.push(name.clone());
        out.push(Node::int(i64::try_from(paths.len()).unwrap_or(0)));
        out.extend(paths.iter().cloned());
        out.push(mode.clone());
        out.push(Node::str(""));
        out.push(Node::str(""));
    }
    Ok(Node::list(out))
}

pub(crate) fn sublist(kind: &str, old: &Node, cx: &mut Cx<'_>) -> Result<Node, Refusal> {
    let l = old.as_list().ok_or(Refusal::Shape("Field[39]"))?;
    let ov = l.first().and_then(Node::text).unwrap_or("?");
    let refuse = || Refusal::NoRuleKind {
        what: "field-kind",
        kind: kind.into(),
        version: ov.into(),
    };
    let c = cx
        .tables
        .sublists
        .get(&format!("Field:{kind}/{ov}"))
        .ok_or_else(refuse)?;
    if l.len() != c.old_len {
        return Err(refuse());
    }
    let env = Env {
        class: "FieldKind",
        body: l,
        kind,
    };
    Ok(Node::list(apply(c, &env, cx)?))
}
