use super::codec::{InternalReader, InternalWriter, ValueCodec};
use super::internal::text::decode_str;
use super::kind::parse_guid;
use crate::collections::{method_name, new_structure, not_implemented, type_name_ru};
use brace::Node;
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, ObjRef, RetainedMemory,
    TypeId, Value,
};
use platform_catalog::TypeIdx;
use std::any::Any;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

#[derive(Clone, Copy)]
enum Shape {
    FormattedDocument,
    CompositionSettings,
}

struct Opaque {
    t: TypeIdx,
    shape: Shape,
}

fn refuse(ex: &mut Execution, t: TypeIdx, what: &str) -> Exception {
    ex.error(HostError::not_implemented(type_name_ru(t), what))
}

pub(crate) const HTML_HEAD: &str = "<!DOCTYPE html>\n<html dir=\"ltr\">\n<head>\n<meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\" />\n<meta http-equiv=\"X-UA-Compatible\" content=\"IE=Edge\" />\n<meta name=\"format-detection\" content=\"telephone=no\" />\n<style type=\"text/css\">\nbody{margin:0;padding:8px;}\np{line-height:1.15;margin:0;white-space:pre-wrap;}\nol,ul{margin-top:0;margin-bottom:0;}\nimg{border:none;}\nli>p{display:inline;}\n</style>\n</head>\n<body>\n";
const HTML_PARA: &str = "<p style=\"line-height: 1.15;text-align: left;\">";
pub(crate) const HTML_TAIL: &str = "</body>\n</html>";

struct Shapes {
    run_head: Vec<Node>,
    run_tail: Vec<Node>,
    para_tail: Node,
    closing_head: Vec<Node>,
    closing_run: Node,
    doc_tail: Vec<Node>,
}

fn parsed(text: &str) -> Option<Node> {
    brace::parse(text).ok()
}

fn shapes() -> Option<&'static Shapes> {
    static S: OnceLock<Option<Shapes>> = OnceLock::new();
    S.get_or_init(|| {
        let list = |t: &str| parsed(t).and_then(|n| n.as_list().map(<[Node]>::to_vec));
        Some(Shapes {
            run_head: list("{1,0,0,0,1.15,1,0}")?,
            run_tail: list("{\"\",{7,3,0,1,100},{3,4,{0}},{3,4,{0}},{0,-1}}")?,
            para_tail: parsed(
                "{11,{{{0,{0,\"\",\"\",\"\",\"\",-1,0,0,1.15,1,\"\",0,0}},0},0},-1}",
            )?,
            closing_head: list("{1,0,0,0,1.15,1,2}")?,
            closing_run: parsed("{1,{0,-1}}")?,
            doc_tail: list("{1,1,{{0,\"AbsoluteFont\"},0,{0},0}}")?,
        })
    })
    .as_ref()
}

fn same(a: &Node, b: &Node) -> bool {
    match (a, b) {
        (Node::List(x), Node::List(y)) => all_same(x, y),
        (Node::List(_), _) | (_, Node::List(_)) => false,
        _ => match (decode_str(a), decode_str(b)) {
            (Some(x), Some(y)) => x == y,
            (None, None) => a.as_atom() == b.as_atom(),
            _ => false,
        },
    }
}

fn all_same(x: &[Node], y: &[Node]) -> bool {
    x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b))
}

fn paragraphs(body: &[Node]) -> Option<Vec<String>> {
    let s = shapes()?;
    let [one, id, Node::List(content), rest @ ..] = body else {
        return None;
    };
    if one.as_atom()? != "1" || parse_guid(id.as_atom()?).is_none() || !all_same(rest, &s.doc_tail)
    {
        return None;
    }
    let [zero, n, zero2, n2, paras @ ..] = content.as_slice() else {
        return None;
    };
    let count = n.as_atom()?.parse::<usize>().ok()?;
    if zero.as_atom()? != "0" || zero2.as_atom()? != "0" || n2 != n || count != paras.len() {
        return None;
    }
    let (closing, runs) = paras.split_last()?;
    let mut out = Vec::with_capacity(paras.len());
    for (i, p) in runs.iter().enumerate() {
        let [one, idx, head @ .., Node::List(run), tail] = p.as_list()? else {
            return None;
        };
        let [r1, text, run_rest @ ..] = run.as_slice() else {
            return None;
        };
        if one.as_atom()? != "1"
            || idx.as_atom()?.parse::<usize>().ok()? != i
            || !all_same(head, s.run_head.get(1..)?)
            || r1.as_atom()? != "1"
            || !all_same(run_rest, &s.run_tail)
            || !same(tail, &s.para_tail)
        {
            return None;
        }
        let text = String::from_utf16(&decode_str(text)?).ok()?;
        if text.chars().any(char::is_control) {
            return None;
        }
        out.push(text);
    }
    let [one, idx, head @ .., run, tail] = closing.as_list()? else {
        return None;
    };
    if one.as_atom()? != "1"
        || idx.as_atom()?.parse::<usize>().ok()? != runs.len()
        || !all_same(head, s.closing_head.get(1..)?)
        || !same(run, &s.closing_run)
        || !same(tail, &s.para_tail)
    {
        return None;
    }
    out.push(String::new());
    Some(out)
}

