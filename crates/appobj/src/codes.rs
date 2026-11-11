use crate::Application;
use crate::errors::{self, keys};
use crate::events::{self, Event, Param};
use crate::meta::{arg_value, type_mismatch};
use crate::model::{CodeSeries, CodeType, FieldDef, ObjDef, ObjId, StdAttr};
use crate::numerator::{self, IssueError, Issued, SeriesKey};
use crate::objects::ObjectInstance;
use crate::predefined::AreaKey;
use crate::read::{admit_data_read, read_view, store_call_error};
use crate::txn::{CallTxn, View};
use crate::values::field::to_field;
use crate::values::refs::{RefValue, as_ref};
use crate::write_hooks::WriteCtx;
use bsl_prim::Collation;
use bsl_vm::{Arg, CallResult, Category, CategorySet, Execution, HostError, Place, Value};
use ibschema::Numeric;
use ibschema::fields::FieldValue;
use ibschema::names::StdField;
use ibschema::storage::{FieldId, IndexId, IndexInfo, TableId, TableInfo};
use ibschema::store::Direction;
use ibschema::values::RefId;
use memory_budget::MemoryBudget;
use platform_catalog::messages::Lang;
use std::cmp::Ordering;
use std::ops::Bound;
use std::rc::Rc;
use std::sync::Arc;

fn internal(text: &str) -> HostError {
    HostError::text(&format!("internal: {text}"), &format!("internal: {text}"))
}

pub(crate) fn other_error(mut e: HostError) -> HostError {
    e.categories = Some(CategorySet::of(&[
        Category::AllErrors,
        Category::ScriptRuntimeError,
        Category::OtherError,
    ]));
    e
}

pub(crate) fn field_message(key: &'static str, args: &[&str], field: StdAttr) -> HostError {
    let (ru, en) = field.names();
    let with = |lang: Lang, name: &str| {
        let mut all: Vec<&str> = args.to_vec();
        all.push(name);
        errors::render(key, &all, lang)
    };
    HostError::text(&with(Lang::Ru, ru), &with(Lang::En, en))
}

fn not_unique(code: &str) -> HostError {
    other_error(field_message(keys::CODE_NOT_UNIQUE, &[code], StdAttr::Code))
}

pub(crate) struct Layout<'a> {
    pub app: &'a Application,
    pub obj: ObjId,
    pub def: &'a ObjDef,
    pub table: TableId,
    pub ti: &'a TableInfo,
    pub number: u32,
    pub code: Option<usize>,
    pub owner: Option<usize>,
    pub parent: Option<usize>,
    pub predefined_name: Option<usize>,
}

impl<'a> Layout<'a> {
    pub(crate) fn of(app: &'a Application, obj: ObjId) -> Result<Layout<'a>, HostError> {
        let model = app.model();
        let def = model.obj(obj);
        let st = model
            .storage_of(obj)
            .ok_or_else(|| internal("the object has no storage"))?;
        let number = st
            .ref_number
            .ok_or_else(|| internal("the object has no reference table"))?;
        let ti = app
            .store()
            .storage()
            .try_table(st.main)
            .ok_or_else(|| internal("no main table"))?;
        let at = |a: StdAttr| def.fields.iter().position(|f| f.std == Some(a));
        Ok(Layout {
            app,
            obj,
            def,
            table: st.main,
            ti,
            number,
            code: at(StdAttr::Code),
            owner: at(StdAttr::Owner),
            parent: at(StdAttr::Parent),
            predefined_name: at(StdAttr::PredefinedDataName),
        })
    }

    pub(crate) fn field(&self, i: usize) -> Result<&'a FieldDef, HostError> {
        self.def
            .fields
            .get(i)
            .ok_or_else(|| internal("no such object field"))
    }

    pub(crate) fn column(&self, std: StdField) -> Option<FieldId> {
        self.ti.field_by_std(std)
    }

