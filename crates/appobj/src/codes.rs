use crate::Application;
use crate::errors::{self, keys};
use crate::events::{self, Event, Param};
use crate::meta::{arg_value, type_mismatch};
use crate::model::{CodeSeries, CodeType, FieldDef, NumberPeriodicity, ObjDef, ObjId, StdAttr};
use crate::numerator::{self, IssueCtx, IssueError, Issued, SeriesKey};
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

    fn probe(
        &self,
        series: &Series,
        code_field: StdField,
    ) -> Result<(IndexId, Vec<Vec<FieldValue>>), HostError> {
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
        want.push(code_field);
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
        code_field: StdField,
    ) -> CallResult<Largest> {
        let (index, keys) = self.probe(series, code_field)?;
        let code = self
            .column(code_field)
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
        code_field: StdField,
    ) -> CallResult<bool> {
        let (index, keys) = self.probe(series, code_field)?;
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
        IssueError::Unobserved => HostError::not_implemented(
            "Справочник",
            "код после отката при уничтоженном держателе не наблюдён",
        )
        .into(),
        IssueError::Stale => HostError::not_implemented(
            "Справочник",
            "код из пула конца сеанса после выдачи кода другого семейства не наблюдён",
        )
        .into(),
    }
}

fn call_number(txn: &Rc<CallTxn>) -> u64 {
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    thread_local! {
        static LAST: RefCell<Option<(std::rc::Weak<CallTxn>, u64)>> = const { RefCell::new(None) };
    }
    LAST.with(|last| {
        let mut last = last.borrow_mut();
        if let Some((w, n)) = last.as_ref()
            && w.upgrade().is_some_and(|t| Rc::ptr_eq(&t, txn))
        {
            return *n;
        }
        let n = NEXT.fetch_add(1, AtomicOrdering::Relaxed);
        *last = Some((Rc::downgrade(txn), n));
        n
    })
}

fn this_call(ex: &mut Execution) -> Option<u64> {
    CallTxn::of_call(ex).map(|t| call_number(&t))
}

fn issue(
    ex: &mut Execution,
    layout: &Layout<'_>,
    series: &Series,
    prefix: &str,
    holder: numerator::Holder,
) -> CallResult<Issued> {
    let app = layout.app;
    let len = layout.def.behavior.code_length as usize;
    let numerator = app.numerator().clone();
    let numeric = layout.def.behavior.code_type == CodeType::Number;
    let ctx = IssueCtx {
        call: this_call(ex),
        pool: true,
    };
    release_dead(ex, &numerator, series.key);
    let issued = if numeric {
        numerator.issue_number_in::<bsl_vm::CallError>(ctx, series.key, len, || {
            Ok(
                match layout.largest(ex, series, prefix, true, StdField::Code)? {
                    Largest::Number(n) => n,
                    Largest::Text(_) => None,
                },
            )
        })
    } else {
        let collating = Collating::of(app);
        numerator.issue_string_in::<bsl_vm::CallError>(
            ctx,
            series.key,
            len,
            prefix,
            || {
                Ok(
                    match layout.largest(ex, series, prefix, false, StdField::Code)? {
                        Largest::Text(t) => t,
                        Largest::Number(_) => None,
                    },
                )
            },
            |a, b| collating.compare(a, b),
        )
    }
    .map_err(|e| issue_error(ex, e))?;
    if issued.ended && !numeric && stored(ex, layout, series, &issued.code)? {
        if let Some(token) = &issued.token {
            numerator.release_token(token);
        }
        return Err(HostError::not_implemented(
            "Справочник",
            "код из пула конца сеанса, который уже есть в базе, не наблюдён",
        )
        .into());
    }
    if let Some(token) = issued.token.clone() {
        keep_in_transaction(ex, &numerator, token, holder);
    }
    Ok(issued)
}

fn stored(
    ex: &mut Execution,
    layout: &Layout<'_>,
    series: &Series,
    code: &str,
) -> CallResult<bool> {
    let code = numerator::trimmed(code);
    Ok(
        match layout.largest(ex, series, code, false, StdField::Code)? {
            Largest::Text(Some(t)) => {
                numerator::fold(numerator::trimmed(&t)) == numerator::fold(code)
            }
            _ => false,
        },
    )
}

