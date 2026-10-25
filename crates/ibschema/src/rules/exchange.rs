use super::catalog::{attributes, main_table, tabular_sections};
use super::common::{common_group, separators_group};
use super::ctx::Ctx;
use super::document::table_of;
use super::fields;
use super::index::Tok;
use super::types::main_target;
use super::{Names, TableBuilder, TableRole};
use crate::error::Result;
use crate::logical::{KeyKind, LType, Table, TableKind, TypeCode};
use crate::names::{StdField, field as f, purpose, role};
use crate::values::Uuid16;

pub(crate) fn predefined_init(
    cx: &Ctx,
    names: &mut Names,
    purpose: &str,
    role: &str,
) -> Result<Option<(Table, TableRole)>> {
    if !cx.predefined {
        return Ok(None);
    }
    let mut b = table_of(cx, names, purpose, role, KeyKind::Plain)?;
    b.role.query_name = String::new();
    b.std(StdField::PdInitialized, fields::pd_initialized());
    separators_group(&mut b, cx, names)?;
    Ok(Some(b.finish()))
}

pub(crate) enum Key {
    Ref,
    Recorder,
    Constant,
    InfoRegister,
}

pub(crate) fn change_records(
    cx: &Ctx,
    names: &mut Names,
    purpose: &str,
    key: Key,
) -> Result<Option<(Table, TableRole)>> {
    if !cx.in_exchange_plan {
        return Ok(None);
    }
    let constant = matches!(key, Key::Constant);
    let rl = if constant {
        role::CONSTANT_CHANGE_RECORDS
    } else {
        role::CHANGE_RECORDS
    };
    let mut b = table_of(cx, names, purpose, rl, KeyKind::Ref)?;
    b.role.query_name = if constant {
        String::new()
    } else {
        format!("{}.Changes", cx.full_name)
    };
    b.std(StdField::Node, fields::node());
    b.std(StdField::MessageNo, fields::message_no());
    let keys: Vec<String> = match key {
        Key::Ref => {
            let own = main_target(cx.obj, names)?;
            b.std(StdField::Id, fields::id_of(&own.table));
            vec![f::ID.to_string()]
        }
        Key::Recorder => {
            b.std(StdField::Recorder, fields::recorder(cx, names)?);
            vec![f::RECORDER.to_string()]
        }
        Key::Constant => {
            b.std(StdField::ConstId, fields::binary(f::CONST_ID, 16));
            vec![f::CONST_ID.to_string()]
        }
        Key::InfoRegister => info_register_key(&mut b, cx, names)?,
    };
    separators_group(&mut b, cx, names)?;
    let node_msg = [
        Tok::F(f::NODE),
        Tok::F(f::MESSAGE_NO),
        Tok::List(keys.clone()),
    ];
    b.rule(cx, names, "ByNodeMsg", true, &node_msg, None)?;
    if !keys.is_empty() {
        let data_key = [Tok::List(keys), Tok::F(f::NODE)];
        b.rule(cx, names, "ByDataKey", true, &data_key, None)?;
    }
    Ok(Some(b.finish()))
}

fn main_filter(v: Option<&md_model::Value>) -> bool {
    !matches!(v, Some(md_model::Value::Bool(false)))
}

fn info_register_key(b: &mut TableBuilder, cx: &Ctx, names: &mut Names) -> Result<Vec<String>> {
    if cx.sub {
        b.std(StdField::Recorder, fields::recorder(cx, names)?);
        return Ok(vec![f::RECORDER.to_string()]);
    }
    let mut keys = Vec::new();
    if cx.periodic && main_filter(cx.obj.prop("MainFilterOnPeriod")) {
        let t = vec![LType::new(TypeCode::T, 0, 0)];
        b.std(StdField::Period, fields::field(f::PERIOD, true, t));
        keys.push(f::PERIOD.to_string());
    }
    for d in cx.obj.dimensions() {
        if !main_filter(d.prop("MainFilter")) {
            continue;
        }
        let at = cx.child_name(d);
        let fl = fields::item(cx, d, &at, true, false, names)?;
        keys.push(fl.name.clone());
        b.item(fl, d.id, at);
    }
    Ok(keys)
}

pub(crate) fn exchange_plan(cx: &Ctx, names: &mut Names) -> Result<Vec<(Table, TableRole)>> {
    use Tok::F;
    let mut b = main_table(cx, names)?;
    b.std(StdField::Version, fields::version());
    b.std(StdField::Marked, fields::marked());
    if cx.code_len > 0 {
        b.std(StdField::Code, fields::code(cx)?);
    }
    if cx.descr_len > 0 {
        b.std(StdField::Description, fields::description(cx.descr_len));
    }
    b.std(StdField::SentNo, fields::numeric(f::SENT_NO, 10, 0));
    b.std(StdField::ReceivedNo, fields::numeric(f::RECEIVED_NO, 10, 0));
    b.std(StdField::PredefinedId, fields::predefined_id());
    b.std(StdField::LastUpdate, fields::datetime(f::LAST_UPDATE));
    attributes(&mut b, cx, names, false)?;
    common_group(&mut b, cx, names)?;

    b.rule(
        cx,
        names,
        "PredefinedID",
        false,
        &[F(f::PREDEFINED_ID)],
        None,
    )?;
    if cx.code_len > 0 {
        b.rule(cx, names, "Code", true, &[F(f::CODE), F(f::ID)], None)?;
    }
    if cx.descr_len > 0 {
        b.rule(
            cx,
            names,
            "Descr",
            true,
            &[F(f::DESCRIPTION), F(f::ID)],
            None,
        )?;
    }
    let mut out = vec![b.finish()];
    out.extend(tabular_sections(cx, names)?);
    Ok(out)
}

pub(crate) fn config_change_records(names: &mut Names) -> Result<Table> {
    let number = names.number(Uuid16::NIL, purpose::CONFIG_CHNGR)?;
    let mut t = Table::new(purpose::CONFIG_CHNGR, TableKind::Main, number, "");
    t.key = KeyKind::Ref;
    t.fields = vec![
        fields::node(),
        fields::message_no(),
        fields::binary(f::MD_OBJ_ID, 16),
        fields::id(purpose::CONFIG_CHNGR),
    ];
    let keys = |k: &[&str]| k.iter().map(ToString::to_string).collect::<Vec<_>>();
    t.indexes = vec![
        super::index::index(
            "ByNodeMsg",
            true,
            keys(&[f::NODE, f::MESSAGE_NO, f::MD_OBJ_ID]),
            (false, false),
        ),
        super::index::index(
            "ByDataKey",
            true,
            keys(&[f::MD_OBJ_ID, f::NODE]),
            (false, false),
        ),
    ];
    let mut props = Table::new(
        purpose::EXT_PROPS,
        TableKind::Nested,
        0,
        purpose::CONFIG_CHNGR,
    );
    props.fields = vec![fields::string(f::FILE_NAME, 128)];
    t.nested = vec![props];
    Ok(t)
}