    pub(crate) fn cell<'r>(&self, row: &'r [FieldValue], std: StdField) -> Option<&'r FieldValue> {
        row.get(usize::from(self.column(std)?.0))
    }

    pub(crate) fn index(&self, want: &[StdField]) -> Option<(IndexId, &'a IndexInfo)> {
        self.ti.indexes.iter().enumerate().find_map(|(k, ix)| {
            let roles: Vec<Option<StdField>> = ix
                .fields
                .iter()
                .skip(ix.prefix)
                .map(|f| self.ti.field(*f).and_then(|fi| fi.std))
                .collect();
            let begins =
                roles.len() >= want.len() && roles.iter().zip(want).all(|(r, w)| *r == Some(*w));
            begins
                .then(|| u16::try_from(k).ok().map(|k| (IndexId::Named(k), ix)))
                .flatten()
        })
    }

    pub(crate) fn area_prefix(&self, ix: &IndexInfo) -> Result<Vec<FieldValue>, HostError> {
        if ix.prefix == 0 {
            return Ok(Vec::new());
        }
        let scope = self
            .app
            .options()
            .area()
            .ok_or_else(|| HostError::not_implemented("Код", "Разделение данных"))?;
        let values = scope
            .area(self.ti)
            .map_err(|_| internal("invalid bound area"))?;
        if values.len() != ix.prefix {
            return Err(internal("separator prefix does not match the index"));
        }
        Ok(values)
    }

    pub(crate) fn id_key(&self, id: RefId) -> Result<Vec<FieldValue>, HostError> {
        let mut key = match self.ti.key_index.as_ref() {
            Some(k) => self.area_prefix(k)?,
            None => Vec::new(),
        };
        key.push(FieldValue::Ref {
            table: self.number,
            id,
        });
        Ok(key)
    }
}

pub(crate) fn open_read(ex: &Execution, view: &mut View<'_>) -> CallResult<MemoryBudget> {
    let remaining = ex
        .options()
        .memory_limit
        .unwrap_or(u64::MAX)
        .saturating_sub(ex.retained_workspace_bytes());
    view.read_memory(remaining)
        .map_err(|e| store_call_error(ex, e))
}

pub(crate) fn ref_id(v: &Value) -> RefId {
    as_ref(v).map_or(RefId::NIL, |r: RefValue| r.id)
}

pub(crate) fn blank_value(v: &Value) -> bool {
    match v {
        Value::String(s) => numerator::blank(&s.to_string_lossy()),
        Value::Number(n) => n.is_zero(),
        Value::Undefined | Value::Null => true,
        _ => false,
    }
}

pub(crate) struct Series {
    pub key: SeriesKey,
    owner: Option<FieldValue>,
    parent: Option<FieldValue>,
}

impl Layout<'_> {
    pub(crate) fn series(&self, ex: &mut Execution, inst: &ObjectInstance) -> CallResult<Series> {
        let within = self.def.behavior.code_series;
        let by_parent = self.parent.is_some() && within == CodeSeries::WithinSubordination;
        let by_owner =
            self.owner.is_some() && (within == CodeSeries::WithinOwnerSubordination || by_parent);
        let mut series = Series {
            key: SeriesKey {
                obj: self.obj,
                owner: RefId::NIL,
                parent: RefId::NIL,
            },
            owner: None,
            parent: None,
        };
        if by_owner && let Some(i) = self.owner {
            let v = inst.attr(ex, i)?;
            series.key.owner = ref_id(&v);
            series.owner = Some(to_field(self.app, self.field(i)?, &v)?);
        }
        if by_parent && let Some(i) = self.parent {
            let v = inst.attr(ex, i)?;
            series.key.parent = ref_id(&v);
            series.parent = Some(to_field(self.app, self.field(i)?, &v)?);
        }
        Ok(series)
    }

    fn probe(&self, series: &Series) -> Result<(IndexId, Vec<Vec<FieldValue>>), HostError> {
        let mut want: Vec<StdField> = Vec::new();
        let mut tail: Vec<FieldValue> = Vec::new();
        if let Some(o) = &series.owner {
            want.push(StdField::OwnerId);
            tail.push(o.clone());
        }
        if let Some(p) = &series.parent {
            want.push(StdField::ParentId);
            tail.push(p.clone());
        }
        let folder = series.parent.is_some() && self.column(StdField::Folder).is_some();
        if folder {
            want.push(StdField::Folder);
        }
        want.push(StdField::Code);
        let (index, ix) = self
            .index(&want)
            .ok_or_else(|| HostError::not_implemented("Код", "индекс кодов серии"))?;
        let mut base = self.area_prefix(ix)?;
        base.extend(tail);
        let keys = if folder {
            [false, true]
                .into_iter()
                .map(|b| {
                    let mut k = base.clone();
                    k.push(FieldValue::Bool(b));
                    k
                })
                .collect()
        } else {
            vec![base]
        };
        Ok((index, keys))
    }
}

