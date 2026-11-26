use crate::Application;
use crate::errors::{self, keys};
use crate::model::{ObjDef, ObjId, ObjKind, ObjStorage, StdAttr};
use crate::objects::{ObjectInstance, WriteStamp};
use crate::predefined::{AreaKey, blank_row};
use crate::read::read_fresh;
use crate::read_budget as budget;
use crate::txn::{CallTxn, Commit, TxnError};
use crate::values::field::to_field;
use crate::values::refs::RefValue;
use crate::write_hooks::{self, DocWrite, DocumentModes, WriteCtx};
use bsl_vm::{CallResult, Category, CategorySet, Execution, HostError, Value};
use ibschema::fields::{FieldValue, encode};
use ibschema::names::StdField;
use ibschema::storage::{IndexId, TableId, TableInfo};
use ibschema::store::{Direction, RowHandle, StoreError, StoreResult, WriteTxn};
use ibschema::values::RefId;
use platform_catalog::messages::Lang;
use std::ops::Bound;
use std::rc::Rc;

fn internal(text: &str) -> HostError {
    HostError::text(&format!("internal: {text}"), &format!("internal: {text}"))
}

fn l5(key: &str) -> HostError {
    match errors::msg(key) {
        Some(m) => HostError::text(m.ru, m.en),
        None => internal(key),
    }
}

pub(crate) fn use_error(key: &str) -> HostError {
    let mut e = l5(key);
    e.categories = Some(CategorySet::of(&[
        Category::AllErrors,
        Category::ScriptRuntimeError,
        Category::ScriptUseError,
    ]));
    e
}

fn other_error(mut e: HostError) -> HostError {
    e.categories = Some(CategorySet::of(&[
        Category::AllErrors,
        Category::ScriptRuntimeError,
        Category::OtherError,
    ]));
    e
}

fn ref_not_unique(table: &str) -> HostError {
    let text = |lang| {
        format!(
            "{}\n{}",
            errors::render(keys::REF_NOT_UNIQUE, &[], lang),
            errors::render(keys::REF_NOT_UNIQUE_TABLE, &[table], lang)
        )
    };
    other_error(HostError::text(&text(Lang::Ru), &text(Lang::En)))
}

struct Plan<'a> {
    app: &'a Application,
    def: &'a ObjDef,
    st: &'a ObjStorage,
    main: &'a TableInfo,
    number: u32,
}

impl<'a> Plan<'a> {
    fn new(app: &'a Application, inst: &ObjectInstance) -> Result<Plan<'a>, HostError> {
        let def = app.model().obj(inst.obj);
        let refuse = |what: &str| {
            HostError::not_implemented(&inst.type_name(), &format!("Записать: {what}"))
        };
        if !matches!(
            def.kind,
            ObjKind::Catalog | ObjKind::Document | ObjKind::ExchangePlan
        ) {
            return Err(refuse("вид объекта"));
        }
        if def.kind == ObjKind::ExchangePlan
            && let Some(what) = node_refusal(app, inst)
        {
            return Err(refuse(what));
        }
        inst.refuse_group_without_hierarchy("Записать")?;
        if app.options().read_only_scope.is_some() {
            return Err(refuse("приложение только для чтения (read_scope)"));
        }
        let st = app
            .model()
            .storage_of(inst.obj)
            .ok_or_else(|| internal("the object has no storage"))?;
        let number = st
            .ref_number
            .ok_or_else(|| internal("the object has no reference table"))?;
        let map = app.store().storage();
        let main = map
            .try_table(st.main)
            .ok_or_else(|| internal("no main table"))?;
        let separated = !main.separators.is_empty()
            || st.tabular.iter().any(|(t, _)| {
                map.try_table(*t)
                    .is_some_and(|ti| !ti.separators.is_empty())
            });
        if separated && app.options().area().is_none() {
            return Err(refuse("данные разделены, область данных не задана"));
        }
        Ok(Plan {
            app,
            def,
            st,
            main,
            number,
        })
    }

    fn key(&self, ti: &TableInfo, id: RefId) -> Result<Vec<FieldValue>, HostError> {
        let mut key = if ti.key_index.as_ref().is_some_and(|k| k.prefix != 0) {
            let scope = self
                .app
                .options()
                .area()
                .ok_or_else(|| internal("separated table without an area"))?;
            scope
                .area(ti)
                .map_err(|_| internal("invalid bound write area"))?
        } else {
            Vec::new()
        };
        key.push(FieldValue::Ref {
            table: self.number,
            id,
        });
        Ok(key)
    }
}

fn node_refusal(app: &Application, inst: &ObjectInstance) -> Option<&'static str> {
    let model = app.model();
    let def = model.obj(inst.obj);
    let flag = |name: &str| {
        model.config.object(def.md).is_none_or(|md| {
            !matches!(md.prop(name), Some(md_model::Value::Bool(false)))
                && !matches!(md.prop(name), Some(md_model::Value::Text(t)) if t == "false")
        })
    };
    if inst.loading() {
        return Some("ОбменДанными.Загрузка узла");
    }
    if flag("DistributedInfoBase") {
        return Some("план обмена распределённой базы");
    }
    if flag("IncludeConfigurationExtensions") {
        return Some("план обмена с данными расширений");
    }
    if crate::history::kept(app, inst.obj) {
        return Some("план обмена с историей данных");
    }
    if def.fields.iter().any(|f| f.std.is_none()) || !def.tabular.is_empty() {
        return Some("план обмена со своими реквизитами или табличными частями");
    }
    let this_node = def
        .fields
        .iter()
        .position(|f| f.std == Some(StdAttr::ThisNode))
        .is_some_and(|i| {
            let st = inst.state.borrow();
            !st.is_new && matches!(st.row.fields.get(i), Some(Value::Bool(true)))
        });
    if this_node {
        return Some("этот узел");
    }
    None
}

