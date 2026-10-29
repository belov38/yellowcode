use md_model::{Child, ChildTag, MdKind};

use super::common::{config_separators, is_separator};
use super::ctx::{Ctx, Props};
use super::{Names, TableBuilder, TableRole, fields, index};
use crate::error::{Error, Result};
use crate::logical::{Field, Index, Table, TableKind, VAR};
use crate::names::{field as f, purpose};
use crate::values::Uuid16;

fn builder(cx: &Ctx, name: String, number: u32) -> TableBuilder {
    TableBuilder::new(
        Table::new(name, TableKind::Main, number, ""),
        "",
        cx.obj.id,
        &cx.full_name,
        "",
    )
}

fn numbered(cx: &Ctx, names: &mut Names, id: Uuid16, purpose: &str) -> Result<TableBuilder> {
    let number = names.number(id, purpose)?;
    Ok(builder(cx, format!("{purpose}{number}"), number))
}

fn binary_nullable(name: &str) -> Field {
    let mut field = fields::binary(name, VAR);
    field.nullable = true;
    field
}

fn ix(name: &str, unique: bool, keys: &[&str], clustered: bool) -> Index {
    index::index(
        name,
        unique,
        keys.iter().map(|k| (*k).to_string()).collect(),
        (clustered, false),
    )
}

pub(crate) fn external_data_source(cx: &Ctx, names: &mut Names) -> Result<Vec<(Table, TableRole)>> {
    let sources = cx.md.of_kind(MdKind::ExternalDataSource);
    let shared = names.get(Uuid16::NIL, purpose::EXT_DATA_SRC_PRMS);
    let legacy = sources
        .iter()
        .any(|s| names.get(s.id, purpose::EXT_DATA_SRC_PRMS).is_some());
    if legacy && shared.is_some() {
        return Err(Error::Unsupported(
            "mixed shared and source-keyed external parameters require native evidence".into(),
        ));
    }
    let applied = cx.md.common.applied(&cx.full_name);
    let separator_ids: Vec<_> = applied
        .iter()
        .filter(|a| is_separator(a))
        .map(|a| a.id)
        .collect();
    if !legacy {
        if !separator_ids.is_empty() {
            return Err(Error::Unsupported(
                "shared external parameters with separation require native evidence".into(),
            ));
        }
        if sources.first().is_some_and(|s| s.id != cx.obj.id) {
            return Ok(Vec::new());
        }
        let number = names.number(Uuid16::NIL, purpose::EXT_DATA_SRC_PRMS)?;
        let mut b = builder(cx, purpose::EXT_DATA_SRC_PRMS.to_string(), number);
        b.table.fields = vec![
            fields::binary(f::USER_ID, 16),
            fields::binary(f::EXT_DATA_SRC_ID, 16),
            binary_nullable(f::CONNECT_PRMS),
        ];
        b.table.indexes = vec![ix(
            "ByUsrExtDataSrc",
            true,
            &[f::USER_ID, f::EXT_DATA_SRC_ID],
            true,
        )];
        return Ok(vec![b.finish()]);
    }
    if names.get(cx.obj.id, purpose::EXT_DATA_SRC_PRMS).is_none() || separator_ids.is_empty() {
        return Err(Error::Unsupported("source-keyed external parameters without an existing number and separation require native evidence".into()));
    }
    let separators: Vec<_> = config_separators(cx.md, names)?
        .into_iter()
        .filter(|s| separator_ids.contains(&s.id))
        .collect();
    if separators.iter().any(|s| !s.independent) {
        return Err(Error::Unsupported(
            "source-keyed external parameters with simultaneous separation require native evidence"
                .into(),
        ));
    }
    let mut b = numbered(cx, names, cx.obj.id, purpose::EXT_DATA_SRC_PRMS)?;
    b.table.fields = vec![
        fields::binary(f::USER_ID, 16),
        binary_nullable(f::CONNECT_PRMS),
    ];
    let mut keys = Vec::new();
    for s in &separators {
        let n = names.number(s.id, purpose::DATA_SEPARATION_USE)?;
        let name = format!("{}{n}", purpose::DATA_SEPARATION_USE);
        b.table.fields.push(fields::boolean(&name));
        keys.push(name);
    }
    let mut splitters = Vec::new();
    for s in separators {
        splitters.push(s.fld.clone());
        b.item(fields::field(&s.fld, false, s.types), s.id, s.md_name);
    }
    b.table.splitters = [vec![splitters.clone()], vec![splitters]];
    keys.push(f::USER_ID.to_string());
    b.table.indexes = vec![index::index("ByUsr", true, keys, (true, false))];
    Ok(vec![b.finish()])
}