struct Collating {
    icu: bsl_prim::IcuCollation,
}

impl Collating {
    fn of(app: &Application) -> Collating {
        Collating {
            icu: bsl_prim::IcuCollation::new(&app.options().locale),
        }
    }

    fn compare(&self, a: &str, b: &str) -> Ordering {
        let key = |s: &str| {
            let units: Vec<u16> = s.encode_utf16().collect();
            self.icu.try_sort_key_for_sorting(&units).ok()
        };
        match (key(a), key(b)) {
            (Some(x), Some(y)) => x.cmp(&y),
            _ => numerator::fold(a).cmp(&numerator::fold(b)),
        }
    }
}

fn number_of(n: &Numeric) -> Option<u128> {
    if n.is_negative() {
        return Some(0);
    }
    let whole = n.digits().len().saturating_sub(n.scale() as usize);
    n.digits().get(..whole)?.iter().try_fold(0u128, |acc, d| {
        acc.checked_mul(10)?.checked_add(u128::from(*d))
    })
}

enum Largest {
    Text(Option<String>),
    Number(Option<u128>),
}

impl Layout<'_> {
    fn largest(
        &self,
        ex: &mut Execution,
        series: &Series,
        prefix: &str,
        numeric: bool,
    ) -> CallResult<Largest> {
        let (index, keys) = self.probe(series)?;
        let code = self
            .column(StdField::Code)
            .ok_or_else(|| internal("the table has no code"))?;
        let txn = CallTxn::of_call(ex);
        let mut view = read_view(ex, txn.as_deref(), self.app)?;
        let _memory = open_read(ex, &mut view)?;
        let rd = view.txn();
        let collating = Collating::of(self.app);
        let folded = numerator::fold(prefix);
        let fields = [code];
        let mut text: Option<String> = None;
        let mut number: Option<u128> = None;
        for key in &keys {
            if numeric || prefix.is_empty() {
                let mut rows = rd
                    .range_fields(
                        self.table,
                        index,
                        Bound::Included(key.as_slice()),
                        Bound::Included(key.as_slice()),
                        Direction::Backward,
                        &fields,
                    )
                    .map_err(|e| store_call_error(ex, e))?;
                ex.charge(1)?;
                if let Some(item) = rows.next() {
                    let (_, row) = item.map_err(|e| store_call_error(ex, e))?;
                    match row.get(usize::from(code.0)) {
                        Some(FieldValue::String(s)) => {
                            if text
                                .as_deref()
                                .is_none_or(|t| collating.compare(t, s) == Ordering::Less)
                            {
                                text = Some(s.clone());
                            }
                        }
                        Some(FieldValue::Number(n)) => {
                            number = number.max(number_of(n));
                        }
                        _ => {}
                    }
                }
            } else {
                let mut from = key.clone();
                from.push(FieldValue::String(prefix.to_string()));
                let rows = rd
                    .range_fields(
                        self.table,
                        index,
                        Bound::Included(from.as_slice()),
                        Bound::Included(key.as_slice()),
                        Direction::Forward,
                        &fields,
                    )
                    .map_err(|e| store_call_error(ex, e))?;
                let mut last: Option<String> = None;
                for item in rows {
                    ex.charge(1)?;
                    let (_, row) = item.map_err(|e| store_call_error(ex, e))?;
                    let Some(FieldValue::String(s)) = row.get(usize::from(code.0)) else {
                        break;
                    };
                    if !numerator::fold(s).starts_with(&folded) {
                        break;
                    }
                    last = Some(s.clone());
                }
                if let Some(l) = last
                    && text
                        .as_deref()
                        .is_none_or(|t| collating.compare(t, &l) == Ordering::Less)
                {
                    text = Some(l);
                }
            }
        }
        Ok(if numeric {
            Largest::Number(number)
        } else {
            Largest::Text(text)
        })
    }

    fn taken(
        &self,
        ex: &mut Execution,
        series: &Series,
        code: &FieldValue,
        own: Option<RefId>,
    ) -> CallResult<bool> {
        let (index, keys) = self.probe(series)?;
        let id = self.column(StdField::Id);
        let fields: Vec<FieldId> = id.into_iter().collect();
        let txn = CallTxn::of_call(ex);
        let mut view = read_view(ex, txn.as_deref(), self.app)?;
        let _memory = open_read(ex, &mut view)?;
        let rd = view.txn();
        for key in &keys {
            let mut at = key.clone();
            at.push(code.clone());
            let rows = rd
                .range_fields(
                    self.table,
                    index,
                    Bound::Included(at.as_slice()),
                    Bound::Included(at.as_slice()),
                    Direction::Forward,
                    &fields,
                )
                .map_err(|e| store_call_error(ex, e))?;
            for item in rows {
                ex.charge(1)?;
                let (_, row) = item.map_err(|e| store_call_error(ex, e))?;
                let row_id = id
                    .and_then(|f| row.get(usize::from(f.0)))
                    .and_then(|v| match v {
                        FieldValue::Ref { id, .. } => Some(*id),
                        _ => None,
                    });
                if own.is_none() || row_id != own {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

pub(crate) struct NewCodeEvent {
    pub standard: bool,
    pub prefix: String,
}

pub(crate) struct NewCodeArgs {
    standard: Place,
    prefix: Place,
    given: String,
}

impl NewCodeArgs {
    pub(crate) fn new(prefix: &str) -> NewCodeArgs {
        NewCodeArgs {
            standard: Place::temp(Value::Bool(true)),
            prefix: Place::temp(Value::from(prefix)),
            given: prefix.to_string(),
        }
    }

    pub(crate) fn params(&self) -> [Param; 2] {
        [
            Param::Ref(self.standard.clone()),
            Param::Ref(self.prefix.clone()),
        ]
    }

    pub(crate) fn result(&self) -> NewCodeEvent {
        NewCodeEvent {
            standard: !matches!(self.standard.temp_value(), Some(Value::Bool(false))),
            prefix: match self.prefix.temp_value() {
                Some(Value::String(s)) => s.to_string_lossy(),
                _ => self.given.clone(),
            },
        }
    }
}

fn new_code_event(
    ex: &mut Execution,
    inst: &Rc<ObjectInstance>,
    prefix: &str,
) -> CallResult<NewCodeEvent> {
    let args = NewCodeArgs::new(prefix);
    events::run(ex, inst, Event::NewCode, &args.params(), None)?;
    Ok(args.result())
}

fn issue_error(ex: &Execution, e: IssueError<bsl_vm::CallError>) -> bsl_vm::CallError {
    match e {
        IssueError::Read(e) => e,
        IssueError::Full => ex.out_of_memory().into(),
    }
}

fn issue(
    ex: &mut Execution,
    layout: &Layout<'_>,
    series: &Series,
    prefix: &str,
) -> CallResult<Issued> {
    let app = layout.app;
    let len = layout.def.behavior.code_length as usize;
    let numerator = app.numerator().clone();
    let numeric = layout.def.behavior.code_type == CodeType::Number;
    let issued = if numeric {
        numerator.issue_number::<bsl_vm::CallError>(series.key, len, || {
            Ok(match layout.largest(ex, series, prefix, true)? {
                Largest::Number(n) => n,
                Largest::Text(_) => None,
            })
        })
    } else {
        let collating = Collating::of(app);
        numerator.issue_string::<bsl_vm::CallError>(
            series.key,
            len,
            prefix,
            || {
                Ok(match layout.largest(ex, series, prefix, false)? {
                    Largest::Text(t) => t,
                    Largest::Number(_) => None,
                })
            },
            |a, b| collating.compare(a, b),
        )
    }
    .map_err(|e| issue_error(ex, e))?;
    if let Some(token) = issued.token.clone() {
        keep_in_transaction(ex, &numerator, token);
    }
    Ok(issued)
}

fn keep_in_transaction(
    ex: &mut Execution,
    numerator: &Arc<numerator::Numerator>,
    token: numerator::Token,
) {
    let Some(txn) = CallTxn::of_call(ex) else {
        return;
    };
    let Some(id) = txn.id() else {
        return;
    };
    let call = Rc::as_ptr(&txn) as usize;
    let ledger = numerator.ledger(call, id, |ledger| {
        let n = numerator.clone();
        txn.on_rollback(move || n.release(&ledger))
    });
    if let Some(ledger) = ledger {
        ledger.push(token);
    }
}

fn code_value(layout: &Layout<'_>, issued: &Issued) -> Result<Value, HostError> {
    if layout.def.behavior.code_type == CodeType::Number {
        bsl_prim::Number::parse(&issued.code)
            .map(Value::Number)
            .ok_or_else(|| internal("a number code that is not a number"))
    } else {
        Ok(Value::from(issued.code.as_str()))
    }
}

pub(crate) fn set_new_code(
    ex: &mut Execution,
    inst: &Rc<ObjectInstance>,
    layout: &Layout<'_>,
    prefix: &str,
) -> CallResult<()> {
    let Some(code) = layout.code else {
        return Err(HostError::not_implemented(
            &inst.type_name(),
            "УстановитьНовыйКод: у справочника нет кода",
        )
        .into());
    };
    let event = new_code_event(ex, inst, prefix)?;
    if !event.standard {
        return Ok(());
    }
    let series = layout.series(ex, inst)?;
    let issued = issue(ex, layout, &series, &event.prefix)?;
    let value = code_value(layout, &issued)?;
    inst.assign(ex, code, value)
}

pub(crate) fn assign(ex: &mut Execution, ctx: &WriteCtx<'_>) -> CallResult<()> {
    let inst = ctx.inst;
    let layout = Layout::of(&inst.app, inst.obj)?;
    let Some(code) = layout.code else {
        return Ok(());
    };
    if ctx.loading || !layout.def.behavior.autonumbering {
        return Ok(());
    }
    if !blank_value(&inst.attr(ex, code)?) {
        return Ok(());
    }
    set_new_code(ex, inst, &layout, "")
}

pub(crate) fn set_new_code_method(
    ex: &mut Execution,
    inst: &Rc<ObjectInstance>,
    args: &[Arg],
) -> CallResult<Value> {
    let prefix = match arg_value(ex, args, 0)? {
        Value::Undefined | Value::Null => String::new(),
        Value::String(s) => s.to_string_lossy(),
        _ => return Err(type_mismatch(1).into()),
    };
    let app = inst.app.clone();
    admit_data_read(ex)?;
    app.predefined()
        .ensure(&app, inst.obj, &AreaKey::default())?;
    let layout = Layout::of(&app, inst.obj)?;
    set_new_code(ex, inst, &layout, &prefix)?;
    Ok(Value::Undefined)
}

pub(crate) fn check_unique(ex: &mut Execution, ctx: &WriteCtx<'_>) -> CallResult<()> {
    let inst = ctx.inst;
    let layout = Layout::of(&inst.app, inst.obj)?;
    let Some(i) = layout.code else {
        return Ok(());
    };
    if ctx.loading || !layout.def.behavior.check_unique {
        return Ok(());
    }
    let value = inst.attr(ex, i)?;
    let stored = to_field(&inst.app, layout.field(i)?, &value)?;
    let series = layout.series(ex, inst)?;
    let own = if ctx.is_new {
        None
    } else {
        inst.state.borrow().r.map(|r| r.id)
    };
    if layout.taken(ex, &series, &stored, own)? {
        let shown = match &value {
            Value::String(s) => numerator::trimmed(&s.to_string_lossy()).to_string(),
            Value::Number(n) => n.to_xml_string(),
            _ => String::new(),
        };
        return Err(not_unique(&shown).into());
    }
    Ok(())
}
