mod data;
mod factory;
mod io_json;
mod io_xml;
mod objects;
mod serializer;
mod types;
mod value_map;

#[allow(unused_imports)]
pub(crate) use data::XdtoWriteFlags;
pub use data::{ScalarProjection, XdtoData};
#[allow(unused_imports)]
pub(crate) use io_json::{read_json, write_json};
#[allow(unused_imports)]
pub(crate) use io_xml::{ReadMode, read_xml, read_xml_mode, write_xml};
pub(crate) use types::comparison_input_len;
pub(crate) use value_map::core;
pub use value_map::{ReservedXdtoName, XdtoCodec, XdtoCtx, register_xdto_codec};

use crate::bind::{key_error, msg, with_other};
use crate::enums::type_by_id;
use bsl_lib::xdto::{Form, LexError, Model, ObjectType, Property, QName, TypeDef, ValueType};
use bsl_vm::{Cause, Execution, HostError, TypeIdx};
use platform_catalog::messages::{Lang, render_key};
use std::sync::OnceLock;

pub(crate) const XS: &str = crate::xml::NS_XS;

pub(crate) struct Types {
    pub factory: TypeIdx,
    pub value: TypeIdx,
    pub object: TypeIdx,
    pub list: TypeIdx,
    pub value_type: TypeIdx,
    pub object_type: TypeIdx,
    pub property: TypeIdx,
    pub properties: TypeIdx,
    pub package: TypeIdx,
    pub packages: TypeIdx,
    pub form: TypeIdx,
    pub serializer: TypeIdx,
}

#[allow(
    clippy::panic,
    reason = "the embedded catalog has every type listed here"
)]
pub(crate) fn types() -> &'static Types {
    static T: OnceLock<Types> = OnceLock::new();
    T.get_or_init(|| {
        let t = |id: &str| type_by_id(id).unwrap_or_else(|| panic!("catalog type {id}"));
        Types {
            factory: t("XDTOFactory"),
            value: t("XDTODataValue"),
            object: t("XDTODataObject"),
            list: t("XDTOList"),
            value_type: t("XDTOValueType"),
            object_type: t("XDTOObjectType"),
            property: t("XDTOProperty"),
            properties: t("XDTOPropertyCollection"),
            package: t("XDTOPackage"),
            packages: t("XDTOPackageCollection"),
            form: t("XMLForm"),
            serializer: t("XDTOSerializer"),
        }
    })
}

pub(crate) fn register(reg: &mut bsl_vm::Registry) {
    factory::register(reg);
    types::register(reg);
    serializer::register(reg);
}

const MAX_BASES: usize = 64;

#[derive(Clone)]
pub(crate) struct EProp {
    pub prop: Property,
    pub owner_ns: String,
}

impl EProp {
    pub(crate) fn xml_name(&self) -> &str {
        self.prop.local_name.as_deref().unwrap_or(&self.prop.name)
    }

    pub(crate) fn json_name(&self) -> &str {
        if self.prop.form == Form::Text {
            &self.prop.name
        } else {
            self.xml_name()
        }
    }

    pub(crate) fn xml_ns(&self) -> Option<&str> {
        (self.prop.qualified && !self.owner_ns.is_empty()).then_some(self.owner_ns.as_str())
    }

    pub(crate) fn is_list(&self) -> bool {
        self.prop.upper != 1
    }
}

pub(crate) fn object_type<'m>(m: &'m Model, q: &QName) -> Option<&'m ObjectType> {
    match m.type_def(q)? {
        TypeDef::Object(o) => Some(o),
        TypeDef::Value(_) => None,
    }
}

pub(crate) fn value_type<'m>(m: &'m Model, q: &QName) -> Option<&'m ValueType> {
    match m.type_def(q)? {
        TypeDef::Value(v) => Some(v),
        TypeDef::Object(_) => None,
    }
}

pub(crate) fn any_type() -> QName {
    QName::new(XS, "anyType")
}

pub(crate) fn is_any(q: &QName) -> bool {
    q.ns == XS && q.name == "anyType"
}

pub(crate) fn effective_props(m: &Model, q: &QName) -> Vec<EProp> {
    let mut chain: Vec<&ObjectType> = Vec::new();
    let mut at = Some(q.clone());
    while let Some(t) = at {
        if chain.len() >= MAX_BASES {
            break;
        }
        match object_type(m, &t) {
            Some(o) => {
                at = o.base.clone();
                chain.push(o);
            }
            None => break,
        }
    }
    let mut out = Vec::new();
    for o in chain.into_iter().rev() {
        for p in &o.properties {
            out.push(EProp {
                prop: p.clone(),
                owner_ns: o.name.ns.clone(),
            });
        }
    }
    out
}

#[derive(Default)]
pub(crate) struct PropCache {
    map: std::cell::RefCell<std::collections::HashMap<QName, std::sync::Arc<Vec<EProp>>>>,
}

