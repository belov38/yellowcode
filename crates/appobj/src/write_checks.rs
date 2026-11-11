use crate::codes::{Layout, field_message, open_read, other_error, ref_id};
use crate::errors::{self, keys};
use crate::model::{StdAttr, Usage};
use crate::objects::ObjectInstance;
use crate::read::{read_view, store_call_error};
use crate::txn::CallTxn;
use crate::values::refs::as_ref;
use crate::write_hooks::WriteCtx;
use bsl_prim::fold_name;
use bsl_vm::{CallResult, Execution, HostError, Value};
use ibschema::fields::FieldValue;
use ibschema::names::StdField;
use ibschema::storage::{FieldId, IndexId, TableId};
use ibschema::store::{Direction, ReadTxn, Row};
use ibschema::values::{RefId, Uuid16};

use platform_catalog::messages::Lang;
use std::ops::Bound;

const MAX_CHAIN: usize = 10_000;

fn assigned(inst: &ObjectInstance, i: usize) -> bool {
    inst.state
        .borrow()
        .attributes
        .get(i)
        .is_some_and(Option::is_some)
}

fn refuse(inst: &ObjectInstance, what: &str) -> HostError {
    HostError::not_implemented(&inst.type_name(), &format!("Записать: {what}"))
}

fn plain(key: &'static str) -> HostError {
    other_error(HostError::text(
        &errors::render(key, &[], Lang::Ru),
        &errors::render(key, &[], Lang::En),
    ))
}

fn row_by_id(
    ex: &Execution,
    rd: &dyn ReadTxn,
    table: TableId,
    key: &[FieldValue],
    fields: &[FieldId],
) -> CallResult<Option<Row>> {
    let mut rows = rd
        .range_fields(
            table,
            IndexId::Id,
            Bound::Included(key),
            Bound::Included(key),
            Direction::Forward,
            fields,
        )
        .map_err(|e| store_call_error(ex, e))?;
    match rows.next() {
        Some(item) => Ok(Some(item.map_err(|e| store_call_error(ex, e))?.1)),
        None => Ok(None),
    }
}

pub(crate) fn check_owner(
    ex: &mut Execution,
    ctx: &WriteCtx<'_>,
    layout: &Layout<'_>,
) -> CallResult<()> {
    let Some(i) = layout.owner else {
        return Ok(());
    };
    let inst = ctx.inst;
    if !(ctx.is_new || assigned(inst, i)) {
        return Ok(());
    }
    let v = inst.attr(ex, i)?;
    let Some(owner) = as_ref(&v).filter(|r| !r.is_empty()) else {
        return Err(other_error(field_message(keys::OWNER_EMPTY, &[], StdAttr::Owner)).into());
    };
    let owner_def = layout.app.model().obj(owner.obj);
    let use_ = layout.def.behavior.subordination_use;
    let other = Layout::of(layout.app, owner.obj)?;
    let txn = CallTxn::of_call(ex);
    let mut view = read_view(ex, txn.as_deref(), layout.app)?;
    let _memory = open_read(ex, &mut view)?;
    let key = other.id_key(owner.id)?;
    let fields: Vec<FieldId> = other.column(StdField::Folder).into_iter().collect();
    let Some(row) = row_by_id(ex, view.txn(), other.table, &key, &fields)? else {
        return Err(refuse(inst, "владелец: ссылка на отсутствующий элемент").into());
    };
    if owner_def.behavior.hierarchical
        && owner_def.behavior.hierarchy_folders
        && use_ != Usage::ForFolderAndItem
    {
        let folder = other
            .cell(&row, StdField::Folder)
            .map(|c| matches!(c, FieldValue::Bool(false)));
        if folder.is_some_and(|group| group != (use_ == Usage::ForFolder)) {
            return Err(refuse(
                inst,
                "владелец: использование подчинения (группа или элемент)",
            )
            .into());
        }
    }
    Ok(())
}

