use super::codec::{InternalReader, InternalWriter, ValueCodec};
use crate::values::boundary::{Boundary, BoundaryKind, boundary_parts, new_boundary};
use brace::Node;
use bsl_vm::{Exception, Execution, HostError, ObjRef, Value};
use platform_catalog::TypeIdx;
use std::sync::Arc;

struct BoundaryCodec;

pub(super) fn codecs() -> Vec<(TypeIdx, Arc<dyn ValueCodec>)> {
    vec![(crate::typedesc::idx("Boundary"), Arc::new(BoundaryCodec))]
}

fn unsupported(ex: &mut Execution, operation: &str) -> Exception {
    ex.error(HostError::not_implemented("Граница", operation))
}

fn kind_code(kind: BoundaryKind) -> i64 {
    match kind {
        BoundaryKind::Including => 0,
        BoundaryKind::Excluding => 1,
    }
}

pub(crate) fn read_xml_data(
    ex: &mut Execution,
    data: &crate::xdto::XdtoData,
    ctx: &mut crate::xdto::XdtoCtx,
) -> Result<Value, Exception> {
    use crate::xdto::{ScalarProjection, XdtoData};

    ctx.account(ex, 2048)?;
    let XdtoData::Object { ty, props } = data else {
        return Err(ctx.mismatch(ex));
    };
    let [(value_name, values), (kind_name, kinds)] = props.as_slice() else {
        return Err(ctx.mismatch(ex));
    };
    if ty.ns != "http://v8.1c.ru/8.1/data/enterprise"
        || ty.name != "Bound"
        || value_name != "value"
        || kind_name != "kind"
    {
        return Err(ctx.mismatch(ex));
    }
    let [
        XdtoData::Value {
            ty: date_type,
            value: Value::Date(date),
            ..
        },
    ] = values.as_slice()
    else {
        return Err(ctx.mismatch(ex));
    };
    let [
        XdtoData::Value {
            ty: kind_type,
            lexical,
            projection,
            value: Value::Number(number),
        },
    ] = kinds.as_slice()
    else {
        return Err(ctx.mismatch(ex));
    };
    if date_type.ns != crate::xdto::XS
        || date_type.name != "dateTime"
        || kind_type.ns != crate::xdto::XS
        || kind_type.name != "decimal"
    {
        return Err(ctx.mismatch(ex));
    }
    let mut bytes = props
        .capacity()
        .checked_mul(std::mem::size_of::<(String, Vec<XdtoData>)>())
        .and_then(|n| n.checked_add(ty.ns.capacity()))
        .and_then(|n| n.checked_add(ty.name.capacity()))
        .ok_or_else(|| ex.out_of_memory())?;
    for (name, items) in props {
        let XdtoData::Value {
            ty,
            lexical,
            projection,
            ..
        } = &items[0]
        else {
            return Err(ctx.mismatch(ex));
        };
        let source = match projection {
            Some(ScalarProjection::Read { source_text, .. }) => source_text.capacity(),
            _ => 0,
        };
        bytes = items
            .capacity()
            .checked_mul(std::mem::size_of::<XdtoData>())
            .and_then(|n| n.checked_add(bytes))
            .and_then(|n| n.checked_add(name.capacity()))
            .and_then(|n| n.checked_add(ty.ns.capacity()))
            .and_then(|n| n.checked_add(ty.name.capacity()))
            .and_then(|n| n.checked_add(lexical.capacity()))
            .and_then(|n| n.checked_add(source))
            .ok_or_else(|| ex.out_of_memory())?;
    }
    let bytes = number
        .backing_capacity_limbs()
        .checked_mul(8)
        .and_then(|n| n.checked_add(bytes))
        .ok_or_else(|| ex.out_of_memory())?;
    ctx.account(ex, u64::try_from(bytes).map_err(|_| ex.out_of_memory())?)?;
    let code = match lexical.as_str() {
        "0" => 0,
        "1" => 1,
        _ => return Err(ctx.mismatch(ex)),
    };
    if *number != bsl_prim::Number::from(code)
        || matches!(projection, Some(ScalarProjection::Read { source_text, .. }) if source_text != lexical)
    {
        return Err(ctx.mismatch(ex));
    }
    new_boundary(
        ex,
        &Value::Date(*date),
        if code == 0 {
            BoundaryKind::Including
        } else {
            BoundaryKind::Excluding
        },
    )
}