impl PropCache {
    pub(crate) fn get(
        &self,
        ex: &mut Execution,
        m: &Model,
        q: &QName,
    ) -> bsl_vm::CallResult<std::sync::Arc<Vec<EProp>>> {
        if let Some(p) = self.map.borrow().get(q) {
            return Ok(p.clone());
        }
        let p = std::sync::Arc::new(effective_props(m, q));
        ex.reserve((p.len() as u64 + 1).saturating_mul(512))?;
        self.map.borrow_mut().insert(q.clone(), p.clone());
        Ok(p)
    }

    pub(crate) fn peek(&self, m: &Model, q: &QName) -> std::sync::Arc<Vec<EProp>> {
        self.map
            .borrow_mut()
            .entry(q.clone())
            .or_insert_with(|| std::sync::Arc::new(effective_props(m, q)))
            .clone()
    }
}

pub(crate) fn is_descendant(m: &Model, sub: &QName, sup: &QName) -> bool {
    if is_any(sup) {
        return true;
    }
    let mut at = Some(sub.clone());
    for _ in 0..=MAX_BASES {
        match at {
            Some(t) if &t == sup => return true,
            Some(t) => at = m.type_def(&t).and_then(|d| d.base().cloned()),
            None => return false,
        }
    }
    false
}

pub(crate) fn builtin_root(m: &Model, q: &QName) -> Option<String> {
    let mut at = q.clone();
    for _ in 0..=MAX_BASES {
        if at.ns == XS && !is_any(&at) {
            return Some(at.name);
        }
        at = m.type_def(&at)?.base()?.clone();
    }
    None
}

pub(crate) fn is_qname_type(m: &Model, q: &QName) -> bool {
    if q.ns == "http://v8.1c.ru/8.1/data/core" && q.name == "Type" {
        return true;
    }
    matches!(builtin_root(m, q).as_deref(), Some("QName" | "NOTATION"))
}

pub(crate) fn is_union(m: &Model, q: &QName) -> bool {
    let mut at = Some(q.clone());
    for _ in 0..=MAX_BASES {
        let Some(t) = at else { return false };
        match m.type_def(&t) {
            Some(TypeDef::Value(v)) => {
                if matches!(v.variety, bsl_lib::xdto::Variety::Union { .. }) {
                    return true;
                }
                at = v.base.clone();
            }
            _ => return false,
        }
    }
    false
}

pub(crate) fn is_qname_union(m: &Model, q: &QName) -> bool {
    let mut at = Some(q.clone());
    for _ in 0..=MAX_BASES {
        let Some(t) = at else { return false };
        match m.type_def(&t) {
            Some(TypeDef::Value(v)) => {
                if let bsl_lib::xdto::Variety::Union { members } = &v.variety
                    && members
                        .iter()
                        .any(|x| (x.ns == XS && x.name == "QName") || is_qname_union_flat(m, x))
                {
                    return true;
                }
                at = v.base.clone();
            }
            _ => return false,
        }
    }
    false
}

fn is_qname_union_flat(m: &Model, q: &QName) -> bool {
    match m.type_def(q) {
        Some(TypeDef::Value(v)) => matches!(
            &v.variety,
            bsl_lib::xdto::Variety::Union { members }
                if members.iter().any(|x| x.ns == XS && x.name == "QName")
        ),
        _ => false,
    }
}

pub(crate) fn takes_without_qname(m: &Model, q: &QName, text: &str) -> bool {
    let mut at = Some(q.clone());
    for _ in 0..=MAX_BASES {
        let Some(t) = at else { return false };
        let Some(TypeDef::Value(v)) = m.type_def(&t) else {
            return false;
        };
        if let bsl_lib::xdto::Variety::Union { members } = &v.variety {
            return members.iter().any(|x| {
                !(is_qname_union_flat(m, x) || x.ns == XS && x.name == "QName")
                    && bsl_lib::xdto::check_lexical(m, x, text).is_ok()
            });
        }
        at = v.base.clone();
    }
    false
}

pub(crate) fn form_name(lang: Lang, f: Form) -> &'static str {
    match (lang, f) {
        (Lang::Ru, Form::Attribute) => "Атрибут",
        (Lang::Ru, Form::Element) => "Элемент",
        (Lang::Ru, Form::Text) => "Текст",
        (Lang::En, Form::Attribute) => "Attribute",
        (Lang::En, Form::Element) => "Element",
        (Lang::En, Form::Text) => "Text",
    }
}

fn other(e: HostError) -> HostError {
    with_other(e, false)
}

fn text_of(ex: &Execution, key: &'static str, args: &[String]) -> String {
    render_key(key, args, "", ex.lang()).unwrap_or_default()
}

pub(crate) fn headed(key: &'static str, body: &str) -> HostError {
    other(key_error(key, Vec::new(), format!(":\n{body}")))
}

pub(crate) fn unsupported(what: &str) -> HostError {
    other(HostError::not_implemented("ФабрикаXDTO", what))
}

