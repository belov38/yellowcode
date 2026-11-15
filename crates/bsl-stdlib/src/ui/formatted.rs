use super::color::{Color, ColorKind};
use super::font::font_data;
use super::{NS_UI, SharedOf, data_text, types};
use crate::bind::{arg_value, value_mismatch};
use crate::collections::{ctor_not_implemented, not_implemented};
use crate::serial::codec::{InternalReader, InternalWriter, ValueCodec};
use crate::serial::internal::text::{decode_str, encode_str};
use crate::xdto::{XdtoCodec, XdtoCtx, XdtoData};
use brace::Node;
use bsl_lib::xdto::QName;
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostObject, MemberId, ObjRef, SharedValue, TypeId, Value,
};
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

const MAX_DEPTH: usize = 32;

pub(crate) struct Formatted {
    markup: String,
}

pub(super) fn presentation_input_len(value: &Value) -> Option<u64> {
    let formatted = value.as_object()?.as_any().downcast_ref::<Formatted>()?;
    Some(formatted.markup.len() as u64)
}

fn new_formatted(markup: String) -> Value {
    Value::Object(Rc::new(Formatted { markup }))
}

fn formatted_of(v: &Value) -> Option<&Formatted> {
    v.as_object()?.as_any().downcast_ref::<Formatted>()
}

fn unsupported() -> bsl_vm::CallError {
    ctor_not_implemented(types().formatted).into()
}

fn escape(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '<' => out.push_str("<<"),
            '>' => out.push_str(">>"),
            _ => out.push(c),
        }
    }
}

fn text_of(markup: &str) -> String {
    let mut out = String::new();
    let mut chars = markup.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' if chars.peek() == Some(&'<') => {
                chars.next();
                out.push('<');
            }
            '>' if chars.peek() == Some(&'>') => {
                chars.next();
                out.push('>');
            }
            '<' => {
                let mut tag = String::new();
                for t in chars.by_ref() {
                    if t == '>' {
                        break;
                    }
                    tag.push(t);
                }
                if tag.starts_with("img ") {
                    out.push(' ');
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn push_part(
    ex: &mut Execution,
    v: &Value,
    single: bool,
    depth: usize,
    out: &mut String,
) -> CallResult<Option<()>> {
    match v {
        Value::String(s) => {
            let text = s.to_string_lossy();
            ex.reserve((out.len() + text.len() * 2) as u64 * 3)?;
            escape(&text, out);
            Ok(Some(()))
        }
        Value::Object(_) => {
            if let Some(f) = formatted_of(v) {
                ex.reserve((out.len() + f.markup.len()) as u64 * 3)?;
                out.push_str(&f.markup);
                return Ok(Some(()));
            }
            let Some(items) = crate::collections::array_items(v) else {
                return Ok(None);
            };
            if depth >= MAX_DEPTH {
                return Ok(None);
            }
            for item in &items {
                match item {
                    Value::String(_) | Value::Object(_) => {}
                    Value::Number(_) if single && depth == 0 => {
                        return Err(value_mismatch(1).into());
                    }
                    _ => return Ok(None),
                }
                if push_part(ex, item, single, depth + 1, out)?.is_none() {
                    return Ok(None);
                }
            }
            Ok(Some(()))
        }
        _ => Ok(None),
    }
}

fn hex(r: u8, g: u8, b: u8) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn text_color_tag(v: &Value) -> Option<String> {
    let c = v.as_object()?.as_any().downcast_ref::<Color>()?;
    match &c.kind {
        ColorKind::Abs(r, g, b) => Some(format!("color {}", hex(*r, *g, *b))),
        ColorKind::Auto => Some("color #000000".to_string()),
        ColorKind::Web("Red") => Some("color #ff0000".to_string()),
        ColorKind::Style { id: Some(-3), .. } => Some("colorstyle -3".to_string()),
        _ => None,
    }
}

fn back_color_tag(v: &Value) -> Option<String> {
    let c = v.as_object()?.as_any().downcast_ref::<Color>()?;
    match &c.kind {
        ColorKind::Abs(r, g, b) => Some(format!("bgcolor {}", hex(*r, *g, *b))),
        _ => None,
    }
}

fn link_tag(link: &str) -> Option<String> {
    let plain = !link
        .chars()
        .any(|c| c == '<' || c == '>' || c.is_whitespace() || c.is_control());
    plain.then(|| format!("link {link}"))
}

pub(super) fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let mut vals = Vec::with_capacity(args.len());
    for i in 0..args.len() {
        vals.push(arg_value(ex, args.get(i))?);
    }
    if vals.is_empty() {
        return Err(unsupported());
    }
    let none = |v: &Value| matches!(v, Value::Undefined);
    let at = |i: usize| vals.get(i).cloned().unwrap_or(Value::Undefined);
    let described = vals.len() <= 5
        && (none(&at(1)) || font_data(&at(1)).is_some())
        && (none(&at(4)) || matches!(at(4), Value::String(_)));
    if described {
        return described_form(ex, &vals);
    }
    let mut markup = String::new();
    for v in &vals {
        let part = matches!(v, Value::String(_)) || formatted_of(v).is_some();
        if !part || push_part(ex, v, false, 0, &mut markup)?.is_none() {
            return Err(unsupported());
        }
    }
    Ok(new_formatted(markup))
}

fn described_form(ex: &mut Execution, vals: &[Value]) -> CallResult<Value> {
    let at = |i: usize| vals.get(i).cloned().unwrap_or(Value::Undefined);
    let font = at(1);
    let color = at(2);
    let back = at(3);
    let link = match at(4) {
        Value::String(s) => Some(s.to_string_lossy()).filter(|l| !l.is_empty()),
        _ => None,
    };
    let mut tags: Vec<String> = Vec::new();
    let mut kinds: Vec<char> = Vec::new();
    if let Some(f) = font_data(&font) {
        let Some(font_tags) = f.formatted_tags() else {
            return Err(unsupported());
        };
        tags.extend(font_tags);
        kinds.push('f');
    }
    if !matches!(color, Value::Undefined) {
        tags.push(text_color_tag(&color).ok_or_else(unsupported)?);
        kinds.push('c');
    }
    if !matches!(back, Value::Undefined) {
        tags.push(back_color_tag(&back).ok_or_else(unsupported)?);
        kinds.push('b');
    }
    if let Some(l) = &link {
        tags.push(link_tag(l).ok_or_else(unsupported)?);
        kinds.push('l');
    }
    let known = matches!(
        kinds.as_slice(),
        [] | [_] | ['f', 'c'] | ['c', 'b'] | ['c', 'l']
    );
    if !known {
        return Err(unsupported());
    }
    let first = at(0);
    if tags.is_empty() {
        let mut markup = String::new();
        if push_part(ex, &first, vals.len() == 1, 0, &mut markup)?.is_none() {
            return Err(unsupported());
        }
        return Ok(new_formatted(markup));
    }
    let Value::String(s) = &first else {
        return Err(unsupported());
    };
    let text = s.to_string_lossy();
    if text.is_empty() {
        return Err(unsupported());
    }
    let tag_len: usize = tags.iter().map(|t| t.len() + 5).sum();
    ex.reserve((text.len() * 2 + tag_len) as u64 * 3)?;
    let mut markup = String::new();
    for t in &tags {
        markup.push('<');
        markup.push_str(t);
        markup.push('>');
    }
    escape(&text, &mut markup);
    for _ in &tags {
        markup.push_str("</>");
    }
    Ok(new_formatted(markup))
}

impl HostObject for Formatted {
    fn retained_memory(&self, visit: &mut dyn bsl_vm::RetainedMemory) -> Result<bool, Exception> {
        visit.bytes(self.markup.capacity() as u64)?;
        Ok(true)
    }

    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().formatted)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(not_implemented(types().formatted, m).into())
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(not_implemented(types().formatted, m).into())
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(types().formatted, m).into())
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        ex.reserve(self.markup.len() as u64 * 2)?;
        Ok(Some(Str::from(text_of(&self.markup).as_str())))
    }

    fn is_empty_value(&self) -> bool {
        self.markup.is_empty()
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let o = other.as_any().downcast_ref::<Formatted>()?;
        Some(o.markup == self.markup)
    }

    fn boost_hash(&self) -> Option<i32> {
        let u: Vec<u16> = self.markup.encode_utf16().collect();
        Some(bsl_prim::boost_hash::string(&u))
    }

    fn to_shared(&self) -> Option<SharedValue> {
        Some(SharedValue::Object(Arc::new(SharedOf::new(
            self.markup.clone(),
            |m| new_formatted(m.clone()),
        ))))
    }
}

