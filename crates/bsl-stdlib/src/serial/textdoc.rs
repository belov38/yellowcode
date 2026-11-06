use super::codec::{InternalReader, InternalWriter, ValueCodec};
use crate::text::{TextDocument, text_types};
use brace::Node;
use bsl_lib::{
    codec::{self, Encoding},
    textio::{reader_all, writer_text},
};
use bsl_vm::{Exception, Execution, ObjRef, Value};
use platform_catalog::TypeIdx;
use std::sync::Arc;

struct TextDocumentCodec;

pub(crate) fn codecs() -> Vec<(TypeIdx, Arc<dyn ValueCodec>)> {
    vec![(text_types().document, Arc::new(TextDocumentCodec))]
}

impl ValueCodec for TextDocumentCodec {
    fn internal_write(
        &self,
        ex: &mut Execution,
        value: &ObjRef,
        writer: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        let Some(document) = value.as_any().downcast_ref::<TextDocument>() else {
            return Ok(None);
        };
        let text = document.serial_text(ex)?;
        let bytes = text
            .len()
            .checked_mul(64)
            .and_then(|n| n.checked_add(256))
            .ok_or_else(|| ex.out_of_memory())?;
        let bytes = u64::try_from(bytes).map_err(|_| ex.out_of_memory())?;
        ex.reserve(bytes)?;
        let tree_bytes = text
            .len()
            .checked_mul(9)
            .and_then(|n| n.checked_add(32))
            .ok_or_else(|| ex.out_of_memory())?;
        writer.account(
            ex,
            u64::try_from(tree_bytes).map_err(|_| ex.out_of_memory())?,
        )?;
        let normalized = writer_text(&reader_all(&text), &[0x0D, 0x0A]);
        let encoded = codec::string_to_bytes(&normalized, Encoding::Utf8, true);
        let payload = brace::base64_atom(&encoded, brace::Newline::Lf, true);
        Ok(Some(vec![Node::List(vec![
            Node::int(0),
            Node::List(vec![payload]),
        ])]))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        items: &[Node],
        reader: &mut InternalReader,
    ) -> Result<Value, Exception> {
        let atom = match items {
            [Node::List(data)] => match data.as_slice() {
                [Node::Atom(version), Node::List(payload)] if version == "0" => {
                    match payload.as_slice() {
                        [Node::Atom(atom)] if atom.starts_with("#base64:") => atom,
                        _ => return Err(reader.format_error(ex)),
                    }
                }
                _ => return Err(reader.format_error(ex)),
            },
            _ => return Err(reader.format_error(ex)),
        };
        let bytes = atom
            .len()
            .checked_mul(64)
            .and_then(|n| n.checked_add(256))
            .ok_or_else(|| ex.out_of_memory())?;
        ex.reserve(u64::try_from(bytes).map_err(|_| ex.out_of_memory())?)?;
        ex.charge(1 + atom.len() as u64 / 64)?;
        let decoded = Node::atom(atom)
            .base64()
            .ok_or_else(|| reader.format_error(ex))?;
        let body = decoded
            .strip_prefix(codec::bom(Encoding::Utf8))
            .ok_or_else(|| reader.format_error(ex))?;
        let text = std::str::from_utf8(body).map_err(|_| reader.format_error(ex))?;
        let units: Vec<u16> = text.encode_utf16().collect();
        TextDocument::from_serial_text(ex, reader_all(&units))
    }
}