impl ValueCodec for BoundaryCodec {
    fn internal_write(
        &self,
        ex: &mut Execution,
        value: &ObjRef,
        writer: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        writer.account(ex, 512)?;
        let Some((value, kind)) = boundary_parts(ex, &Value::Object(value.clone()))? else {
            return Err(unsupported(ex, "Внутренняя запись"));
        };
        match &value {
            Value::Date(_) => {}
            Value::Number(number) => {
                writer.account(
                    ex,
                    number
                        .size_hint_limbs()
                        .saturating_mul(64)
                        .saturating_add(256),
                )?;
            }
            _ => return Err(unsupported(ex, "Внутренняя запись значения")),
        }
        let nested = writer.write_value(ex, &value)?;
        Ok(Some(vec![Node::List(vec![
            Node::int(0),
            Node::int(kind_code(kind)),
            nested,
        ])]))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        items: &[Node],
        reader: &mut InternalReader,
    ) -> Result<Value, Exception> {
        if items.is_empty() {
            return Err(unsupported(ex, "Внутреннее чтение"));
        }
        let [Node::List(parts)] = items else {
            return Err(reader.format_error(ex));
        };
        let [Node::Atom(version), Node::Atom(kind), nested] = parts.as_slice() else {
            return Err(reader.format_error(ex));
        };
        if version != "0" {
            return Err(reader.format_error(ex));
        }
        let kind = match kind.as_str() {
            "0" => BoundaryKind::Including,
            "1" => BoundaryKind::Excluding,
            _ => return Err(reader.format_error(ex)),
        };
        let Some([Node::Str(tag), Node::Atom(atom)]) = nested.as_list() else {
            return Err(reader.format_error(ex));
        };
        if !matches!(tag.as_str(), "D" | "N") {
            return Err(reader.format_error(ex));
        }
        let len = u64::try_from(atom.len()).map_err(|_| ex.out_of_memory())?;
        let scratch = len
            .checked_mul(64)
            .and_then(|n| n.checked_add(512))
            .ok_or_else(|| ex.out_of_memory())?;
        ex.charge(1 + len / 64)?;
        ex.reserve(scratch)?;
        let value = reader.read_value(ex, nested)?;
        new_boundary(ex, &value, kind)
    }

    fn storable(&self) -> bool {
        false
    }
}

pub(crate) fn xml_data(
    ex: &mut Execution,
    value: &ObjRef,
    ctx: &mut crate::xdto::XdtoCtx,
) -> Result<crate::xdto::XdtoData, Exception> {
    use bsl_lib::xdto::QName;

    if !value.as_any().is::<Boundary>() {
        return Err(ctx.mismatch(ex));
    }
    ctx.account(ex, 2048)?;
    let Some((value, kind)) = boundary_parts(ex, &Value::Object(value.clone()))? else {
        return Err(ctx.mismatch(ex));
    };
    if !matches!(value, Value::Date(_)) {
        return Err(ctx.fail(HostError::not_implemented("Граница", "Запись XML значения")));
    }
    let value = ctx.to_data(ex, &value)?;
    let kind = ctx.to_data(ex, &Value::from(kind_code(kind)))?;
    Ok(crate::xdto::XdtoData::Object {
        ty: QName::new("http://v8.1c.ru/8.1/data/enterprise", "Bound"),
        props: vec![
            ("value".to_string(), vec![value]),
            ("kind".to_string(), vec![kind]),
        ],
    })
}
