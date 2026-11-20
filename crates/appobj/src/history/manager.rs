use super::versions::{HistTables, Version};
use crate::Application;
use crate::errors::{self, keys};
use crate::meta::{MdKey, MdPath, arg_value, key_with_app, member_name, no_field, type_mismatch};
use crate::model::{ObjId, ObjKind};
use crate::session::call_ctx;
use crate::txn::CallTxn;
use crate::values::refs::{RefValue, as_ref, ref_value};
use bsl_prim::BslDate;
use bsl_vm::{
    Arg, CallError, CallResult, Execution, HostError, HostObject, MemberId, Number, Registry,
    TypeId, TypeIdx, Value,
};
use ibschema::DateTime;
use ibschema::store::ReadTxn;
use ibschema::values::RefId;
use platform_catalog::catalog;
use platform_catalog::messages::Lang;
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

const MANAGER: &str = "ИсторияДанных";

const CHANGES: [&str; 3] = ["Добавление", "Изменение", "Удаление"];

fn refusal(what: &str) -> HostError {
    HostError::not_implemented(MANAGER, what)
}

fn bad_parameter(n: usize, tail: Option<(&str, &str)>) -> HostError {
    let text = |lang: Lang, t: Option<&str>| {
        let head = errors::render(keys::PARAMETER_VALUE_MISMATCH, &[&n.to_string()], lang);
        match t {
            Some(t) => format!("{head} ({t})"),
            None => head,
        }
    };
    HostError::text(
        &text(Lang::Ru, tail.map(|t| t.0)),
        &text(Lang::En, tail.map(|t| t.1)),
    )
}

fn unknown_column(n: usize, name: &str) -> HostError {
    bad_parameter(
        n,
        Some((
            &format!("Не найдена колонка {name}"),
            &format!("Column {name} not found"),
        )),
    )
}

const HELP_KEYS: [&str; 30] = [
    "Метаданные",
    "Metadata",
    "Данные",
    "Data",
    "ПредставлениеДанных",
    "DataPresentation",
    "НомерВерсии",
    "VersionNumber",
    "ДатаНачала",
    "StartDate",
    "ДатаОкончания",
    "EndDate",
    "Пользователь",
    "User",
    "ИмяПользователя",
    "UserName",
    "ПолноеИмяПользователя",
    "UserFullName",
    "ВидИзмененияДанных",
    "DataChangeType",
    "Комментарий",
    "Comment",
    "Транзакция",
    "Transaction",
    "Узел",
    "Node",
    "ЗначенияПолей",
    "FieldValues",
    "ИзменениеЗначенийПолей",
    "FieldValuesChange",
];

const COLUMNS_EN: [&str; 11] = [
    "Data",
    "DataPresentation",
    "VersionNumber",
    "Date",
    "User",
    "UserName",
    "UserFullName",
    "DataChangeType",
    "Comment",
    "Transaction",
    "Node",
];

fn named(names: impl IntoIterator<Item = &'static str>, name: &str) -> bool {
    let name = name.to_lowercase();
    names.into_iter().any(|n| n.to_lowercase() == name)
}

fn column_like(name: &str) -> bool {
    named(ALL.iter().map(|c| c.name()).chain(COLUMNS_EN), name)
}

pub(crate) fn register(app: &Arc<Application>, reg: &mut Registry) {
    let Some(ty) = catalog().find_type("МенеджерИсторииДанных") else {
        return;
    };
    let app = app.clone();
    reg.global_prop(
        MANAGER,
        "DataHistory",
        Arc::new(move |_| {
            Ok(Value::Object(Rc::new(Manager {
                app: app.clone(),
                ty,
            })))
        }),
    );
}