fn release_dead(ex: &mut Execution, numerator: &numerator::Numerator, key: SeriesKey) {
    let Some((txn, id)) = open_txn(ex) else {
        return;
    };
    if let Some(ledger) = numerator.ledger(Rc::as_ptr(&txn) as usize, id, |_| false) {
        numerator.release_dead(&ledger, key, Some(call_number(&txn)), id);
    }
}

fn keep_in_transaction(
    ex: &mut Execution,
    numerator: &Arc<numerator::Numerator>,
    token: numerator::Token,
    holder: numerator::Holder,
) {
    let Some(txn) = CallTxn::of_call(ex) else {
        return;
    };
    let Some(id) = txn.id() else {
        return;
    };
    let call = Rc::as_ptr(&txn) as usize;
    let number = call_number(&txn);
    let ledger = numerator.ledger(call, id, |ledger| {
        let n = numerator.clone();
        txn.on_rollback(move || n.release(&ledger, Some(number)))
    });
    if let Some(ledger) = ledger {
        ledger.push(token, holder);
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

#[derive(Clone)]
pub(crate) struct IssuedInTxn {
    txn: std::rc::Weak<CallTxn>,
    id: u64,
    series: SeriesKey,
    prefix: String,
    code: Value,
    depth: u32,
    loading: bool,
    token: Option<numerator::Token>,
}

fn same_code(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(a), Value::String(b)) => a.to_string_lossy() == b.to_string_lossy(),
        (Value::Number(a), Value::Number(b)) => a.to_xml_string() == b.to_xml_string(),
        _ => false,
    }
}

fn open_txn(ex: &mut Execution) -> Option<(Rc<CallTxn>, u64)> {
    let txn = CallTxn::of_call(ex)?;
    let id = txn.id()?;
    Some((txn, id))
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
    let open = open_txn(ex);
    let earlier = open.as_ref().and_then(|(txn, id)| {
        let st = inst.state.borrow();
        st.issued
            .clone()
            .filter(|p| p.id == *id && p.txn.upgrade().is_some_and(|t| Rc::ptr_eq(&t, txn)))
    });
    if let Some(earlier) = earlier {
        let current = inst.attr(ex, code)?;
        if earlier.series != series.key || !same_code(&current, &earlier.code) {
            return Err(HostError::not_implemented(
                &inst.type_name(),
                "УстановитьНовыйКод: повторно в транзакции после смены серии или кода",
            )
            .into());
        }
        let_go(inst, &earlier);
    }
    let issued = issue(
        ex,
        layout,
        &series,
        &event.prefix,
        inst.code_holder.holder(),
    )?;
    let value = code_value(layout, &issued)?;
    let loading = inst.loading();
    inst.state.borrow_mut().issued = open.map(|(txn, id)| IssuedInTxn {
        txn: Rc::downgrade(&txn),
        id,
        series: series.key,
        prefix: event.prefix.clone(),
        code: value.clone(),
        depth: txn.depth(),
        loading,
        token: issued.token.clone(),
    });
    inst.assign(ex, code, value)
}

fn let_go(inst: &ObjectInstance, issued: &IssuedInTxn) {
    let Some(token) = issued.token.clone() else {
        return;
    };
    let Some(txn) = issued.txn.upgrade() else {
        return;
    };
    let numerator = inst.app.numerator().clone();
    let call = Rc::as_ptr(&txn) as usize;
    if let Some(ledger) = numerator.ledger(call, issued.id, |_| false) {
        let call = Some(call_number(&txn));
        let _ = numerator.let_go(
            &ledger,
            &token,
            token.number(),
            &inst.code_holder.holder(),
            call,
        );
    }
    inst.state.borrow_mut().issued = None;
}