pub(crate) fn single_value(ex: &Execution, ep: &EProp, n: usize) -> Result<(), HostError> {
    if ep.is_list() || n <= 1 {
        return Ok(());
    }
    let line = text_for(ex, msg::XDTO_UPPER, &[n.to_string(), "1".to_string()]);
    Err(headed(msg::XDTO_VALIDATION, &line))
}

pub(crate) fn reader_context_chain(ex: &Execution, ctx: Vec<(String, QName)>) -> Option<HostError> {
    let lang = ex.lang();
    let mut err: Option<HostError> = None;
    for (name, ty) in ctx.into_iter().rev() {
        let body = text_for(
            ex,
            msg::XDTO_READER_CONTEXT,
            &[
                text_for(ex, msg::XDTO_START_ITEM, &[]),
                name,
                form_name(lang, Form::Element).to_string(),
                ty.clark(),
            ],
        );
        let mut h = headed(msg::XDTO_SERIALIZATION, &body);
        if let Some(inner) = err.take() {
            h = h.caused_by(Cause::Host(Box::new(inner)));
        }
        err = Some(h);
    }
    err
}

pub(crate) fn plain_error(key: &'static str) -> HostError {
    other(key_error(key, Vec::new(), String::new()))
}

pub(crate) fn args_error(key: &'static str, args: Vec<String>) -> HostError {
    other(key_error(key, args, String::new()))
}

pub(crate) fn check_error(e: LexError) -> HostError {
    headed(msg::XDTO_VALIDATION, &e.lines.join("\n"))
}

pub(crate) fn mismatch_error(e: LexError) -> HostError {
    other(key_error(
        msg::XDTO_TYPE_MISMATCH,
        Vec::new(),
        String::new(),
    ))
    .caused_by(Cause::Host(Box::new(check_error(e))))
}

pub(crate) fn property_block(ex: &Execution, p: &EProp) -> String {
    text_for(
        ex,
        msg::XDTO_CHECK_PROPERTY,
        &[
            p.prop.name.clone(),
            form_name(ex.lang(), p.prop.form).to_string(),
            p.xml_ns().map_or_else(
                || p.xml_name().to_string(),
                |ns| QName::new(ns, p.xml_name()).clark(),
            ),
            p.prop.ty.clark(),
        ],
    )
}

pub(crate) fn text_for(ex: &Execution, key: &'static str, args: &[String]) -> String {
    text_of(ex, key, args)
}

pub(crate) fn structure_error(ex: &Execution, ty: &QName, at: &EProp, expected: &str) -> HostError {
    headed(msg::XDTO_VALIDATION, &structure_body(ex, ty, at, expected))
}

pub(crate) fn structure_body(ex: &Execution, ty: &QName, at: &EProp, expected: &str) -> String {
    format!(
        "{}{}\n{}",
        text_for(ex, msg::XDTO_STRUCTURE, &[ty.clark()]),
        property_block(ex, at),
        expected
    )
}

pub(crate) fn expected_line(ex: &Execution, names: &[String]) -> String {
    text_for(ex, msg::XDTO_EXPECTED, &[names.join(", ")])
}

pub(crate) fn check_structure(
    ex: &Execution,
    ty: &QName,
    props: &[EProp],
    ordered: bool,
    count_of: &dyn Fn(&str) -> usize,
) -> Result<(), Box<(EProp, HostError)>> {
    let mut missing: Option<&EProp> = None;
    for p in props {
        let n = count_of(&p.prop.name);
        if p.prop.form == Form::Attribute {
            if n == 0 && p.prop.lower > 0 {
                let line = expected_line(ex, std::slice::from_ref(&p.prop.name));
                return Err(Box::new((p.clone(), structure_error(ex, ty, p, &line))));
            }
            continue;
        }
        if n == 0 {
            if p.prop.lower > 0 && missing.is_none() {
                missing = Some(p);
            }
            continue;
        }
        if let Some(first) = missing.filter(|_| ordered) {
            let line = expected_line(ex, std::slice::from_ref(&first.prop.name));
            return Err(Box::new((p.clone(), structure_error(ex, ty, p, &line))));
        }
        if p.prop.upper >= 0 && n as i64 > p.prop.upper {
            let line = text_for(
                ex,
                msg::XDTO_UPPER,
                &[n.to_string(), p.prop.upper.to_string()],
            );
            return Err(Box::new((p.clone(), structure_error(ex, ty, p, &line))));
        }
        if (n as u64) < u64::from(p.prop.lower) {
            let line = text_for(
                ex,
                msg::XDTO_LOWER,
                &[n.to_string(), p.prop.lower.to_string()],
            );
            return Err(Box::new((p.clone(), structure_error(ex, ty, p, &line))));
        }
    }
    if let Some(first) = missing {
        let line = expected_line(ex, std::slice::from_ref(&first.prop.name));
        return Err(Box::new((
            first.clone(),
            structure_error(ex, ty, first, &line),
        )));
    }
    Ok(())
}