pub(super) struct FormattedCodec;

impl ValueCodec for FormattedCodec {
    fn internal_write(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        w: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        let Some(f) = v.as_any().downcast_ref::<Formatted>() else {
            return Ok(None);
        };
        w.account(ex, f.markup.len() as u64 * 3)?;
        let u: Vec<u16> = f.markup.encode_utf16().collect();
        Ok(Some(vec![Node::int(1), encode_str(&u)]))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        items: &[Node],
        r: &mut InternalReader,
    ) -> Result<Value, Exception> {
        if let Some(text) = items.get(1).and_then(Node::as_str) {
            ex.reserve((text.len() as u64).saturating_mul(4))?;
        }
        match items.get(1).and_then(decode_str) {
            Some(u) => Ok(new_formatted(String::from_utf16_lossy(&u))),
            None => Err(r.format_error(ex)),
        }
    }
}

fn ui_type() -> QName {
    QName::new(NS_UI, "FormattedString")
}

impl XdtoCodec for FormattedCodec {
    fn to_xdto(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        s: &mut XdtoCtx,
    ) -> Result<XdtoData, Exception> {
        let Some(f) = v.as_any().downcast_ref::<Formatted>() else {
            return Err(s.mismatch(ex));
        };
        s.account(ex, f.markup.len() as u64)?;
        Ok(data_text(ui_type(), &f.markup))
    }

    fn from_xdto(
        &self,
        ex: &mut Execution,
        d: &XdtoData,
        s: &mut XdtoCtx,
    ) -> Result<Value, Exception> {
        match d {
            XdtoData::Value { lexical, .. } => {
                ex.reserve(lexical.len() as u64 * 2)?;
                Ok(new_formatted(lexical.clone()))
            }
            _ => Err(s.lexical_error(ex, "", &ui_type())),
        }
    }

    fn xdto_type(&self) -> QName {
        ui_type()
    }
}