pub(crate) fn manual_code(
    ex: &mut Execution,
    inst: &ObjectInstance,
    index: usize,
    value: &Value,
) -> CallResult<()> {
    let Some(issued) = inst.state.borrow().issued.clone() else {
        return Ok(());
    };
    let layout = Layout::of(&inst.app, inst.obj)?;
    if layout.code != Some(index) || same_code(value, &issued.code) {
        return Ok(());
    }
    let open = open_txn(ex).is_some_and(|(txn, id)| {
        id == issued.id && issued.txn.upgrade().is_some_and(|t| Rc::ptr_eq(&t, &txn))
    });
    if !open {
        return Ok(());
    }
    if !issued.prefix.is_empty() || issued.loading || inst.loading() {
        return Err(HostError::not_implemented(
            &inst.type_name(),
            "Код: ручной код после выдачи с префиксом или при ОбменДанными.Загрузка не наблюдён",
        )
        .into());
    }
    let_go(inst, &issued);
    Ok(())
}

pub(crate) fn assign(ex: &mut Execution, ctx: &WriteCtx<'_>) -> CallResult<()> {
    let inst = ctx.inst;
    inst.code_holder.note_write();
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

fn refuse_after_rollback(
    ex: &mut Execution,
    inst: &ObjectInstance,
    layout: &Layout<'_>,
    prefix: &str,
) -> CallResult<Option<HostError>> {
    let (issued, clean, created_in, stored) = {
        let st = inst.state.borrow();
        let Some(p) = st.issued.clone() else {
            return Ok(None);
        };
        let Some(txn) = p.txn.upgrade().filter(|txn| txn.was_rolled_back(p.id)) else {
            return Ok(None);
        };
        let clean = txn.was_rolled_back_clean(p.id);
        (
            p,
            clean,
            st.created_in.clone(),
            !st.is_new && st.written.is_none(),
        )
    };
    let made_there = created_in.is_some_and(|(txn, id)| id == issued.id && txn.ptr_eq(&issued.txn));
    let current = match layout.code {
        Some(i) => Some(inst.attr(ex, i)?),
        None => None,
    };
    let case = if !made_there && stored {
        Some("объект прочитан из базы")
    } else if !made_there {
        Some("объект создан не СоздатьЭлемент() в этой транзакции")
    } else if issued.depth != 1 {
        Some("код выдан во вложенной транзакции")
    } else if !clean {
        Some("откат отравленной транзакции")
    } else if !issued.prefix.is_empty() || !prefix.is_empty() {
        Some("с префиксом")
    } else if issued.loading || inst.loading() {
        Some("ОбменДанными.Загрузка")
    } else if !current.is_some_and(|c| same_code(&c, &issued.code)) {
        Some("код изменён после выдачи")
    } else if open_txn(ex).is_none() {
        Some("вне транзакции")
    } else {
        None
    };
    Ok(Some(match case {
        None => crate::objects::not_selected(),
        Some(case) => HostError::not_implemented(
            &inst.type_name(),
            &format!(
                "УстановитьНовыйКод: повторный вызов после отката транзакции не наблюдён для этого случая ({case})"
            ),
        ),
    }))
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
    let layout = Layout::of(&app, inst.obj)?;
    if let Some(refused) = refuse_after_rollback(ex, inst, &layout, &prefix)? {
        return Err(refused.into());
    }
    admit_data_read(ex)?;
    app.predefined()
        .ensure(&app, inst.obj, &AreaKey::default())?;
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
    if layout.taken(ex, &series, &stored, own, StdField::Code)? {
        let shown = match &value {
            Value::String(s) => numerator::trimmed(&s.to_string_lossy()).to_string(),
            Value::Number(n) => n.to_xml_string(),
            _ => String::new(),
        };
        return Err(not_unique(&shown).into());
    }
    Ok(())
}

fn number_refusal(inst: &ObjectInstance, what: &str) -> HostError {
    HostError::not_implemented(&inst.type_name(), &format!("Номер: {what}"))
}

fn number_of_document(inst: &ObjectInstance, layout: &Layout<'_>) -> CallResult<Option<usize>> {
    let def = layout.def;
    let Some(i) = def
        .fields
        .iter()
        .position(|f| f.std == Some(StdAttr::Number))
    else {
        return Ok(None);
    };
    let b = &def.behavior;
    if b.number_type != CodeType::String {
        return Err(number_refusal(inst, "номер типа Число").into());
    }
    if b.number_periodicity != NumberPeriodicity::Nonperiodical {
        return Err(number_refusal(inst, "периодичность номера").into());
    }
    let md = layout.app.model().config.object(def.md);
    if md.is_some_and(
        |o| matches!(o.prop("NumberAllowedLength"), Some(md_model::Value::Text(t)) if t == "Fixed"),
    ) {
        return Err(number_refusal(inst, "номер фиксированной длины").into());
    }
    if b.numerator.is_some() {
        return Err(number_refusal(inst, "общий нумератор").into());
    }
    if events::handled(inst, Event::NewNumber) {
        return Err(number_refusal(inst, "обработчик ПриУстановкеНовогоНомера").into());
    }
    Ok(Some(i))
}

fn number_series(layout: &Layout<'_>) -> Series {
    Series {
        key: SeriesKey {
            obj: layout.obj,
            owner: RefId::NIL,
            parent: RefId::NIL,
        },
        owner: None,
        parent: None,
    }
}

fn set_new_number(
    ex: &mut Execution,
    inst: &Rc<ObjectInstance>,
    layout: &Layout<'_>,
    i: usize,
) -> CallResult<()> {
    let app = layout.app;
    let series = number_series(layout);
    let numerator = app.numerator().clone();
    if let Some(refused) = number_after_rollback(ex, inst) {
        return Err(refused.into());
    }
    if let Some((txn, id)) = open_txn(ex)
        && let Some(ledger) = numerator.ledger(Rc::as_ptr(&txn) as usize, id, |_| false)
    {
        let writers =
            dead_writers_in_base(ex, &numerator, &ledger, layout, &series, i, txn.poisoned())?;
        if writers.is_some() {
            numerator.release_dead(&ledger, series.key, Some(call_number(&txn)), id);
        }
        if writers.is_none_or(|n| n > 0 && numerator.pooled(series.key)) {
            return Err(number_refusal(
                inst,
                "номер после уничтожения в транзакции держателя, начавшего запись, не наблюдён",
            )
            .into());
        }
    }
    if numerator.number_unobserved(series.key, this_call(ex), open_txn(ex).map(|(_, id)| id)) {
        return Err(number_refusal(
            inst,
            "номер документа после отката при уничтоженном держателе не наблюдён",
        )
        .into());
    }
    let open = open_txn(ex);
    let again =
        open.as_ref().is_some_and(|(txn, id)| {
            inst.state.borrow().issued.as_ref().is_some_and(|p| {
                p.id == *id && p.txn.upgrade().is_some_and(|t| Rc::ptr_eq(&t, txn))
            })
        });
    if again {
        return Err(
            number_refusal(inst, "второй номер объекта в одной транзакции не наблюдён").into(),
        );
    }
    let len = layout.def.behavior.number_length as usize;
    let collating = Collating::of(app);
    let ctx = IssueCtx {
        call: this_call(ex),
        pool: true,
    };
    let issued = numerator
        .issue_string_in::<bsl_vm::CallError>(
            ctx,
            series.key,
            len,
            "",
            || {
                Ok(
                    match layout.largest(ex, &series, "", false, StdField::Number)? {
                        Largest::Text(t) => t,
                        Largest::Number(_) => None,
                    },
                )
            },
            |a, b| collating.compare(a, b),
        )
        .map_err(|e| issue_error(ex, e))?;
    if let Some(token) = issued.token.clone() {
        keep_in_transaction(ex, &numerator, token, inst.code_holder.holder());
    }
    let value = Value::from(issued.code.as_str());
    let loading = inst.loading();
    inst.state.borrow_mut().issued = open.map(|(txn, id)| IssuedInTxn {
        txn: Rc::downgrade(&txn),
        id,
        series: series.key,
        prefix: String::new(),
        code: value.clone(),
        depth: txn.depth(),
        loading,
        token: issued.token.clone(),
    });
    inst.assign(ex, i, value)
}

fn dead_writers_in_base(
    ex: &mut Execution,
    numerator: &numerator::Numerator,
    ledger: &numerator::Ledger,
    layout: &Layout<'_>,
    series: &Series,
    i: usize,
    poisoned: bool,
) -> CallResult<Option<usize>> {
    let Some(dead) = numerator.dead_writers(ledger, series.key) else {
        return Ok(None);
    };
    if dead.is_empty() {
        return Ok(Some(0));
    }
    if poisoned || dead.iter().filter(|(_, h)| !h.seen()).count() >= 2 {
        return Ok(None);
    }
    let field = layout.field(i)?;
    let deletions = numerator.deletions();
    for (code, holder) in &dead {
        holder.note_seen();
        if holder.found(deletions) {
            continue;
        }
        let stored = to_field(layout.app, field, &Value::from(code.as_str()))?;
        if !layout.taken(ex, series, &stored, None, StdField::Number)? {
            return Ok(None);
        }
        holder.note_found(deletions);
    }
    Ok(Some(dead.len()))
}

pub(crate) fn assign_number(ex: &mut Execution, ctx: &WriteCtx<'_>) -> CallResult<()> {
    let inst = ctx.inst;
    inst.code_holder.note_write();
    let layout = Layout::of(&inst.app, inst.obj)?;
    if ctx.loading || !layout.def.behavior.autonumbering {
        return Ok(());
    }
    let Some(i) = number_of_document(inst, &layout)? else {
        return Ok(());
    };
    if !blank_value(&inst.attr(ex, i)?) {
        return Ok(());
    }
    set_new_number(ex, inst, &layout, i)
}

fn number_after_rollback(ex: &mut Execution, inst: &ObjectInstance) -> Option<HostError> {
    let observed = {
        let st = inst.state.borrow();
        let p = st.issued.clone()?;
        let txn = p.txn.upgrade().filter(|t| t.was_rolled_back(p.id))?;
        let made_there = st
            .created_in
            .as_ref()
            .is_some_and(|(t, id)| *id == p.id && t.ptr_eq(&p.txn));
        txn.was_rolled_back_clean(p.id) && made_there && p.depth == 1 && !p.loading
    };
    if observed && !inst.loading() && open_txn(ex).is_some() {
        let mut e = match errors::msg(keys::DOCUMENT_NOT_SELECTED) {
            Some(m) => HostError::text(m.ru, m.en),
            None => HostError::text(
                &format!("internal: {}", keys::DOCUMENT_NOT_SELECTED),
                &format!("internal: {}", keys::DOCUMENT_NOT_SELECTED),
            ),
        };
        e.categories = crate::objects::not_selected().categories;
        return Some(e);
    }
    Some(number_refusal(
        inst,
        "УстановитьНовыйНомер после отката транзакции в этой форме не наблюдён",
    ))
}

pub(crate) fn set_new_number_method(
    ex: &mut Execution,
    inst: &Rc<ObjectInstance>,
    args: &[Arg],
) -> CallResult<Value> {
    match arg_value(ex, args, 0)? {
        Value::Undefined => {}
        Value::String(s) if s.to_string_lossy().is_empty() => {}
        Value::String(_) => return Err(number_refusal(inst, "префикс номера").into()),
        _ => return Err(type_mismatch(1).into()),
    }
    admit_data_read(ex)?;
    if !inst.state.borrow().is_new {
        return Err(number_refusal(inst, "новый номер записанного документа").into());
    }
    let layout = Layout::of(&inst.app, inst.obj)?;
    let Some(i) = number_of_document(inst, &layout)? else {
        return Err(number_refusal(inst, "у документа нет номера").into());
    };
    set_new_number(ex, inst, &layout, i)?;
    Ok(Value::Undefined)
}

pub(crate) fn check_unique_number(ex: &mut Execution, ctx: &WriteCtx<'_>) -> CallResult<()> {
    let inst = ctx.inst;
    let layout = Layout::of(&inst.app, inst.obj)?;
    if ctx.loading || !layout.def.behavior.check_unique {
        return Ok(());
    }
    let Some(i) = number_of_document(inst, &layout)? else {
        return Ok(());
    };
    let value = inst.attr(ex, i)?;
    let stored = to_field(&inst.app, layout.field(i)?, &value)?;
    let own = if ctx.is_new {
        None
    } else {
        inst.state.borrow().r.map(|r| r.id)
    };
    if layout.taken(ex, &number_series(&layout), &stored, own, StdField::Number)? {
        return Err(number_refusal(inst, "номер уже занят другим документом").into());
    }
    Ok(())
}