struct TabJob {
    table: TableId,
    key: Vec<FieldValue>,
    rows: Vec<Vec<FieldValue>>,
    key_col: Option<usize>,
    line_col: Option<usize>,
}

fn same_cell(a: &FieldValue, b: &FieldValue) -> bool {
    match (a, b) {
        (FieldValue::Number(x), FieldValue::Number(y)) => x.eq_value(y),
        _ => a == b,
    }
}

enum LineWrite {
    Keep,
    Update(RowHandle, Vec<FieldValue>),
    Insert(Vec<FieldValue>),
}

impl TabJob {
    fn grown(&self, stored: &[(RowHandle, Vec<FieldValue>)]) -> Option<Vec<LineWrite>> {
        if stored.len() >= self.rows.len() {
            return None;
        }
        let (line, key) = (self.line_col?, self.key_col?);
        let int = |r: &[FieldValue], c: usize| match r.get(c) {
            Some(FieldValue::Number(n)) => n.to_string().parse::<i64>().ok(),
            _ => None,
        };
        let mut next = stored
            .iter()
            .map(|(_, r)| int(r, key))
            .try_fold(0i64, |m, k| k.map(|k| m.max(k)))?;
        let same = |a: &[FieldValue], b: &[FieldValue]| {
            a.iter()
                .zip(b)
                .enumerate()
                .all(|(i, (x, y))| i == key || same_cell(x, y))
        };
        let mut out = Vec::with_capacity(self.rows.len());
        let mut matched = 0usize;
        for row in &self.rows {
            let old = stored
                .iter()
                .find(|(_, old)| int(old, line).is_some() && int(old, line) == int(row, line));
            if let Some((_, old)) = old
                && same(old, row)
            {
                matched += 1;
                out.push(LineWrite::Keep);
                continue;
            }
            next = next.saturating_add(1);
            let mut row = row.clone();
            if let Some(cell) = row.get_mut(key) {
                *cell = FieldValue::Number(ibschema::Numeric::from_i64(next));
            }
            out.push(match old {
                Some((h, _)) => {
                    matched += 1;
                    LineWrite::Update(*h, row)
                }
                None => LineWrite::Insert(row),
            });
        }
        (matched == stored.len()).then_some(out)
    }
}

struct Edits {
    main: Vec<(usize, FieldValue)>,
    tabs: Vec<TabJob>,
}

fn set_std(row: &mut [FieldValue], ti: &TableInfo, std: StdField, v: FieldValue) {
    if let Some(slot) = ti
        .field_by_std(std)
        .and_then(|fid| row.get_mut(usize::from(fid.0)))
    {
        *slot = v;
    }
}