fn receives(channel: &Child, cx: &Ctx, names: &Names) -> Result<bool> {
    match channel.text("MessageDirection") {
        Some("Receive") => Ok(true),
        Some("Send") => Ok(false),
        None => match (
            names.get(channel.id, purpose::INTEG_CHANNEL_IN_QUEUE),
            names.get(channel.id, purpose::INTEG_CHANNEL_OUT_QUEUE),
        ) {
            (Some(_), None) => Ok(true),
            (None, Some(_)) => Ok(false),
            _ => Err(direction_error(channel, cx)),
        },
        Some(_) => Err(direction_error(channel, cx)),
    }
}

fn direction_error(channel: &Child, cx: &Ctx) -> Error {
    Error::Unsupported(format!(
        "{}: MessageDirection is absent, unknown, or has ambiguous queue reservations",
        cx.child_name(channel)
    ))
}

pub(crate) fn integration_service(cx: &Ctx, names: &mut Names) -> Result<Vec<(Table, TableRole)>> {
    let channels = cx
        .obj
        .children
        .iter()
        .filter(|c| c.tag == ChildTag::IntegrationServiceChannel)
        .map(|c| receives(c, cx, names).map(|receive| (c, receive)))
        .collect::<Result<Vec<_>>>()?;
    let mut settings = numbered(cx, names, cx.obj.id, purpose::INTEG_SERVICE_SETTINGS)?;
    settings.table.fields = vec![fields::boolean(f::ACTIVE), binary_nullable(f::CONTENT)];
    let mut body = numbered(cx, names, cx.obj.id, purpose::INTEG_SERVICE_MSG_BODY)?;
    body.table.fields = vec![
        fields::binary(f::MESSAGE_BODY_ID, 16),
        fields::numeric(f::MESSAGE_BODY_PART_POSITION, 10, 0),
        fields::binary(f::MESSAGE_BODY_PART, VAR),
        fields::datetime(f::EXPIRATION_DATE),
    ];
    body.table.indexes = vec![
        ix(
            "MessageBodyIdMessageBodyPartPosition",
            true,
            &[f::MESSAGE_BODY_ID, f::MESSAGE_BODY_PART_POSITION],
            true,
        ),
        ix("ExpirationDate", false, &[f::EXPIRATION_DATE], false),
    ];
    let mut external_body = numbered(cx, names, cx.obj.id, purpose::INTEG_SERVICE_EXT_MSG_BODY)?;
    external_body.table.fields = vec![
        fields::string(f::MESSAGE_BODY_KEY, 512),
        fields::datetime(f::EXPIRATION_DATE),
    ];
    external_body.table.indexes = vec![
        ix("MessageBodyKey", true, &[f::MESSAGE_BODY_KEY], true),
        ix("ExpirationDate", false, &[f::EXPIRATION_DATE], false),
    ];
    let mut out = vec![settings.finish(), body.finish(), external_body.finish()];
    for receive in [true, false] {
        for &(channel, _) in channels.iter().filter(|(_, r)| *r == receive) {
            out.push(queue(cx, names, channel, receive)?);
        }
    }
    Ok(out)
}

fn queue(
    cx: &Ctx,
    names: &mut Names,
    channel: &Child,
    receive: bool,
) -> Result<(Table, TableRole)> {
    let purpose = if receive {
        purpose::INTEG_CHANNEL_IN_QUEUE
    } else {
        purpose::INTEG_CHANNEL_OUT_QUEUE
    };
    let mut b = numbered(cx, names, channel.id, purpose)?;
    b.role.md = channel.id;
    b.role.md_name = cx.child_name(channel);
    b.table.fields = vec![
        fields::binary(f::MESSAGE_ID, 16),
        fields::numeric(f::POSITION, 10, 0),
        fields::datetime(f::EXPIRATION_DATE),
    ];
    if receive {
        b.table.fields.push(fields::boolean(f::PROCESSED));
        b.table.fields.push(binary_nullable(f::MESSAGE_HEADER));
        b.table.indexes = vec![
            ix("MessageId", true, &[f::MESSAGE_ID], true),
            ix("Position", true, &[f::PROCESSED, f::POSITION], false),
        ];
    } else {
        b.table.fields.push(fields::binary(f::MESSAGE_HEADER, VAR));
        b.table.indexes = vec![ix("Position", true, &[f::POSITION], true)];
    }
    b.table.fields.push(binary_nullable(f::MESSAGE_BODY));
    Ok(b.finish())
}