struct Manager {
    app: Arc<Application>,
    ty: TypeIdx,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Col {
    Data,
    Presentation,
    Number,
    Date,
    User,
    UserName,
    UserFullName,
    Change,
    Comment,
    Transaction,
    Node,
}

const ALL: [Col; 11] = [
    Col::Data,
    Col::Presentation,
    Col::Number,
    Col::Date,
    Col::User,
    Col::UserName,
    Col::UserFullName,
    Col::Change,
    Col::Comment,
    Col::Transaction,
    Col::Node,
];

impl Col {
    fn name(self) -> &'static str {
        match self {
            Col::Data => "Данные",
            Col::Presentation => "ПредставлениеДанных",
            Col::Number => "НомерВерсии",
            Col::Date => "Дата",
            Col::User => "Пользователь",
            Col::UserName => "ИмяПользователя",
            Col::UserFullName => "ПолноеИмяПользователя",
            Col::Change => "ВидИзмененияДанных",
            Col::Comment => "Комментарий",
            Col::Transaction => "Транзакция",
            Col::Node => "Узел",
        }
    }

    fn type_name(self) -> Option<&'static str> {
        match self {
            Col::Data | Col::Node => None,
            Col::Number => Some("Число"),
            Col::Date => Some("Дата"),
            Col::User => Some("УникальныйИдентификатор"),
            Col::Change => Some("ВидИзмененияДанных"),
            Col::Presentation
            | Col::UserName
            | Col::UserFullName
            | Col::Comment
            | Col::Transaction => Some("Строка"),
        }
    }

    fn of(name: &str) -> Option<Col> {
        ALL.iter().copied().find(|c| c.name() == name)
    }
}

struct Text {
    values: Vec<String>,
    occurrence: bool,
}

impl Text {
    fn of(v: &Value) -> Option<Text> {
        let one = |v: &Value| match v {
            Value::String(s) => Some(s.to_string_lossy()),
            _ => None,
        };
        let values = match bsl_stdlib::array_items(v) {
            Some(items) => items.iter().map(one).collect::<Option<Vec<_>>>()?,
            None => vec![one(v)?],
        };
        (!values.is_empty()).then_some(Text {
            values,
            occurrence: false,
        })
    }

    fn keeps(&self, field: &str) -> Option<bool> {
        let lower = field.to_lowercase();
        if self.occurrence {
            return Some(
                self.values
                    .iter()
                    .any(|x| lower.contains(&x.to_lowercase())),
            );
        }
        let readings = [
            self.values.iter().any(|x| field == x),
            self.values.iter().any(|x| field.contains(x.as_str())),
            self.values.iter().any(|x| lower == x.to_lowercase()),
            self.values
                .iter()
                .any(|x| lower.contains(&x.to_lowercase())),
        ];
        readings
            .iter()
            .all(|&r| r == readings[0])
            .then_some(readings[0])
    }
}

#[derive(Default)]
struct Filter {
    md: Option<ObjId>,
    data: Option<RefValue>,
    number: Option<i64>,
    changes: Option<Vec<i64>>,
    users: Option<Vec<[u8; 16]>>,
    user_name: Option<Text>,
    user_full_name: Option<Text>,
    comment: Option<Text>,
    from: Option<i64>,
    to: Option<i64>,
}

#[derive(Clone, Copy)]
enum Order {
    Number { desc: bool },
    DateNumberDesc,
}

fn seconds(d: &DateTime) -> Option<i64> {
    if d.year == 0 {
        return Some(0);
    }
    BslDate::from_parts(
        u32::from(d.year),
        u32::from(d.month),
        u32::from(d.day),
        u32::from(d.hour),
        u32::from(d.minute),
        u32::from(d.second),
    )
    .map(|b| b.whole_seconds())
}

fn change_of(v: &Value) -> Option<i64> {
    let e = bsl_stdlib::enums::as_enum_value(v)?;
    if catalog().type_info(e.type_idx()).name_ru != "ВидИзмененияДанных" {
        return None;
    }
    CHANGES
        .iter()
        .position(|n| *n == e.name_ru())
        .and_then(|i| i64::try_from(i).ok())
}

impl Manager {
    fn admit(&self, ex: &mut Execution, member: &str) -> CallResult<()> {
        if call_ctx(ex).is_some_and(|c| c.sess.user().no_users) {
            Ok(())
        } else {
            Err(refusal(&format!("{member}: сеанс с пользователями")).into())
        }
    }