fn edits(
    ex: &mut Execution,
    plan: &Plan<'_>,
    inst: &ObjectInstance,
    id: RefId,
    is_new: bool,
) -> CallResult<Edits> {
    let map = plan.app.store().storage();
    let mut main = Vec::new();
    let group = inst.folder_flag().unwrap_or(false);
    {
        let st = inst.state.borrow();
        for (i, f) in plan.def.fields.iter().enumerate() {
            let Some(fid) = plan.st.fields.get(i).copied().flatten() else {
                continue;
            };
            let col = usize::from(fid.0);
            match f.std {
                Some(StdAttr::PredefinedDataName) => {
                    if crate::write_checks::clears_predefined_name(inst, i) {
                        main.push((col, crate::write_checks::cleared_id()));
                    }
                    continue;
                }
                Some(StdAttr::Ref | StdAttr::Predefined) => continue,
                Some(StdAttr::IsFolder) => {
                    if is_new {
                        main.push((col, FieldValue::Bool(!group)));
                    }
                    continue;
                }
                _ => {}
            }
            if is_new && inst.hidden(i) && encode(plan.main, fid, &FieldValue::Null).is_ok() {
                main.push((col, FieldValue::Null));
                continue;
            }
            if let Some(Some(v)) = st.attributes.get(i) {
                main.push((col, to_field(plan.app, f, v)?));
            }
        }
    }
    let tabs = inst.state.borrow().tabular.clone();
    let mut jobs = Vec::new();
    for (t, tab) in tabs.iter().enumerate() {
        let lines = tab.lines();
        if if is_new {
            lines.is_empty()
        } else {
            !tab.touched()
        } {
            continue;
        }
        let (Some((table, ids)), Some(td)) = (plan.st.tabular.get(t), plan.def.tabular.get(t))
        else {
            return Err(internal("no table of the section").into());
        };
        let ti = map
            .try_table(*table)
            .ok_or_else(|| internal("no table of the section"))?;
        let mut hints = vec![None; ti.fields.len()];
        for (f, fid) in td.fields.iter().zip(ids) {
            if let Some(slot) = fid.and_then(|fid| hints.get_mut(usize::from(fid.0))) {
                *slot = Some(f);
            }
        }
        let mut rows = Vec::with_capacity(lines.len());
        for (j, line) in lines.iter().enumerate() {
            ex.charge(1)?;
            let number = FieldValue::Number(ibschema::Numeric::from_i64(
                i64::try_from(j + 1).unwrap_or(i64::MAX),
            ));
            let mut row = blank_row(plan.app, ti, &hints);
            set_std(
                &mut row,
                ti,
                StdField::Id,
                FieldValue::Ref {
                    table: plan.number,
                    id,
                },
            );
            let key = FieldValue::Number(ibschema::Numeric::from_i64(
                i64::try_from(if is_new { j } else { j + 1 }).unwrap_or(i64::MAX),
            ));
            set_std(&mut row, ti, StdField::KeyField, key);
            for ((c, f), fid) in td.fields.iter().enumerate().zip(ids) {
                let Some(slot) = fid.and_then(|fid| row.get_mut(usize::from(fid.0))) else {
                    continue;
                };
                *slot = if f.std == Some(StdAttr::LineNumber) {
                    number.clone()
                } else {
                    match line.get(c) {
                        Some(v) => to_field(plan.app, f, v)?,
                        None => continue,
                    }
                };
            }
            rows.push(row);
        }
        jobs.push(TabJob {
            table: *table,
            key: plan.key(ti, id)?,
            rows,
            key_col: ti
                .field_by_std(StdField::KeyField)
                .map(|f| usize::from(f.0)),
            line_col: td
                .fields
                .iter()
                .zip(ids)
                .find(|(f, _)| f.std == Some(StdAttr::LineNumber))
                .and_then(|(_, fid)| fid.map(|f| usize::from(f.0))),
        });
    }
    Ok(Edits { main, tabs: jobs })
}

fn apply(row: &mut [FieldValue], edits: &[(usize, FieldValue)]) {
    for (col, v) in edits {
        if let Some(slot) = row.get_mut(*col) {
            *slot = v.clone();
        }
    }
}

pub(crate) fn row_version(row: &[FieldValue]) -> Option<String> {
    row.iter().find_map(|v| match v {
        FieldValue::RowVersion(w) => Some(query_ir::convert::data_version(w)),
        _ => None,
    })
}

enum Stored {
    Done,
    Refused(HostError),
    Conflict,
}

