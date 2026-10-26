use super::codecs::{Ctx, count, slice};
use crate::model::{
    BinaryQualifiers, DateFractions, NumberQualifiers, StringQualifiers, TypeDescription, TypeRef,
    TypedValue,
};
use brace::{Guid, Node};

const T_DTREF: &str = "5c14e26f-099b-4d37-84a6-b433d87400da";
const T_TYPEDESC: &str = "f5c65050-3bbb-11d5-b988-0050bae0a95d";
const T_FIXARR: &str = "4500381b-db30-4a10-9db4-990038032acf";
const ZERO_GUID: &str = "00000000-0000-0000-0000-000000000000";
pub(crate) const SET_CATEGORIES: [&str; 2] = ["DefinedType", "Characteristic"];

fn num(n: &Node) -> Option<u32> {
    n.text()?.trim().parse().ok()
}

fn len_fixed(it: &[Node]) -> Option<(u32, bool)> {
    let length = match it.get(1) {
        Some(n) => num(n)?,
        None => 0,
    };
    let variable = match it.get(2) {
        Some(n) => n.text()? == "1",
        None => true,
    };
    Some((length, !variable))
}

fn simple(qname: &str) -> Option<TypeRef> {
    Some(TypeRef {
        name: crate::XmlTypeName::from_qname(qname)?,
        set: false,
        type_id: None,
    })
}

pub fn type_desc(ctx: &Ctx, pat: &Node) -> Option<TypeDescription> {
    type_desc_with_platform(ctx, pat, |text| {
        ctx.schema
            .platform_type(text)
            .map(|p| mdschema::BorrowedPlatformType {
                name: p.name(),
                set: p.set,
            })
    })
}
fn type_desc_with_platform<'p>(
    ctx: &Ctx,
    pat: &Node,
    platform: impl Fn(&str) -> Option<mdschema::BorrowedPlatformType<'p>>,
) -> Option<TypeDescription> {
    let l = pat.as_list()?;
    if l.first()?.text()? != "Pattern" {
        return None;
    }
    let mut d = TypeDescription {
        types: Vec::new(),
        number: None,
        string: None,
        date: None,
        binary: None,
    };
    for it in l.get(1..)? {
        let it = it.as_list()?;
        match it.first()?.text()? {
            "B" => d.types.push(simple("xs:boolean")?),
            "S" => {
                d.types.push(simple("xs:string")?);
                let (length, fixed) = len_fixed(it)?;
                d.string = Some(StringQualifiers { length, fixed });
            }
            "N" => {
                d.types.push(simple("xs:decimal")?);
                d.number = Some(NumberQualifiers {
                    digits: num(it.get(1)?)?,
                    fraction: num(it.get(2)?)?,
                    nonnegative: it.get(3)?.text()? == "1",
                });
            }
            "D" => {
                d.types.push(simple("xs:dateTime")?);
                d.date = Some(match it.get(1).and_then(Node::text) {
                    Some("D") => DateFractions::Date,
                    Some("T") => DateFractions::Time,
                    _ => DateFractions::DateTime,
                });
            }
            "R" => {
                d.types.push(simple("xs:base64Binary")?);
                let (length, fixed) = len_fixed(it)?;
                d.binary = Some(BinaryQualifiers { length, fixed });
            }
            "#" => {
                let text = it.get(1)?.text()?;
                let g = Guid::parse(text)?;
                let r = if let Some(n) = ctx.index.types.get(&g) {
                    let set = SET_CATEGORIES.contains(&n.split('.').next()?);
                    TypeRef {
                        name: crate::XmlTypeName::new(mdschema::FixedNamespace::Cfg.uri(), n)?,
                        set,
                        type_id: Some(g),
                    }
                } else if text == T_TYPEDESC {
                    simple("v8:TypeDescription")?
                } else {
                    let p = platform(text)?;
                    TypeRef {
                        name: crate::XmlTypeName::new(p.name.namespace_uri, p.name.local)?,
                        set: p.set,
                        type_id: None,
                    }
                };
                d.types.push(r);
            }
            _ => return None,
        }
    }
    Some(d)
}

fn fmt_date(s: &str) -> Option<String> {
    if s.len() != 14 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!(
        "{}-{}-{}T{}:{}:{}",
        &s[0..4],
        &s[4..6],
        &s[6..8],
        &s[8..10],
        &s[10..12],
        &s[12..14]
    ))
}

pub fn design_time_ref(ctx: &Ctx, v: &Node) -> Option<String> {
    let l = v.as_list()?;
    let (ou, vu) = (l.get(1)?.text()?, l.get(2)?.text()?);
    if ou == ZERO_GUID {
        return Some(String::new());
    }
    let og = Guid::parse(ou)?;
    let on = match ctx.index.names.get(&og) {
        Some(n) => n.clone(),
        None => match ctx.index.types.get(&og) {
            Some(t) => {
                let (pref, rest) = t.split_once('.')?;
                format!("{}.{rest}", pref.strip_suffix("Ref").unwrap_or(pref))
            }
            None if vu == ZERO_GUID => return Some(format!("{og}.{ZERO_GUID}")),
            None => return None,
        },
    };
    if vu == ZERO_GUID {
        return Some(format!("{on}.EmptyRef"));
    }
    let vg = Guid::parse(vu)?;
    if let Some(n) = ctx.index.names.get(&vg) {
        return Some(n.clone());
    }
    let pn = ctx.predef.get(&vg).filter(|s| !s.is_empty())?;
    Some(format!("{on}.{pn}"))
}

pub(crate) fn typed_value(ctx: &Ctx, v: &Node) -> Option<TypedValue> {
    let l = v.as_list()?;
    let arg = || l.get(1)?.text();
    match l.first()?.text()? {
        "U" => Some(TypedValue::Nil),
        "S" => Some(TypedValue::Str(super::codecs::cf_text(arg()?).into_owned())),
        "N" => Some(TypedValue::Number(arg()?.to_string())),
        "B" => Some(TypedValue::Bool(arg()? == "1")),
        "D" => Some(TypedValue::Date(fmt_date(arg()?)?)),
        "#" => match arg()? {
            T_DTREF => Some(TypedValue::DesignTimeRef(design_time_ref(ctx, l.get(2)?)?)),
            T_TYPEDESC => match l.get(2)?.as_list()? {
                [p] if p.text() == Some("Pattern") => Some(TypedValue::EmptyTypeDescription),
                _ => None,
            },
            T_FIXARR => {
                let arr = l.get(2)?.as_list()?;
                let n = count(arr.first()?)?;
                let items = slice(arr, 1, n)
                    .iter()
                    .map(|x| typed_value(ctx, x))
                    .collect::<Option<Vec<_>>>()?;
                Some(TypedValue::FixedArray(items))
            }
            _ => None,
        },
        _ => None,
    }
}