    fn object_of(&self, v: &Value, member: &str) -> CallResult<ObjId> {
        let obj = match key_with_app(v) {
            Some((MdKey::Path(MdPath::Object(o)), app)) if Arc::ptr_eq(app, &self.app) => o,
            _ => return Err(refusal(&format!("{member}: значение не объект метаданных")).into()),
        };
        self.kept(obj, member)?;
        Ok(obj)
    }

    fn kept(&self, obj: ObjId, member: &str) -> CallResult<()> {
        let def = self.app.model().obj(obj);
        if !matches!(
            def.kind,
            ObjKind::Catalog
                | ObjKind::Document
                | ObjKind::ChartOfCharacteristicTypes
                | ObjKind::InformationRegister
        ) {
            return Err(refusal(&format!("{member}: вид объекта метаданных")).into());
        }
        if !super::kept(&self.app, obj) {
            return Err(refusal(&format!("{member}: объект без истории данных")).into());
        }
        Ok(())
    }

    fn read<T>(
        &self,
        ex: &mut Execution,
        member: &str,
        f: impl FnOnce(&HistTables<'_>, &dyn ReadTxn) -> Result<T, String>,
    ) -> CallResult<T> {
        let tables = HistTables::of(&self.app).map_err(|e| refusal(&format!("{member}: {e}")))?;
        let fail = |e: String| -> CallError { refusal(&format!("{member}: {e}")).into() };
        match CallTxn::of_call(ex) {
            Some(txn) => {
                let mut view = txn.view(&*self.app).map_err(|e| e.host())?;
                f(&tables, view.txn()).map_err(fail)
            }
            None => {
                let r = self.app.store().read().map_err(|e| fail(e.to_string()))?;
                f(&tables, &*r).map_err(fail)
            }
        }
    }

    fn get_settings(&self, ex: &mut Execution, args: &[Arg], member: &str) -> CallResult<Value> {
        let obj = match key_with_app(&arg_value(ex, args, 0)?) {
            Some((MdKey::Path(MdPath::Object(o)), app)) if Arc::ptr_eq(app, &self.app) => o,
            _ => return Err(refusal(&format!("{member}: значение не объект метаданных")).into()),
        };
        if !matches!(
            self.app.model().obj(obj).kind,
            ObjKind::Catalog
                | ObjKind::Document
                | ObjKind::ChartOfCharacteristicTypes
                | ObjKind::InformationRegister
                | ObjKind::Constant
        ) {
            return Err(refusal(&format!("{member}: вид объекта метаданных")).into());
        }
        let md = self.app.model().obj(obj).md;
        let stored = self.read(ex, member, |t, r| {
            t.has_settings(r, md).map_err(|e| e.to_string())
        })?;
        if stored {
            return Err(refusal(&format!("{member}: настройки истории записаны в базе")).into());
        }
        Ok(Value::Undefined)
    }

    fn filter(&self, v: &Value, member: &str) -> CallResult<Filter> {
        if bsl_stdlib::array_items(v).is_some() {
            return Err(type_mismatch(1).into());
        }
        let fields = bsl_stdlib::structure_fields(v)
            .ok_or_else(|| refusal(&format!("{member}: Отбор не Структура")))?;
        let mut f = Filter::default();
        for (key, value) in fields {
            let bad = || refusal(&format!("{member}: Отбор.{key}: значение"));
            match key.as_str() {
                "Метаданные" => f.md = Some(self.object_of(&value, member)?),
                "Данные" => {
                    let r = as_ref(&value).filter(|r| !r.is_empty()).ok_or_else(|| {
                        refusal(&format!(
                            "{member}: Отбор.Данные: не ссылка (ключ записи регистра не читается)"
                        ))
                    })?;
                    self.kept(r.obj, member)?;
                    f.data = Some(r);
                }
                "НомерВерсии" => match value {
                    Value::Number(n) => f.number = Some(n.to_i64().ok_or_else(bad)?),
                    _ => return Err(bad().into()),
                },
                "ВидИзмененияДанных" => {
                    let items =
                        bsl_stdlib::array_items(&value).unwrap_or_else(|| vec![value.clone()]);
                    let changes = items
                        .iter()
                        .map(change_of)
                        .collect::<Option<Vec<_>>>()
                        .filter(|c| !c.is_empty())
                        .ok_or_else(bad)?;
                    f.changes = Some(changes);
                }
                "ИмяПользователя" => {
                    let mut t = Text::of(&value).ok_or_else(bad)?;
                    t.occurrence = true;
                    f.user_name = Some(t)
                }
                "Пользователь" => {
                    let items =
                        bsl_stdlib::array_items(&value).unwrap_or_else(|| vec![value.clone()]);
                    let users = items
                        .iter()
                        .map(|i| match i {
                            Value::String(_) => None,
                            _ => bsl_stdlib::values::uuid_bytes(i),
                        })
                        .collect::<Option<Vec<_>>>()
                        .filter(|u| !u.is_empty())
                        .ok_or_else(bad)?;
                    f.users = Some(users);
                }
                "ПолноеИмяПользователя" => {
                    f.user_full_name = Some(Text::of(&value).ok_or_else(bad)?)
                }
                "Комментарий" => match value {
                    Value::String(_) => f.comment = Some(Text::of(&value).ok_or_else(bad)?),
                    _ => return Err(bad().into()),
                },
                "ДатаНачала" | "ДатаОкончания" => {
                    let Value::Date(d) = value else {
                        return Err(bad().into());
                    };
                    let s = d.whole_seconds();
                    if key == "ДатаОкончания" && s == 0 {
                        return Err(refusal(&format!("{member}: Отбор.{key}: пустая дата")).into());
                    }
                    if key == "ДатаНачала" {
                        f.from = Some(s);
                    } else {
                        f.to = Some(s);
                    }
                }
                "Транзакция" => match &value {
                    Value::String(s) if s.to_string_lossy().is_empty() => {}
                    _ => return Err(refusal(&format!("{member}: Отбор.{key}")).into()),
                },
                k if named(HELP_KEYS, k) => {
                    return Err(refusal(&format!("{member}: Отбор.{key}")).into());
                }
                _ => {}
            }
        }
        if let (Some(md), Some(data)) = (f.md, f.data)
            && md != data.obj
        {
            return Err(refusal(&format!("{member}: Метаданные и Данные разных объектов")).into());
        }
        if f.md.is_none() && f.data.is_none() {
            return Err(bad_parameter(1, None).into());
        }
        Ok(f)
    }

    fn keeps(f: &Filter, v: &Version, local: Option<i64>) -> Option<bool> {
        let mut keep = f.number.is_none_or(|n| n == v.number)
            && f.changes.as_ref().is_none_or(|c| c.contains(&v.change))
            && f.users
                .as_ref()
                .is_none_or(|u| u.contains(v.user.as_bytes()));
        for (text, field) in [
            (&f.user_name, &v.user_name),
            (&f.user_full_name, &v.user_full_name),
            (&f.comment, &v.comment),
        ] {
            if let Some(t) = text {
                keep &= t.keeps(field)?;
            }
        }
        if f.from.is_some() || f.to.is_some() {
            let at = local?;
            for (bound, after) in [(f.from, true), (f.to, false)] {
                let Some(b) = bound else { continue };
                keep &= if after { at >= b } else { at <= b };
            }
        }
        Some(keep)
    }

    fn columns(&self, v: &Value, member: &str) -> CallResult<Vec<Col>> {
        let names: Vec<String> = match v {
            Value::Undefined => return Ok(ALL.to_vec()),
            Value::String(s) => s
                .to_string_lossy()
                .split(',')
                .map(|x| x.trim().to_string())
                .collect(),
            _ => match bsl_stdlib::array_items(v) {
                Some(items) => items
                    .iter()
                    .map(|i| match i {
                        Value::String(s) => Ok(s.to_string_lossy().trim().to_string()),
                        _ => Err(refusal(&format!("{member}: Колонки: значение"))),
                    })
                    .collect::<Result<_, _>>()?,
                None => return Err(refusal(&format!("{member}: Колонки: значение")).into()),
            },
        };
        let mut out = Vec::new();
        for n in names {
            match Col::of(&n) {
                Some(c) if !out.contains(&c) => out.push(c),
                Some(_) => return Err(refusal(&format!("{member}: колонка {n} дважды")).into()),
                None if column_like(&n) => {
                    return Err(refusal(&format!("{member}: колонка {n}")).into());
                }
                None => return Err(unknown_column(2, &n).into()),
            }
        }
        if out.is_empty() {
            return Err(refusal(&format!("{member}: пустой список колонок")).into());
        }
        Ok(out)
    }

    fn select_versions(&self, ex: &mut Execution, args: &[Arg], member: &str) -> CallResult<Value> {
        let f = self.filter(&arg_value(ex, args, 0)?, member)?;
        let cols = self.columns(&arg_value(ex, args, 1)?, member)?;
        let order = arg_value(ex, args, 2)?;
        let limit = arg_value(ex, args, 3)?;
        let obj =
            f.md.or(f.data.map(|d| d.obj))
                .ok_or_else(|| refusal(member))?;
        let def = self.app.model().obj(obj);
        let md = def.md;
        let data = f.data.map(|d| super::dataid::object_id(d.id));
        let mut rows = 0u64;
        let all = self.read(ex, member, |t, r| {
            t.versions(r, md, data, &mut || {
                rows += 1;
                Ok(())
            })
            .map_err(|e| e.to_string())?
        })?;
        ex.charge(rows)?;
        crate::read_budget::reserve(ex, crate::read_budget::slots::<Version>(all.len()))?;
        let dated = f.from.is_some() || f.to.is_some();
        let mut picked = Vec::new();
        for v in all {
            let local = if dated {
                Some(local_date(ex, &v.date, member)?.whole_seconds())
            } else {
                None
            };
            match Self::keeps(&f, &v, local) {
                Some(true) => picked.push(v),
                Some(false) => {}
                None => {
                    return Err(refusal(&format!(
                        "{member}: отбор неоднозначен (регистр или вхождение)"
                    ))
                    .into());
                }
            }
        }
        let default = if f.data.is_some() {
            Order::Number { desc: true }
        } else {
            Order::DateNumberDesc
        };
        self.order(&mut picked, &order, default, member)?;
        match limit {
            Value::Undefined => {}
            Value::Number(n) => match n.to_i64().and_then(|k| usize::try_from(k).ok()) {
                Some(k) if k > 0 => picked.truncate(k),
                _ => {
                    return Err(refusal(&format!("{member}: МаксимальноеКоличество {n}")).into());
                }
            },
            _ => {
                return Err(refusal(&format!("{member}: МаксимальноеКоличество: значение")).into());
            }
        }
        if !picked.is_empty() {
            let empty = self.read(ex, member, |t, r| {
                t.queues_empty(r).map_err(|e| e.to_string())
            })?;
            if !empty {
                return Err(refusal(&format!("{member}: версии при непустой очереди")).into());
            }
        }
        self.table(ex, obj, &cols, &picked, member)
    }

    fn order(
        &self,
        rows: &mut [Version],
        order: &Value,
        default: Order,
        member: &str,
    ) -> CallResult<()> {
        let how = match order {
            Value::Undefined => default,
            Value::String(s) if s.to_string_lossy().contains(',') => {
                return Err(refusal(&format!("{member}: Порядок из нескольких колонок")).into());
            }
            Value::String(s) => {
                let spec = s.to_string_lossy();
                match spec.split_whitespace().collect::<Vec<_>>().as_slice() {
                    ["НомерВерсии"] | ["НомерВерсии", "Возр"] => {
                        Order::Number { desc: false }
                    }
                    ["НомерВерсии", "Убыв"] => Order::Number { desc: true },
                    [name] | [name, "Возр" | "Убыв"]
                        if Col::of(name).is_none() && !column_like(name) =>
                    {
                        return Err(unknown_column(3, name).into());
                    }
                    _ => return Err(refusal(&format!("{member}: Порядок {spec}")).into()),
                }
            }
            _ => return Err(refusal(&format!("{member}: Порядок: значение")).into()),
        };
        let key = |v: &Version| match how {
            Order::Number { .. } => (0, v.number),
            Order::DateNumberDesc => (seconds(&v.date).unwrap_or(0), v.number),
        };
        rows.sort_by_key(key);
        if rows
            .iter()
            .zip(rows.iter().skip(1))
            .any(|(a, b)| key(a) == key(b))
        {
            return Err(refusal(&format!("{member}: Порядок с равными ключами")).into());
        }
        if !matches!(how, Order::Number { desc: false }) {
            rows.reverse();
        }
        Ok(())
    }

    fn cell(
        &self,
        ex: &mut Execution,
        obj: ObjId,
        c: Col,
        v: &Version,
        member: &str,
    ) -> CallResult<Value> {
        Ok(match c {
            Col::Data => data_ref(&self.app, obj, &v.data)?,
            Col::Presentation => {
                let mut id = [0u8; 16];
                id.copy_from_slice(&v.data[..16]);
                let found =
                    crate::read::read_object_row(ex, &self.app, RefValue { obj, id: RefId(id) })?
                        .found;
                if !found {
                    return Err(
                        refusal(&format!("{member}: ПредставлениеДанных: данных нет")).into(),
                    );
                }
                let r = data_ref(&self.app, obj, &v.data)?;
                Value::String(ex.present(&r)?)
            }
            Col::Date => Value::Date(local_date(ex, &v.date, member)?),
            Col::User => bsl_stdlib::values::new_uuid(*v.user.as_bytes()),
            Col::Transaction => Value::from(transaction(ex, &v.txn)?.as_str()),
            Col::Number => Value::Number(Number::from_i64(v.number)),
            Col::Change => {
                let name = usize::try_from(v.change)
                    .ok()
                    .and_then(|i| CHANGES.get(i))
                    .ok_or_else(|| refusal(&format!("{member}: вид изменения {}", v.change)))?;
                bsl_stdlib::api::system_enum_value("ВидИзмененияДанных", name)
                    .ok_or_else(|| refusal(&format!("{member}: ВидИзмененияДанных")))?
            }
            Col::UserName => Value::from(v.user_name.as_str()),
            Col::UserFullName => Value::from(v.user_full_name.as_str()),
            Col::Comment => Value::from(v.comment.as_str()),
            Col::Node => {
                if !v.node_empty {
                    return Err(refusal(&format!("{member}: Узел не пуст")).into());
                }
                Value::Undefined
            }
        })
    }

    fn table(
        &self,
        ex: &mut Execution,
        obj: ObjId,
        cols: &[Col],
        rows: &[Version],
        member: &str,
    ) -> CallResult<Value> {
        if (cols.contains(&Col::Data) || cols.contains(&Col::Presentation))
            && self.app.model().obj(obj).kind == ObjKind::InformationRegister
        {
            return Err(refusal(&format!("{member}: колонка Данные регистра сведений")).into());
        }
        let table = ex.construct_by_name("ТаблицаЗначений", &mut [])?;
        let list = ex.get_member(&table, "Колонки")?;
        for c in cols {
            ex.charge(1)?;
            let mut add = vec![Arg::Val(Value::from(c.name()))];
            if let Some(t) = c.type_name() {
                let td = ex.construct_by_name("ОписаниеТипов", &mut [Arg::Val(Value::from(t))])?;
                add.push(Arg::Val(td));
            }
            ex.call_method(&list, "Добавить", &mut add, true)?;
        }
        for v in rows {
            let row = ex.call_method(&table, "Добавить", &mut [], true)?;
            for &c in cols {
                let cell = self.cell(ex, obj, c, v, member)?;
                ex.set_member(&row, c.name(), cell)?;
            }
        }
        Ok(table)
    }

    fn update_history(&self, ex: &mut Execution, args: &[Arg], member: &str) -> CallResult<Value> {
        let first = arg_value(ex, args, 0)?;
        let flags_from = match first {
            Value::Undefined | Value::Bool(_) => 0,
            _ => {
                return Err(
                    refusal(&format!("{member}: вариант с данными или метаданными")).into(),
                );
            }
        };
        if args.len() > 2 {
            return Err(refusal(&format!("{member}: больше двух параметров")).into());
        }
        for i in flags_from..args.len() {
            if !matches!(arg_value(ex, args, i)?, Value::Undefined | Value::Bool(_)) {
                return Err(refusal(&format!("{member}: параметр {}", i + 1)).into());
            }
        }
        if CallTxn::of_call(ex).is_some_and(|t| t.active()) {
            let r = self
                .app
                .store()
                .read()
                .map_err(|e| refusal(&format!("{member}: {e}")))?;
            let tables =
                HistTables::of(&self.app).map_err(|e| refusal(&format!("{member}: {e}")))?;
            let empty = tables
                .queues_empty(&*r)
                .map_err(|e| refusal(&format!("{member}: {e}")))?;
            if !empty {
                return Err(
                    refusal(&format!("{member}: в транзакции при непустой очереди")).into(),
                );
            }
            return Ok(Value::Undefined);
        }
        let empty = self.read(ex, member, |t, r| {
            t.queues_empty(r).map_err(|e| e.to_string())
        })?;
        if !empty {
            return Err(refusal(&format!("{member}: очередь не пуста")).into());
        }
        Ok(Value::Undefined)
    }
}

impl HostObject for Manager {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(no_field(&m.0.to_string()).into())
    }

    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(no_field(&m.0.to_string()).into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let Some((true, ru, en)) = member_name(catalog(), self.ty, m) else {
            return Err(no_field(&m.0.to_string()).into());
        };
        self.admit(ex, ru)?;
        match en {
            "GetSettings" => self.get_settings(ex, args, ru),
            "SelectVersions" => self.select_versions(ex, args, ru),
            "UpdateHistory" => self.update_history(ex, args, ru),
            _ => Err(refusal(ru).into()),
        }
    }