pub(crate) fn xml_data(
    ex: &mut Execution,
    document: &TextDocument,
    ctx: &mut crate::xdto::XdtoCtx,
) -> Result<crate::xdto::XdtoData, Exception> {
    use crate::xdto::XdtoData;
    use bsl_lib::xdto::QName;
    use bsl_prim::Str;

    let units = document.serial_text(ex)?;
    let bytes = units
        .len()
        .checked_mul(128)
        .and_then(|n| n.checked_add(2048))
        .ok_or_else(|| ex.out_of_memory())?;
    ctx.account(ex, u64::try_from(bytes).map_err(|_| ex.out_of_memory())?)?;
    let invalid = bsl_lib::xml::xml_invalid_pos(&units, false);
    if invalid != 0 {
        return Err(ctx.fail(crate::xml::bad_char_error(
            invalid as usize - 1,
            &crate::xml::shown_text(&units),
        )));
    }
    let lexical = String::from_utf16(&units).map_err(|_| ex.out_of_memory())?;
    Ok(XdtoData::Object {
        ty: QName::new("http://v8.1c.ru/8.1/data/txtedt", "TextDocument"),
        props: vec![(
            "content".to_string(),
            vec![XdtoData::Value {
                projection: None,
                ty: QName::new("http://www.w3.org/2001/XMLSchema", "string"),
                lexical,
                value: Value::String(Str::from_vec(units)),
            }],
        )],
    })
}

pub(crate) fn read_xml_data(
    ex: &mut Execution,
    data: &crate::xdto::XdtoData,
    ctx: &mut crate::xdto::XdtoCtx,
) -> Result<Value, Exception> {
    use crate::xdto::{ScalarProjection, XdtoData};

    let XdtoData::Object { ty, props } = data else {
        return Err(ctx.mismatch(ex));
    };
    ex.charge(1)?;
    if ty.ns != "http://v8.1c.ru/8.1/data/txtedt" || ty.name != "TextDocument" {
        return Err(ctx.mismatch(ex));
    }
    let mut bytes = props
        .capacity()
        .checked_mul(std::mem::size_of::<(String, Vec<XdtoData>)>())
        .and_then(|n| n.checked_add(std::mem::size_of::<XdtoData>()))
        .and_then(|n| n.checked_add(ty.ns.capacity()))
        .and_then(|n| n.checked_add(ty.name.capacity()))
        .ok_or_else(|| ex.out_of_memory())?;
    let text = match props.as_slice() {
        [] => None,
        [(name, values)] if name == "content" => match values.as_slice() {
            [
                XdtoData::Value {
                    projection,
                    ty,
                    lexical,
                    value: Value::String(text),
                },
            ] if ty.ns == "http://www.w3.org/2001/XMLSchema" && ty.name == "string" => {
                let source = match projection {
                    Some(ScalarProjection::Read { source_text, .. }) => source_text.capacity(),
                    Some(ScalarProjection::Host { .. }) | None => 0,
                };
                bytes = values
                    .capacity()
                    .checked_mul(std::mem::size_of::<XdtoData>())
                    .and_then(|n| n.checked_add(bytes))
                    .and_then(|n| n.checked_add(name.capacity()))
                    .and_then(|n| n.checked_add(ty.ns.capacity()))
                    .and_then(|n| n.checked_add(ty.name.capacity()))
                    .and_then(|n| n.checked_add(lexical.capacity()))
                    .and_then(|n| n.checked_add(source))
                    .and_then(|n| n.checked_add(text.backing_capacity_units().checked_mul(2)?))
                    .and_then(|n| {
                        n.checked_add(
                            std::mem::size_of::<Vec<u16>>() + 2 * std::mem::size_of::<usize>(),
                        )
                    })
                    .and_then(|n| n.checked_add(text.len().checked_mul(64)?))
                    .ok_or_else(|| ex.out_of_memory())?;
                Some(text.units())
            }
            _ => return Err(ctx.mismatch(ex)),
        },
        _ => return Err(ctx.mismatch(ex)),
    };
    let bytes = bytes.checked_add(2048).ok_or_else(|| ex.out_of_memory())?;
    ctx.account(ex, u64::try_from(bytes).map_err(|_| ex.out_of_memory())?)?;
    if let Some(text) = text {
        ex.charge(1 + text.len() as u64 / 64)?;
        if bsl_lib::xml::xml_invalid_pos(text, false) != 0 {
            return Err(ctx.mismatch(ex));
        }
    }
    TextDocument::from_xml_text(ex, text.unwrap_or_default())
}