fn outside_call() -> HostError {
    internal("the write of an object outside a call")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DataWrite {
    NoUsers,
    Privileged,
    Allowed,
    Denied,
}

pub(crate) fn user_refusal(what: &str) -> HostError {
    HostError::not_implemented("P6", &format!("запись данных под пользователем: {what}"))
}

pub(crate) fn data_write(
    ex: &mut Execution,
    app: &Application,
    md: crate::model::Uuid16,
    rights: &[&str],
) -> Result<DataWrite, HostError> {
    use md_model::parts::RightTarget as MdTarget;
    let privileged = ex.effective_flags().privileged;
    let Some(ctx) = crate::session::call_ctx(ex) else {
        return Err(HostError::not_implemented(
            "P6",
            "application write security provider",
        ));
    };
    let user = ctx.sess.user();
    if user.no_users {
        return Ok(DataWrite::NoUsers);
    }
    if privileged {
        return Ok(DataWrite::Privileged);
    }
    let config = &app.model().config;
    let Some(obj) = config.object(md) else {
        return Err(user_refusal("объект вне конфигурации"));
    };
    let mut asked = Vec::with_capacity(rights.len());
    for name in rights {
        let Some(right) = md_model::parts::Right::from_name(name) else {
            return Err(user_refusal(&format!("право {name} не найдено")));
        };
        if !right.is_applicable(obj.kind) {
            return Err(user_refusal(&format!("вид объекта без права {name}")));
        }
        asked.push(right);
    }
    let mut fields = Vec::new();
    let mut stack: Vec<&md_model::Child> = obj.children.iter().collect();
    while let Some(c) = stack.pop() {
        fields.push(c.id);
        stack.extend(c.children.iter());
    }
    let object = MdTarget::Object(md);
    for role in user.roles.iter().filter_map(|id| config.object(*id)) {
        let Some(granted) = role.rights() else {
            continue;
        };
        for &right in &asked {
            if granted
                .restrictions(&object, right)
                .iter()
                .any(|r| !r.condition.trim().is_empty() || !r.fields.is_empty())
            {
                return Err(user_refusal("ограничение на уровне записей"));
            }
            let field_right = granted.objects.iter().any(|o| {
                let named = match &o.target {
                    MdTarget::Child { id, field: true } => fields.contains(id),
                    MdTarget::StandardAttribute { owner, .. } => *owner == md,
                    _ => false,
                };
                named && o.rights.iter().any(|rv| rv.right == right)
            });
            if field_right {
                return Err(user_refusal("права на поля"));
            }
        }
    }
    let target = crate::rights::RightTarget::Object(md);
    Ok(
        if asked
            .iter()
            .all(|r| app.has_right(user, target, crate::rights::Right(*r)))
        {
            DataWrite::Allowed
        } else {
            DataWrite::Denied
        },
    )
}

pub(crate) fn admit_insert(
    ex: &mut Execution,
    app: &Application,
    obj: ObjId,
    method: &str,
) -> CallResult<()> {
    if crate::session::call_ctx(ex).is_none() {
        return Ok(());
    }
    let md = app.model().obj(obj).md;
    match data_write(ex, app, md, &["Insert"])? {
        DataWrite::NoUsers | DataWrite::Privileged => Ok(()),
        _ if method == "СоздатьГруппу" || method == "СоздатьУзел" => {
            Err(user_refusal(method).into())
        }
        DataWrite::Allowed => Ok(()),
        DataWrite::Denied => Err(crate::rights::access_violation().into()),
    }
}

fn admit_write(
    ex: &mut Execution,
    app: &Application,
    inst: &ObjectInstance,
    def: &ObjDef,
    document: Option<&DocumentModes>,
) -> CallResult<()> {
    let is_new = inst.state.borrow().is_new;
    let op = document.map(|m| m.op);
    let asked = match data_write(ex, app, def.md, &["Update"])? {
        DataWrite::NoUsers => return Ok(()),
        DataWrite::Privileged => {
            return Err(user_refusal("запись в привилегированном режиме").into());
        }
        asked => asked,
    };
    if def.kind == ObjKind::ExchangePlan {
        return Err(user_refusal("запись узла плана обмена").into());
    }
    if is_new {
        return Err(user_refusal("запись нового объекта").into());
    }
    match op {
        None | Some(DocWrite::Write) | Some(DocWrite::Post) => {}
        Some(DocWrite::Unpost) => return Err(user_refusal("отмена проведения").into()),
        Some(DocWrite::Mark(_)) => return Err(user_refusal("пометка удаления").into()),
    }
    if inst.loading() {
        return Err(user_refusal("ОбменДанными.Загрузка").into());
    }
    if asked == DataWrite::Denied {
        return Err(crate::rights::access_violation().into());
    }
    match op {
        None => Err(user_refusal("запись справочника с правом Изменение").into()),
        Some(DocWrite::Post)
            if data_write(ex, app, def.md, &["Posting"])? != DataWrite::Allowed =>
        {
            Err(user_refusal("проведение с правом Изменение без права Проведение").into())
        }
        _ => plans_read(ex, app, def),
    }
}

fn plans_read(ex: &mut Execution, app: &Application, def: &ObjDef) -> CallResult<()> {
    let Some(ctx) = crate::session::call_ctx(ex) else {
        return Err(outside_call().into());
    };
    let user = ctx.sess.user();
    let read = md_model::parts::Right::from_name("Read")
        .ok_or_else(|| internal("the right Read is not known"))?;
    for (plan, _) in crate::changes::plans_of(app, def.id) {
        let md = app.model().obj(plan).md;
        if !app.has_right(
            user,
            crate::rights::RightTarget::Object(md),
            crate::rights::Right(read),
        ) {
            return Err(user_refusal("объект плана обмена без права Чтение на план").into());
        }
    }
    Ok(())
}

pub(crate) fn write(ex: &mut Execution, inst: &Rc<ObjectInstance>) -> CallResult<()> {
    write_with(ex, inst, None)
}

fn document_modes(
    ex: &mut Execution,
    inst: &ObjectInstance,
    args: &[bsl_vm::Arg],
) -> CallResult<DocumentModes> {
    let refuse = |what: &str| -> bsl_vm::CallError {
        HostError::not_implemented(&inst.type_name(), &format!("Записать: {what}")).into()
    };
    let enum_type = |name: &str| platform_catalog::catalog().find_type(name);
    let mode = crate::meta::arg_value(ex, args, 0)?;
    let op = match &mode {
        Value::Undefined => DocWrite::Write,
        v => match bsl_stdlib::enums::as_enum_value(v) {
            Some(e) if Some(e.type_idx()) == enum_type("РежимЗаписиДокумента") => {
                match e.name_ru() {
                    "Запись" => DocWrite::Write,
                    "Проведение" => DocWrite::Post,
                    "ОтменаПроведения" => DocWrite::Unpost,
                    _ => return Err(refuse(&format!("РежимЗаписиДокумента.{}", e.name_ru()))),
                }
            }
            _ => return Err(crate::meta::type_mismatch(1).into()),
        },
    };
    let post = op == DocWrite::Post;
    let posting = crate::meta::arg_value(ex, args, 1)?;
    if op == DocWrite::Unpost {
        if !matches!(posting, Value::Undefined) {
            return Err(refuse(
                "РежимЗаписиДокумента.ОтменаПроведения с режимом проведения",
            ));
        }
        return Ok(DocumentModes::undoing(DocWrite::Unpost)?);
    }
    let mut real_time = false;
    match (&posting, post) {
        (Value::Undefined, false) => {}
        (_, false) => return Err(refuse("режим проведения при записи")),
        (Value::Undefined, true) => default_posting_check(ex, inst)?,
        (v, true) => match bsl_stdlib::enums::as_enum_value(v) {
            Some(e) if Some(e.type_idx()) == enum_type("РежимПроведенияДокумента") => {
                match e.name_ru() {
                    "Неоперативный" => {}
                    "Оперативный" => {
                        real_time_check(ex, inst)?;
                        real_time = true;
                    }
                    other => return Err(refuse(&format!("РежимПроведенияДокумента.{other}"))),
                }
            }
            _ => return Err(crate::meta::type_mismatch(2).into()),
        },
    }
    let value = |t: &str, v: &str| {
        bsl_stdlib::api::system_enum_value(t, v)
            .ok_or_else(|| internal(&format!("no system enumeration value {t}.{v}")))
    };
    Ok(DocumentModes {
        write: value(
            "РежимЗаписиДокумента",
            if post {
                "Проведение"
            } else {
                "Запись"
            },
        )?,
        posting: value(
            "РежимПроведенияДокумента",
            if real_time {
                "Оперативный"
            } else {
                "Неоперативный"
            },
        )?,
        op,
    })
}

fn default_posting_check(ex: &mut Execution, inst: &ObjectInstance) -> CallResult<()> {
    let def = inst.app.model().obj(inst.obj);
    if def.behavior.realtime_posting == crate::model::Posting::Deny {
        return Ok(());
    }
    let refuse = |what: &str| -> bsl_vm::CallError {
        HostError::not_implemented(
            &inst.type_name(),
            &format!("Записать: РежимЗаписиДокумента.Проведение без режима проведения, {what}"),
        )
        .into()
    };
    let Some(i) = def.fields.iter().position(|f| f.std == Some(StdAttr::Date)) else {
        return Err(refuse("документ без даты"));
    };
    match inst.attr(ex, i)? {
        Value::Date(d) if d != bsl_prim::BslDate::EMPTY => Ok(()),
        _ => Err(refuse("дата документа пустая")),
    }
}

fn real_time_check(ex: &mut Execution, inst: &ObjectInstance) -> CallResult<()> {
    let refuse = |what: &str| -> bsl_vm::CallError {
        HostError::not_implemented(&inst.type_name(), &format!("Записать: {what}")).into()
    };
    let def = inst.app.model().obj(inst.obj);
    if def.behavior.realtime_posting == crate::model::Posting::Deny {
        return Err(crate::select::value_mismatch(2).into());
    }
    let dated = match def.fields.iter().position(|f| f.std == Some(StdAttr::Date)) {
        Some(i) => !matches!(inst.attr(ex, i)?, Value::Date(d) if d == bsl_prim::BslDate::EMPTY),
        None => false,
    };
    if !dated && crate::events::handled(inst, crate::events::Event::BeforeWrite) {
        return Err(refuse(
            "оперативное проведение без даты при обработчике ПередЗаписью",
        ));
    }
    Ok(())
}

fn real_time_date(ex: &mut Execution, inst: &ObjectInstance) -> CallResult<()> {
    let def = inst.app.model().obj(inst.obj);
    let Some(i) = def.fields.iter().position(|f| f.std == Some(StdAttr::Date)) else {
        return Ok(());
    };
    let now = ex.call_global("ТекущаяДатаСеанса", &mut [])?;
    let day = |v: &Value| match v {
        Value::Date(d) => {
            let p = d.parts();
            Some((p.year, p.month, p.day))
        }
        _ => None,
    };
    let today = day(&now).ok_or_else(|| internal("ТекущаяДатаСеанса is not a date"))?;
    let given = inst.attr(ex, i)?;
    if matches!(given, Value::Date(d) if d == bsl_prim::BslDate::EMPTY) {
        inst.assign(ex, i, now)?;
        inst.app.note_real_time_day(today);
        return Ok(());
    }
    let refuse = |what: &str| -> bsl_vm::CallError {
        HostError::not_implemented(&inst.type_name(), &format!("Записать: {what}")).into()
    };
    let date = day(&given).ok_or_else(|| internal("the date of a document is not a date"))?;
    if ex.session_time_zone() != ex.env().time_zone() {
        return Err(refuse(
            "оперативное проведение документа с датой при поясе сеанса, отличном от пояса сервера",
        ));
    }
    if crate::events::modes_seen(inst) {
        return Err(refuse(
            "оперативное проведение документа с датой при обработчике ПередЗаписью, видящем режимы",
        ));
    }
    if date == today {
        return Err(refuse(
            "оперативное проведение документа датой текущего дня",
        ));
    }
    if inst.app.last_real_time_day() == Some(date) {
        return Err(refuse(
            "оперативное проведение документа датой последнего оперативно проведённого документа",
        ));
    }
    let text = |lang| errors::render(keys::DOCOBJ_CANT_POST_OPERATIONAL, &[], lang);
    let mut e = HostError::text(&text(Lang::Ru), &text(Lang::En));
    e.categories = Some(CategorySet::of(&[
        Category::AllErrors,
        Category::ScriptRuntimeError,
        Category::OtherError,
    ]));
    Err(e.into())
}

pub(crate) fn write_document(
    ex: &mut Execution,
    inst: &Rc<ObjectInstance>,
    args: &[bsl_vm::Arg],
) -> CallResult<()> {
    inst.live()?;
    let modes = document_modes(ex, inst, args)?;
    if inst.loading() {
        return Err(HostError::not_implemented(
            &inst.type_name(),
            "Записать: ОбменДанными.Загрузка документа",
        )
        .into());
    }
    let flag = |ex: &mut Execution, std: StdAttr| -> CallResult<bool> {
        let def = inst.app.model().obj(inst.obj);
        match def.fields.iter().position(|f| f.std == Some(std)) {
            Some(i) => Ok(matches!(inst.attr(ex, i)?, Value::Bool(true))),
            None => Ok(false),
        }
    };
    let posted = flag(ex, StdAttr::Posted)?;
    if modes.op == DocWrite::Unpost {
        let marked = flag(ex, StdAttr::DeletionMark)?;
        crate::posting::unpost_check(inst, posted, marked)?;
    } else if modes.post() {
        let marked = flag(ex, StdAttr::DeletionMark)?;
        crate::posting::check(inst, posted, marked)?;
    }
    write_with(ex, inst, Some(modes))
}

pub(crate) fn write_with(
    ex: &mut Execution,
    inst: &Rc<ObjectInstance>,
    document: Option<DocumentModes>,
) -> CallResult<()> {
    let app = inst.app.clone();
    inst.live()?;
    let plan = Plan::new(&app, inst)?;
    if (plan.def.kind == ObjKind::Document) != document.is_some() {
        return Err(internal("the write of a document without its modes").into());
    }
    admit_write(ex, &app, inst, plan.def, document.as_ref())?;
    let txn = CallTxn::of_call(ex).ok_or_else(outside_call)?;
    txn.check().map_err(|e| e.host())?;
    let loading = inst.loading();
    app.predefined()
        .ensure(&app, inst.obj, &AreaKey::default())?;
    let is_new = inst.state.borrow().is_new;
    let unposting = document.as_ref().map(|m| m.op);
    let outermost = !txn.active();
    let snapshot = outermost.then(|| inst.snapshot());
    txn.begin();
    let mut taken = None;
    let mut movements = Vec::new();
    let ctx = WriteCtx {
        inst,
        is_new,
        loading,
        document,
    };
    let ended = match steps(ex, &plan, &txn, &ctx, &mut taken, &mut movements) {
        Ok(()) => crate::fo_version::commit(ex, &app, &txn),
        Err(e) => {
            let _ = txn.rollback(&*app);
            Err(e)
        }
    };
    if matches!(ended, Ok(Commit::Committed | Commit::Inner))
        && !movements.is_empty()
        && let Some(id) = taken
    {
        crate::regread::movements_written(inst, &txn, &movements, RefValue { obj: inst.obj, id })?;
    }
    if matches!(ended, Ok(Commit::Committed | Commit::Inner))
        && let Some(op @ (DocWrite::Unpost | DocWrite::Mark(true))) = unposting
    {
        crate::regread::movements_unposted(ex, inst, &txn, op == DocWrite::Unpost)?;
    }
    if matches!(ended, Ok(Commit::Committed | Commit::Inner))
        && let Some(modes) = ctx.document.as_ref()
    {
        let not_write = modes.op != DocWrite::Write;
        inst.note_document_write(&txn, !not_write);
        crate::regread::movements_after_rollback(inst, &txn, &movements, not_write)?;
    }
    if !matches!(ended, Ok(Commit::Committed | Commit::Inner)) {
        if snapshot.is_none() && is_new && ctx.document.is_some() {
            inst.note_failed_new_write(&txn);
            crate::regread::movements_failed_new(inst, &txn)?;
        }
        if let Some(snapshot) = snapshot {
            inst.restore(snapshot);
        } else if is_new && let Some(id) = taken {
            inst.take_ref(&txn, id);
        }
    }
    ended.map(|_| ())
}

fn steps(
    ex: &mut Execution,
    plan: &Plan<'_>,
    txn: &Rc<CallTxn>,
    ctx: &WriteCtx<'_>,
    taken: &mut Option<RefId>,
    movements: &mut Vec<Value>,
) -> CallResult<()> {
    let app = plan.app;
    let (inst, is_new) = (ctx.inst, ctx.is_new);
    let params = inst.exchange()?;
    let auto_before = params.auto_fill();
    let post = ctx.document.as_ref().is_some_and(|m| m.post());
    let unposts = ctx.document.as_ref().is_some_and(|m| m.unposts());
    if post {
        inst.set_posted(true)?;
    }
    if ctx.document.as_ref().is_some_and(DocumentModes::real_time) {
        real_time_date(ex, inst)?;
    }
    if unposts {
        inst.set_posted(false)?;
    }
    write_hooks::before_write(ex, ctx)?;
    let sets = match &ctx.document {
        Some(_) if post => {
            crate::posting::no_changed_sets(inst)?;
            Vec::new()
        }
        Some(m) if m.op != DocWrite::Write => {
            crate::posting::untouched_sets(inst, "в ПередЗаписью")?;
            Vec::new()
        }
        Some(_) => crate::regread::document_movements(inst, is_new)?,
        None => Vec::new(),
    };
    write_hooks::assign_code(ex, ctx)?;
    let (id, expected) = {
        let st = inst.state.borrow();
        let id = match st.r {
            Some(r) if !is_new => r.id,
            _ => st.new_ref.map_or_else(|| app.uuid1().next_ref(), |r| r.id),
        };
        (id, st.data_version.clone())
    };
    *taken = Some(id);
    write_hooks::check_write(ex, ctx)?;
    let edits = edits(ex, plan, inst, id, is_new)?;
    let hints: Vec<Option<&crate::model::FieldDef>> = {
        let mut hints = vec![None; plan.main.fields.len()];
        for (f, fid) in plan.def.fields.iter().zip(&plan.st.fields) {
            if let Some(slot) = fid.and_then(|fid| hints.get_mut(usize::from(fid.0))) {
                *slot = Some(f);
            }
        }
        hints
    };
    let mut fresh = blank_row(app, plan.main, &hints);
    crate::values::field::empty_storages(&mut fresh, &hints);
    set_std(
        &mut fresh,
        plan.main,
        StdField::Id,
        FieldValue::Ref {
            table: plan.number,
            id,
        },
    );
    apply(&mut fresh, &edits.main);
    let key = plan.key(plan.main, id)?;
    let main_table = plan.st.main;
    let bytes = edits
        .tabs
        .iter()
        .flat_map(|j| j.rows.iter())
        .fold(budget::stored_row(&fresh), |n, r| {
            n.saturating_add(budget::stored_row(r))
        });
    budget::reserve(ex, bytes.saturating_mul(2))?;
    if crate::data_lock::lock_for_write(app, txn, plan.def).is_err() {
        return Err(crate::data_lock::conflict().into());
    }
    let registration =
        crate::changes::Registration::for_write(app, inst.obj, &params, auto_before)?;
    let change_key = match &registration {
        Some(r) => r.changes.key_ref(plan.number, id)?,
        None => Vec::new(),
    };
    let journals = if ctx.document.is_some() {
        Some(crate::journal::Journals::of(app, inst.obj)?).filter(|j| !j.is_empty())
    } else {
        None
    };
    let queue = crate::history::Queue::for_write(ex, app, txn, inst.obj)?;
    let mut registered = Vec::new();
    let stored = txn.with_write(app, |w: &mut dyn WriteTxn| -> StoreResult<Stored> {
        let mut updated = None;
        if is_new {
            w.insert(main_table, &fresh)?;
        } else {
            let found = {
                let mut rows = w.range(
                    main_table,
                    IndexId::Id,
                    Bound::Included(key.as_slice()),
                    Bound::Included(key.as_slice()),
                    Direction::Forward,
                )?;
                rows.next().transpose()?
            };
            let Some((handle, mut row)) = found else {
                return Ok(Stored::Conflict);
            };
            if row_version(&row).as_deref() != Some(expected.as_str()) {
                return Ok(Stored::Conflict);
            }
            apply(&mut row, &edits.main);
            w.update(main_table, handle, &row)?;
            updated = Some(row);
        }
        for job in &edits.tabs {
            if !is_new {
                let stored = w
                    .range(
                        job.table,
                        IndexId::Id,
                        Bound::Included(job.key.as_slice()),
                        Bound::Included(job.key.as_slice()),
                        Direction::Forward,
                    )?
                    .collect::<StoreResult<Vec<(RowHandle, Vec<FieldValue>)>>>()?;
                if let Some(writes) = job.grown(&stored) {
                    for lw in writes {
                        match lw {
                            LineWrite::Keep => {}
                            LineWrite::Update(h, row) => w.update(job.table, h, &row)?,
                            LineWrite::Insert(row) => drop(w.insert(job.table, &row)?),
                        }
                    }
                    continue;
                }
                for (h, _) in stored {
                    w.delete(job.table, h)?;
                }
            }
            for row in &job.rows {
                w.insert(job.table, row)?;
            }
        }
        if let Some(j) = &journals
            && let Err(e) = j.write(w, id, updated.as_ref().unwrap_or(&fresh), is_new)?
        {
            return Ok(Stored::Refused(e));
        }
        if let Some(r) = &registration {
            registered = r.nodes(&*w)?;
            r.apply(w, &registered, &change_key)?;
        }
        if let Some(q) = &queue {
            q.object(w, id, Some(updated.as_ref().unwrap_or(&fresh)), is_new)?;
        }
        Ok(Stored::Done)
    });
    match stored {
        Ok(Stored::Done) => {
            if let Some(r) = &registration {
                r.done(&registered);
            }
            if let Some(q) = queue {
                q.finish(txn);
            }
            crate::commit_guard::guard(app, txn, inst.obj);
        }
        Ok(Stored::Conflict) => {
            return Err(other_error(l5(keys::OBJECT_VERSION_MISMATCH)).into());
        }
        Ok(Stored::Refused(e)) => return Err(e.into()),
        Err(TxnError::Store(StoreError::UniqueViolation { table_name, .. })) if is_new => {
            return Err(ref_not_unique(&table_name).into());
        }
        Err(e) => return Err(e.call_error(ex)),
    }
    if !sets.is_empty() {
        crate::regread::write_movements(ex, &inst.app, txn, &sets, RefValue { obj: inst.obj, id })?;
        *movements = sets;
    }
    if unposts {
        crate::posting::unpost(ex, inst, txn, RefValue { obj: inst.obj, id })?;
    }
    let r = RefValue { obj: inst.obj, id };
    let row = read_fresh(ex, app, r)?;
    if !row.found {
        return Err(internal("the written row is not found").into());
    }
    {
        let mut st = inst.state.borrow_mut();
        st.r = Some(r);
        st.is_new = false;
        st.data_version = row.version.clone();
        st.row = Rc::new(row);
        for slot in st.attributes.iter_mut() {
            *slot = None;
        }
        for tab in &st.tabular {
            tab.clear_touched();
        }
        st.modified.set(false);
        st.new_ref = None;
        st.issued = None;
        let id = txn.id().unwrap_or(0);
        let was_new = is_new || st.written.as_ref().is_some_and(|w| w.was_new && w.id == id);
        st.written = Some(WriteStamp {
            txn: Rc::downgrade(txn),
            id,
            was_new,
        });
    }
    let posted = write_hooks::post(ex, ctx, r)?;
    if !posted.is_empty() {
        *movements = posted;
    }
    write_hooks::on_write(ex, ctx)?;
    Ok(())
}