fn empty_document(body: &[Node]) -> bool {
    let Some(s) = shapes() else {
        return false;
    };
    let [one, id, Node::List(content), rest @ ..] = body else {
        return false;
    };
    one.as_atom() == Some("1")
        && id.as_atom().and_then(parse_guid).is_some()
        && content.len() == 4
        && content.iter().all(|n| n.as_atom() == Some("0"))
        && all_same(rest, &s.doc_tail)
}

fn html_of(paras: &[String]) -> String {
    let mut out = String::from(HTML_HEAD);
    for p in paras {
        out.push_str(HTML_PARA);
        if p.is_empty() {
            out.push_str("<br>");
        }
        for c in p.chars() {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"),
                c => out.push(c),
            }
        }
        out.push_str("</p>\n");
    }
    out.push_str(HTML_TAIL);
    out
}

fn text_of(paras: &[String]) -> String {
    let mut out = paras.join("\n");
    out.push('\n');
    out
}

struct Stored {
    t: TypeIdx,
    paras: Option<Vec<String>>,
}

impl Stored {
    fn bytes(&self) -> u64 {
        self.paras
            .as_ref()
            .map_or(0, |p| p.iter().map(|s| s.capacity() as u64 + 24).sum())
    }
}

impl HostObject for Stored {
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        if let Some(p) = &self.paras {
            visit.allocation(p.as_ptr().cast::<()>() as usize, self.bytes())?;
        }
        Ok(true)
    }
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.t)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(not_implemented(self.t, m).into())
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(not_implemented(self.t, m).into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let Some(paras) = &self.paras else {
            return Err(not_implemented(self.t, m).into());
        };
        match method_name(self.t, m) {
            Some("GetHTML") => {
                let html = html_of(paras);
                ex.reserve((html.len() as u64).saturating_mul(2))?;
                ex.charge(1 + html.len() as u64 / 64)?;
                if let Some(Arg::Place(p)) = args.first() {
                    p.set(ex, Value::String(Str::from(html.as_str())))?;
                }
                if let Some(Arg::Place(p)) = args.get(1) {
                    p.set(ex, new_structure(Vec::new()))?;
                }
                Ok(Value::Undefined)
            }
            Some("GetText") if args.iter().all(|a| matches!(a, Arg::Omitted)) => {
                let text = text_of(paras);
                ex.reserve((text.len() as u64).saturating_mul(2))?;
                Ok(Value::String(Str::from(text.as_str())))
            }
            _ => Err(not_implemented(self.t, m).into()),
        }
    }
    fn presentation(&self, _: &mut Execution) -> CallResult<Option<Str>> {
        if self.paras.is_some() {
            return Ok(Some(Str::from(type_name_ru(self.t))));
        }
        Err(HostError::not_implemented(type_name_ru(self.t), "Строка").into())
    }
}

impl Opaque {
    fn accepts(&self, items: &[Node], r: &InternalReader) -> bool {
        let [Node::List(body)] = items else {
            return false;
        };
        match self.shape {
            Shape::FormattedDocument => {
                body.len() > 2
                    && body.first().and_then(Node::as_atom) == Some("1")
                    && body
                        .get(1)
                        .and_then(Node::as_atom)
                        .and_then(parse_guid)
                        .is_some()
            }
            Shape::CompositionSettings => {
                r.is_storage()
                    && body.len() == 1
                    && body.first().and_then(Node::as_atom) == Some("0")
                    && r.stream(0).is_some()
            }
        }
    }
}

impl ValueCodec for Opaque {
    fn internal_write(
        &self,
        ex: &mut Execution,
        _v: &ObjRef,
        _w: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        Err(refuse(ex, self.t, "ЗначениеВСтрокуВнутр"))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        items: &[Node],
        r: &mut InternalReader,
    ) -> Result<Value, Exception> {
        if !self.accepts(items, r) {
            return Err(refuse(ex, self.t, "ЗначениеИзСтрокиВнутр"));
        }
        let paras = match (self.shape, items) {
            (Shape::FormattedDocument, [Node::List(body)]) => {
                let p = paragraphs(body);
                if p.is_none() && !empty_document(body) {
                    return Err(refuse(ex, self.t, "ЗначениеИзСтрокиВнутр"));
                }
                p
            }
            _ => None,
        };
        if let Some(p) = &paras {
            ex.reserve(p.iter().map(|s| s.len() as u64 + 24).sum())?;
        }
        Ok(Value::Object(Rc::new(Stored { t: self.t, paras })))
    }
}

pub(crate) fn codecs() -> Vec<(TypeIdx, Arc<dyn ValueCodec>)> {
    let mut v = Vec::new();
    for (id, shape) in [
        ("FormattedDocument", Shape::FormattedDocument),
        ("DataCompositionSettings", Shape::CompositionSettings),
    ] {
        if let Some(t) = crate::enums::type_by_id(id) {
            v.push((t, Arc::new(Opaque { t, shape }) as Arc<dyn ValueCodec>));
        }
    }
    v
}
