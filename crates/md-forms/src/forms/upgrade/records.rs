use brace::Node;

use super::{Cx, Refusal, at, int_at};

fn contains_atom(n: &Node, text: &str) -> bool {
    match n {
        Node::List(l) => l.iter().any(|x| contains_atom(x, text)),
        Node::Atom(a) => a == text,
        Node::Str(_) => false,
    }
}

fn is_zero_pair(n: &Node) -> bool {
    n.as_list()
        .is_some_and(|l| l.len() == 2 && l.iter().all(|x| x.text() == Some("0")))
}

pub(crate) fn attributes(section: &Node, cx: &mut Cx<'_>) -> Result<Node, Refusal> {
    const W: &str = "attributes";
    let dl = cx
        .tables
        .guids
        .get("DynamicList")
        .ok_or_else(|| Refusal::Table("DynamicList".into()))?
        .clone();
    let l = section.as_list().ok_or(Refusal::Record(W))?;
    let n = usize::try_from(int_at(l, 1, W)?).map_err(|_| Refusal::Record(W))?;
    let mut out = l.to_vec();
    for row in out.iter_mut().skip(2).take(n) {
        let a = row.as_list_mut().ok_or(Refusal::Record(W))?;
        let ty = at(a, 5, W)?.clone();
        if !contains_atom(&ty, &dl) {
            continue;
        }
        if at(a, 11, W)?.text() != Some("0") {
            *a.get_mut(11).ok_or(Refusal::Record(W))? = Node::atom("0");
            cx.stats.hit("attr: DL SavedData cleared");
        }
        let zero = || Node::list(vec![Node::atom("0"), Node::atom("0")]);
        if at(a, 9, W)?.is_list() && !is_zero_pair(at(a, 9, W)?) {
            *a.get_mut(9).ok_or(Refusal::Record(W))? = zero();
            cx.stats.hit("attr: DL Save cleared");
        }
        let exact = ty.as_list().is_some_and(|t| t.len() == 2);
        if exact && at(a, 8, W)?.is_list() && !is_zero_pair(at(a, 8, W)?) {
            *a.get_mut(8).ok_or(Refusal::Record(W))? = zero();
            cx.stats.hit("attr: DL UseAlways cleared");
        }
    }
    Ok(Node::list(out))
}

pub(crate) fn commands(section: &Node, cx: &mut Cx<'_>) -> Result<Node, Refusal> {
    const W: &str = "commands";
    let l = section.as_list().ok_or(Refusal::Record(W))?;
    let n = usize::try_from(int_at(l, 1, W)?).map_err(|_| Refusal::Record(W))?;
    let mut out = l.to_vec();
    for row in out.iter_mut().skip(2).take(n) {
        let c = row.as_list().ok_or(Refusal::Record(W))?;
        if c.first().and_then(Node::text) == Some("9") {
            let mut nc = vec![Node::atom("11")];
            nc.extend(c.iter().skip(1).cloned());
            nc.push(Node::atom("0"));
            nc.push(Node::atom("0"));
            *row = Node::list(nc);
            cx.stats.hit("command v9->v11");
        }
    }
    Ok(Node::list(out))
}
