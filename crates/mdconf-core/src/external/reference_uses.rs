use crate::objects::codecs::{Ctx, Owner};
use crate::{Error, Result};
use brace::{Guid, Node};
use mdschema::Codec;
use memory_budget::{Category, LimitReason, MemoryLimit};

#[derive(Clone, Copy)]
enum Copies {
    Name,
    Constructed,
    XmlName,
    TypedValue,
    XmlTypedValue,
    Field,
    XmlPath,
    Characteristics,
}
fn add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b).ok_or(Error::Memory(MemoryLimit {
        category: Category::Metadata,
        requested: u64::MAX,
        reason: LimitReason::Overflow,
    }))
}
fn formatted(bytes: u64) -> Result<u64> {
    add(bytes, bytes)
}
fn copies(bytes: u64, consumer: Copies) -> Result<u64> {
    let borrowed_fact_clone = bytes;
    match consumer {
        Copies::Name | Copies::Field => Ok(borrowed_fact_clone),
        Copies::Constructed => add(borrowed_fact_clone, formatted(bytes)?),
        Copies::XmlName | Copies::XmlPath => {
            let constructed_path = formatted(bytes)?;
            let xml_text_clone = bytes;
            add(add(borrowed_fact_clone, constructed_path)?, xml_text_clone)
        }
        Copies::Characteristics => {
            let mut fields = 0;
            for _ in [
                "KeyField",
                "TypesFilterField",
                "DataPathField",
                "MultipleValuesUseField",
                "ObjectField",
                "TypeField",
                "ValueField",
                "MultipleValuesKeyField",
                "MultipleValuesOrderField",
            ] {
                fields = add(fields, add(formatted(bytes)?, bytes)?)?;
            }
            let source_attribute = bytes;
            add(add(borrowed_fact_clone, fields)?, source_attribute)
        }
        Copies::TypedValue | Copies::XmlTypedValue => {
            let value = add(bytes, ".EmptyRef".len() as u64)?;
            let constructed_value = formatted(value)?;
            let xml_text_clone = if matches!(consumer, Copies::XmlTypedValue) {
                value
            } else {
                0
            };
            add(add(borrowed_fact_clone, constructed_value)?, xml_text_clone)
        }
    }
}

pub(crate) fn reference_use_bytes(
    ctx: &Ctx<'_>,
    node: &Node,
    codec: Codec,
    owner: Option<&Owner>,
    check: &dyn Fn() -> Result<()>,
) -> Result<u64> {
    let (consumer, type_fallback, fields) = match codec {
        Codec::Ref | Codec::MdList | Codec::FieldList0 | Codec::FoContent | Codec::CmdGroup => {
            (Copies::Name, false, false)
        }
        Codec::Method => (Copies::Constructed, false, false),
        Codec::Picture | Codec::CaContent => (Copies::XmlName, false, false),
        Codec::FieldList => (Copies::Field, false, true),
        Codec::CpLinks | Codec::LinkByType => (Copies::XmlPath, false, true),
        Codec::TValue => (Copies::TypedValue, true, false),
        Codec::Characteristics => (Copies::Characteristics, true, true),
        Codec::CParams | Codec::StdAttrs | Codec::Unknown => (Copies::XmlTypedValue, true, true),
        _ => {
            check()?;
            return Ok(0);
        }
    };
    fn visit(
        ctx: &Ctx<'_>,
        node: &Node,
        owner: Option<&Owner>,
        (consumer, type_fallback, fields): (Copies, bool, bool),
        check: &dyn Fn() -> Result<()>,
    ) -> Result<u64> {
        check()?;
        let mut bytes = 0;
        if let Some(id) = node.text().and_then(Guid::parse) {
            let name = ctx.index.names.get(&id);
            let ty = type_fallback.then(|| ctx.index.types.get(&id)).flatten();
            let fact = name.or(ty);
            if let Some(fact) = fact {
                bytes = copies(fact.len() as u64, consumer)?;
            }
        }
        if let Some(items) = node.as_list() {
            if fields
                && let Some(owner) = owner
                && let Some(label) = items
                    .first()
                    .and_then(Node::text)
                    .and_then(|id| ctx.schema.std_attr(&owner.kind, id))
            {
                let prefix = add(add(owner.kind.len() as u64, 1)?, owner.name.len() as u64)?;
                let full = add(
                    add(prefix, ".StandardAttribute.".len() as u64)?,
                    label.len() as u64,
                )?;
                let field = formatted(full)?;
                let xml = if matches!(consumer, Copies::XmlPath | Copies::XmlTypedValue) {
                    full
                } else {
                    0
                };
                bytes = add(bytes, add(field, xml)?)?;
            }
            for item in items {
                bytes = add(
                    bytes,
                    visit(ctx, item, owner, (consumer, type_fallback, fields), check)?,
                )?;
            }
        }
        Ok(bytes)
    }
    visit(ctx, node, owner, (consumer, type_fallback, fields), check)
}