pub(crate) fn check_hierarchy(
    ex: &mut Execution,
    ctx: &WriteCtx<'_>,
    layout: &Layout<'_>,
) -> CallResult<()> {
    let Some(pi) = layout.parent else {
        return Ok(());
    };
    let inst = ctx.inst;
    if !(ctx.is_new || assigned(inst, pi)) {
        return Ok(());
    }
    let def = layout.def;
    let own: Option<RefId> = if ctx.is_new {
        None
    } else {
        inst.state.borrow().r.map(|r| r.id)
    };
    let parent = ref_id(&inst.attr(ex, pi)?);
    let owner = match layout.owner {
        Some(oi) => Some(ref_id(&inst.attr(ex, oi)?)),
        None => None,
    };
    let mut level = 0usize;
    if parent != RefId::NIL {
        let txn = CallTxn::of_call(ex);
        let mut view = read_view(ex, txn.as_deref(), layout.app)?;
        let _memory = open_read(ex, &mut view)?;
        let rd = view.txn();
        let fields: Vec<FieldId> = [StdField::Folder, StdField::OwnerId, StdField::ParentId]
            .into_iter()
            .filter_map(|s| layout.column(s))
            .collect();
        let mut cur = parent;
        let mut first = true;
        while cur != RefId::NIL {
            if Some(cur) == own {
                return Err(plain(keys::LEVELS_LOOP).into());
            }
            if level >= MAX_CHAIN {
                return Err(refuse(inst, "родитель: цепочка родителей длиннее допустимой").into());
            }
            ex.charge(1)?;
            let key = layout.id_key(cur)?;
            let Some(row) = row_by_id(ex, rd, layout.table, &key, &fields)? else {
                return Err(refuse(inst, "родитель: ссылка на отсутствующий элемент").into());
            };
            if first {
                first = false;
                if def.behavior.hierarchy_folders
                    && matches!(
                        layout.cell(&row, StdField::Folder),
                        Some(FieldValue::Bool(true))
                    )
                {
                    return Err(other_error(field_message(
                        keys::PARENT_MUST_BE_FOLDER,
                        &[],
                        StdAttr::Parent,
                    ))
                    .into());
                }
                if let (Some(owner), Some(FieldValue::Ref { id, .. })) =
                    (owner, layout.cell(&row, StdField::OwnerId))
                    && owner != *id
                {
                    return Err(refuse(inst, "родитель: другой владелец").into());
                }
            }
            level += 1;
            cur = match layout.cell(&row, StdField::ParentId) {
                Some(FieldValue::Ref { id, .. }) => *id,
                _ => RefId::NIL,
            };
        }
    }
    if let Some(n) = def.behavior.levels {
        let group = def.behavior.hierarchy_folders && inst.folder_flag() == Some(true);
        let top = i64::from(n) - if group { 2 } else { 1 };
        if i64::try_from(level).unwrap_or(i64::MAX) > top {
            return Err(plain(keys::LEVELS_LIMIT).into());
        }
    }
    Ok(())
}

pub(crate) fn clears_predefined_name(inst: &ObjectInstance, i: usize) -> bool {
    let st = inst.state.borrow();
    let now_empty = matches!(
        st.attributes.get(i),
        Some(Some(Value::String(s))) if s.units().is_empty()
    );
    let was = matches!(
        st.row.fields.get(i),
        Some(Value::String(s)) if !s.units().is_empty()
    );
    now_empty && was
}

pub(crate) fn cleared_id() -> FieldValue {
    FieldValue::Uuid(Uuid16::NIL)
}

fn name_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.to_string_lossy(),
        _ => String::new(),
    }
}

pub(crate) fn check_predefined_name(
    ex: &mut Execution,
    ctx: &WriteCtx<'_>,
    layout: &Layout<'_>,
) -> CallResult<()> {
    let Some(i) = layout.predefined_name else {
        return Ok(());
    };
    let inst = ctx.inst;
    if !assigned(inst, i) {
        return Ok(());
    }
    let now = name_of(&inst.attr(ex, i)?);
    let was = name_of(
        &inst
            .state
            .borrow()
            .row
            .fields
            .get(i)
            .cloned()
            .unwrap_or(Value::Undefined),
    );
    if fold_name(&now) == fold_name(&was) {
        return Ok(());
    }
    if now.is_empty() {
        return Ok(());
    }
    let app = layout.app;
    let Some((pid, _)) = app
        .predefined()
        .names(app, layout.obj)
        .into_iter()
        .find(|(_, n)| fold_name(n) == fold_name(&now))
    else {
        return Err(refuse(
            inst,
            "ИмяПредопределенныхДанных: имени нет в данных предопределённых",
        )
        .into());
    };
    let id = RefId(pid).to_uuid();
    let Some((index, ix)) = layout.index(&[StdField::PredefinedId]) else {
        return Err(refuse(
            inst,
            "ИмяПредопределенныхДанных: нет индекса по идентификатору",
        )
        .into());
    };
    let mut key = layout.area_prefix(ix)?;
    key.push(FieldValue::Uuid(id));
    let own = if ctx.is_new {
        None
    } else {
        inst.state.borrow().r.map(|r| r.id)
    };
    let txn = CallTxn::of_call(ex);
    let mut view = read_view(ex, txn.as_deref(), app)?;
    let _memory = open_read(ex, &mut view)?;
    let rd = view.txn();
    let fields: Vec<FieldId> = layout.column(StdField::Id).into_iter().collect();
    let rows = rd
        .range_fields(
            layout.table,
            index,
            Bound::Included(key.as_slice()),
            Bound::Included(key.as_slice()),
            Direction::Forward,
            &fields,
        )
        .map_err(|e| store_call_error(ex, e))?;
    for item in rows {
        ex.charge(1)?;
        let (_, row) = item.map_err(|e| store_call_error(ex, e))?;
        let row_id = match layout.cell(&row, StdField::Id) {
            Some(FieldValue::Ref { id, .. }) => Some(*id),
            _ => None,
        };
        if own.is_none() || row_id != own {
            return Err(other_error(HostError::text(
                &errors::render(keys::PREDEFINED_ITEM_NOT_UNIQUE, &[], Lang::Ru),
                &errors::render(keys::PREDEFINED_ITEM_NOT_UNIQUE, &[], Lang::En),
            ))
            .into());
        }
    }
    Err(refuse(
        inst,
        "ИмяПредопределенныхДанных: присвоение имени, свободного в данных",
    )
    .into())
}