    fn boost_hash(&self) -> Option<i32> {
        Some(0x6869_7300 | i32::from(self.ty.0 as u8))
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<Manager>()
                .is_some_and(|o| Arc::ptr_eq(&o.app, &self.app)),
        )
    }
}

fn data_ref(app: &Application, obj: ObjId, data: &[u8; 20]) -> Result<Value, HostError> {
    let mut id = [0u8; 16];
    id.copy_from_slice(&data[..16]);
    if data[16..] != [0; 4] {
        return Err(refusal("ВыбратьВерсии: ключ данных не ссылка"));
    }
    ref_value(app, RefValue { obj, id: RefId(id) })
}

fn local_date(ex: &mut Execution, d: &DateTime, member: &str) -> CallResult<BslDate> {
    let zone = ex.env().time_zone().to_string();
    if zone.is_empty() {
        return Err(refusal(&format!("{member}: пояс сервера не задан")).into());
    }
    if ex.session_time_zone() != zone {
        return Err(refusal(&format!("{member}: пояс сеанса не равен поясу сервера")).into());
    }
    let utc = seconds(d)
        .filter(|s| *s != 0)
        .and_then(|s| BslDate::from_ticks(s * BslDate::TICKS_PER_SECOND))
        .ok_or_else(|| refusal(&format!("{member}: дата версии")))?;
    let mut args = [
        Arg::Val(Value::Date(utc)),
        Arg::Val(Value::from(zone.as_str())),
    ];
    match ex.call_global("МестноеВремя", &mut args) {
        Ok(Value::Date(local)) => Ok(local),
        _ => Err(refusal(&format!("{member}: пояс сервера {zone}")).into()),
    }
}

fn transaction(ex: &mut Execution, txn: &[u8; 16]) -> CallResult<String> {
    if txn.iter().all(|b| *b == 0) {
        return Ok(String::new());
    }
    let mut head = [0u8; 8];
    head.copy_from_slice(&txn[..8]);
    let mut tail = [0u8; 8];
    tail.copy_from_slice(&txn[8..]);
    let ticks = i64::try_from(u64::from_le_bytes(head)).ok();
    let date = ticks
        .map(|t| t - t % BslDate::TICKS_PER_SECOND)
        .and_then(BslDate::from_ticks)
        .ok_or_else(|| refusal("ВыбратьВерсии: транзакция версии"))?;
    let text = ex.present(&Value::Date(date))?;
    Ok(format!(
        "{} ({})",
        text.to_string_lossy(),
        u64::from_le_bytes(tail)
    ))
}
