use crate::Application;
use crate::app::QueryCtx;
use crate::errors::{self, keys};
use crate::meta::{arg_value, member_name, type_mismatch};
use crate::model::{CodeType, NumberPeriodicity, ObjId, ObjKind, TypeAtom};
use crate::queryhost::{CallBindHost, exec_error, to_value};
use crate::read::{field_index, store_error, tabular_index};
use crate::readtab::tabular_value;
use crate::session::call_ctx;
use crate::types::Category;
use crate::values::field::default_of;
use crate::values::refs::{RefValue, as_ref, empty_ref};
use bsl_prim::BslDate;
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, MemberKind, TypeId, Value,
};
use ibschema::values::RefId;
use platform_catalog::messages::Lang;
use query_exec::{ExecEnv, ExecEnvOwned, RowStream, execute_store, stream};
use query_ir::build::{BuildValue, Selection};
use query_ir::{BindCtx, FieldRole, ParamValue, ParamValues, QFieldId, QStr, QValue, StdRole};
use std::any::Any;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

const ATTR_BASE: u32 = 1 << 20;
const TAB_BASE: u32 = 1 << 21;
const MAX_LIMBS: u64 = 4096;

#[derive(Default)]
pub(crate) struct SelectSpec {
    pub parent: Option<Value>,
    pub owner: Option<Value>,
    pub filter: Option<(String, Value)>,
    pub order: Vec<(String, bool)>,
    pub hierarchical: bool,
    pub parent_no: usize,
    pub owner_no: usize,
    pub period: Option<(Value, Value)>,
    pub starts_with: Option<(String, String)>,
    pub exact: Option<(String, Value)>,
    pub top: Option<u64>,
    pub ref_order: bool,
}

pub(crate) struct Params {
    items: Vec<(String, Value)>,
}

impl Params {
    pub fn collect(ex: &mut Execution, args: &[Arg], names: &[String]) -> CallResult<Params> {
        let mut items = Vec::with_capacity(names.len());
        for (i, n) in names.iter().enumerate() {
            items.push((n.clone(), arg_value(ex, args, i)?));
        }
        Ok(Params { items })
    }

    fn raw(&self, name: &str) -> Option<&Value> {
        self.items.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }

    fn opt(&self, name: &str) -> Option<&Value> {
        self.raw(name)
            .filter(|v| !matches!(v, Value::Undefined | Value::Null))
    }

    fn number(&self, name: &str) -> usize {
        self.items
            .iter()
            .position(|(n, _)| n == name)
            .map_or(0, |i| i + 1)
    }
}

fn internal(text: &str) -> HostError {
    HostError::text(&format!("internal: {text}"), &format!("internal: {text}"))
}

fn l5(key: &'static str, args: &[&str]) -> HostError {
    HostError::text(
        &errors::render(key, args, Lang::Ru),
        &errors::render(key, args, Lang::En),
    )
}

fn value_mismatch(n: usize) -> HostError {
    l5(keys::PARAMETER_VALUE_MISMATCH, &[&n.to_string()])
}

fn cant_order() -> HostError {
    l5(keys::CANT_ORDER_BY_THIS_FIELD, &[])
}

fn wrong_attribute() -> HostError {
    l5(keys::WRONG_ATTRIBUTE, &[])
}

struct NoValues;

impl ParamValues for NoValues {
    fn get(&self, _: &str) -> Option<ParamValue> {
        None
    }
}

fn value_to_q(app: &Application, v: &Value) -> Option<QValue> {
    Some(match v {
        Value::Bool(b) => QValue::Bool(*b),
        Value::Number(n) => QValue::Number(n.clone()),
        Value::String(s) => QValue::String(QStr::from_str(&s.to_string_lossy())),
        Value::Date(d) => QValue::Date(*d),
        Value::Object(_) => {
            if let Some(r) = as_ref(v) {
                let table = app.model().storage_of(r.obj)?.ref_number?;
                return Some(QValue::Ref { table, id: r.id.0 });
            }
            QValue::Uuid(bsl_stdlib::values::uuid_bytes(v)?)
        }
        _ => return None,
    })
}

fn admits(app: &Application, ts: &crate::model::TypeSet, q: &QValue) -> bool {
    if ts.atoms.is_empty() {
        return true;
    }
    ts.atoms.iter().any(|a| match (a, q) {
        (TypeAtom::Bool, QValue::Bool(_))
        | (TypeAtom::Number, QValue::Number(_))
        | (TypeAtom::String, QValue::String(_))
        | (TypeAtom::Date, QValue::Date(_))
        | (TypeAtom::Uuid, QValue::Uuid(_)) => true,
        (TypeAtom::Ref(o), QValue::Ref { table, .. }) => {
            app.model().obj_of_ref_number(*table) == Some(*o)
        }
        (TypeAtom::AnyRef(kind), QValue::Ref { table, .. }) => app
            .model()
            .obj_of_ref_number(*table)
            .is_some_and(|o| kind.is_none_or(|k| app.model().obj(o).kind == k)),
        _ => false,
    })
}

struct Prepared {
    ctx: Arc<QueryCtx>,
    stmt: query_ir::ir::StmtS,
    cols: Vec<Option<usize>>,
    ref_col: Option<usize>,
    parent_col: Option<usize>,
    version_col: Option<usize>,
    subtree: Option<RefId>,
    empty: bool,
}

fn std_field(qt: &query_ir::QTable, role: StdRole) -> Option<QFieldId> {
    qt.fields
        .iter()
        .position(|f| f.role == FieldRole::Std(role))
        .and_then(|i| u16::try_from(i).ok())
        .map(QFieldId)
}

fn ref_q(app: &Application, r: RefValue) -> Option<QValue> {
    let table = app.model().storage_of(r.obj)?.ref_number?;
    Some(QValue::Ref { table, id: r.id.0 })
}

fn prepare(app: &Application, obj: ObjId, spec: &SelectSpec) -> Result<Prepared, HostError> {
    app.predefined()
        .ensure(app, obj, &crate::predefined::AreaKey::default())?;
    let def = app.model().obj(obj);
    if spec.hierarchical && !def.behavior.hierarchical {
        return Err(l5(keys::NON_HIERARCHICAL, &[]));
    }
    let ctx = app.query_ctx()?.clone();
    let t = ctx
        .schema
        .object_table(def.md)
        .ok_or_else(|| internal("the object has no query table"))?;
    let qt = ctx.schema.table(t);
    let by_name = |name: &str| ctx.schema.find_field(t, name);
    let mut cols: Vec<QFieldId> = qt
        .star
        .iter()
        .copied()
        .filter(|f| {
            qt.fields.get(usize::from(f.0)).is_some_and(|x| {
                x.nested.is_none() && x.role != FieldRole::Std(StdRole::PredefinedDataName)
            })
        })
        .collect();
    let mut want = vec![
        std_field(qt, StdRole::Ref),
        std_field(qt, StdRole::Parent),
        std_field(qt, StdRole::Predefined),
        std_field(qt, StdRole::DataVersion),
    ];
    if def.behavior.predefined_count > 0 {
        want.push(std_field(qt, StdRole::PredefinedDataName));
    }
    for f in want.into_iter().flatten() {
        if !cols.contains(&f) {
            cols.push(f);
        }
    }
    let col_of = |f: Option<QFieldId>| f.and_then(|f| cols.iter().position(|c| *c == f));
    let ref_col = col_of(std_field(qt, StdRole::Ref));
    let parent_col = col_of(std_field(qt, StdRole::Parent));
    let version_col = col_of(std_field(qt, StdRole::DataVersion));
    let colmap: Vec<Option<usize>> = cols
        .iter()
        .map(|c| {
            def.fields
                .iter()
                .position(|fd| by_name(&fd.name) == Some(*c))
        })
        .collect();

    let mut sel = Selection::of(&ctx.schema, t).columns(&cols);
    let mut empty = false;
    let mut subtree = None;

    let numbers = (
        if spec.parent_no == 0 {
            1
        } else {
            spec.parent_no
        },
        if spec.owner_no == 0 { 2 } else { spec.owner_no },
    );
    for (value, role, number) in [
        (&spec.parent, StdRole::Parent, numbers.0),
        (&spec.owner, StdRole::Owner, numbers.1),
    ] {
        let Some(v) = value
            .as_ref()
            .filter(|v| !matches!(v, Value::Undefined | Value::Null))
        else {
            continue;
        };
        let Some(r) = as_ref(v) else {
            return Err(type_mismatch(number));
        };
        let Some(f) = std_field(qt, role) else {
            continue;
        };
        let right_object = if role == StdRole::Parent {
            r.obj == obj
        } else {
            def.behavior.owners.contains(&app.model().obj(r.obj).md)
        };
        if !right_object {
            return Err(type_mismatch(number));
        }
        if role == StdRole::Parent && spec.hierarchical {
            if !r.is_empty() {
                subtree = Some(r.id);
            }
            continue;
        }
        match ref_q(app, r) {
            Some(q) => sel = sel.eq(f, BuildValue::Value(q)),
            None => empty = true,
        }
    }

    let resolve_eq = |name: &str,
                      v: &Value,
                      number: usize,
                      strict: bool|
     -> Result<Option<(QFieldId, QValue)>, HostError> {
        let fd = field_index(def, name)
            .and_then(|i| def.fields.get(i))
            .ok_or_else(|| value_mismatch(number))?;
        let f = by_name(&fd.name).ok_or_else(|| value_mismatch(number))?;
        let q = value_to_q(app, v).filter(|q| admits(app, &fd.types, q));
        if q.is_none() && strict {
            return Err(value_mismatch(number));
        }
        Ok(q.map(|q| (f, q)))
    };
    for (pair, number, strict) in [(&spec.filter, 3, true), (&spec.exact, 1, false)] {
        if let Some((name, v)) = pair {
            match resolve_eq(name, v, number, strict)? {
                Some((f, q)) => sel = sel.eq(f, BuildValue::Value(q)),
                None => empty = true,
            }
        }
    }
    if let Some((name, prefix)) = &spec.starts_with {
        let f = field_index(def, name)
            .and_then(|i| def.fields.get(i))
            .and_then(|fd| by_name(&fd.name))
            .ok_or_else(|| internal("no field for the prefix"))?;
        sel = sel.starts_with(f, BuildValue::Value(QValue::String(QStr::from_str(prefix))));
    }
    if let Some((lo, hi)) = &spec.period {
        let f = std_field(qt, StdRole::Date).ok_or_else(|| internal("no date field"))?;
        let end = |v: &Value, number: usize| match v {
            Value::Date(d) if *d != BslDate::EMPTY => Ok(Some(BuildValue::Value(QValue::Date(*d)))),
            Value::Date(_) | Value::Undefined | Value::Null => Ok(None),
            _ => Err(type_mismatch(number)),
        };
        let (lo, hi) = (end(lo, 1)?, end(hi, 2)?);
        if lo.is_some() || hi.is_some() {
            sel = sel.range(f, lo, hi);
        }
    }

    let ref_f = std_field(qt, StdRole::Ref);
    let mut keys: Vec<(QFieldId, bool)> = Vec::new();
    if !spec.ref_order {
        if spec.hierarchical
            && let Some(f) = std_field(qt, StdRole::IsFolder)
        {
            keys.push((f, spec.order.first().is_none_or(|(_, desc)| !*desc)));
        }
        if spec.order.is_empty() {
            let by_code = def.behavior.present_by_code;
            let (d, c) = (
                std_field(qt, StdRole::Description),
                std_field(qt, StdRole::Code),
            );
            let main =
                if by_code { c.or(d) } else { d.or(c) }.or_else(|| std_field(qt, StdRole::Date));
            keys.extend(main.map(|f| (f, false)));
        } else {
            for (name, desc) in &spec.order {
                let f = field_index(def, name)
                    .and_then(|i| def.fields.get(i))
                    .and_then(|fd| by_name(&fd.name))
                    .ok_or_else(cant_order)?;
                keys.push((f, *desc));
            }
        }
    }
    if let Some(f) = ref_f
        && !keys.iter().any(|(k, _)| *k == f)
    {
        keys.push((f, false));
    }
    sel = sel.order(&keys);
    if spec.hierarchical {
        sel = sel.hierarchical();
    }
    if let Some(n) = spec.top {
        sel = sel.top(n);
    }
    let stmt = sel
        .build()
        .map_err(|e| internal(&format!("selection: {e}")))?;
    Ok(Prepared {
        ctx,
        stmt,
        cols: colmap,
        ref_col,
        parent_col,
        version_col,
        subtree: subtree.filter(|_| ref_col.is_some() && parent_col.is_some()),
        empty,
    })
}

struct InterimSelectionSecurity;

impl InterimSelectionSecurity {
    fn for_call(ex: &mut Execution) -> Result<Self, HostError> {
        if call_ctx(ex).is_some_and(|c| c.sess.user().no_users) {
            Ok(Self)
        } else {
            Err(HostError::not_implemented(
                "P6",
                "selection security provider",
            ))
        }
    }
}

impl query_ir::AccessPolicy for InterimSelectionSecurity {
    fn read_policy(
        &self,
        _: &query_ir::SourceTable,
        _: &query_ir::FieldSet,
    ) -> Result<query_ir::Access, query_ir::HostError> {
        Ok(query_ir::Access::Unrestricted)
    }
}

impl query_ir::SessionValues for InterimSelectionSecurity {
    fn session_param(&self, name: &str) -> Result<QValue, query_ir::HostError> {
        Err(query_ir::HostError::new(format!(
            "P6: session parameter {name}"
        )))
    }

    fn separators(&self) -> Result<Vec<query_ir::SeparatorFilter>, query_ir::HostError> {
        Ok(Vec::new())
    }

    fn user(&self) -> Option<&query_ir::UserRef> {
        None
    }
}

fn bind(
    app: &Application,
    ctx: &QueryCtx,
    stmt: &query_ir::ir::StmtS,
    security: &InterimSelectionSecurity,
) -> Result<query_ir::StmtL, HostError> {
    let host = CallBindHost::new(app);
    query_ir::bind(
        stmt,
        &BindCtx {
            schema: &ctx.schema,
            storage: &ctx.binding,
            map: &ctx.map,
            params: &NoValues,
            host: &host,
            security: query_ir::host::SecurityCtx {
                access: security,
                session: security,
            },
        },
    )
    .map_err(|e| exec_error(&e))
}

pub(crate) const MAX_LIVE_STREAMS: usize = 64;

struct StreamGuard(Arc<std::sync::atomic::AtomicUsize>);

impl StreamGuard {
    fn take(counter: &Arc<std::sync::atomic::AtomicUsize>) -> Option<StreamGuard> {
        use std::sync::atomic::Ordering::SeqCst;
        if counter.fetch_add(1, SeqCst) >= MAX_LIVE_STREAMS {
            counter.fetch_sub(1, SeqCst);
            return None;
        }
        Some(StreamGuard(counter.clone()))
    }
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

enum Src {
    Pending(Box<(Arc<QueryCtx>, query_ir::ir::StmtS)>),
    Running(RowStream<'static>, #[allow(dead_code)] StreamGuard),
    Rows(std::collections::VecDeque<Vec<QValue>>),
    Done,
}

struct Subtree {
    ids: HashSet<[u8; 16]>,
    root: [u8; 16],
    started: bool,
    ref_col: usize,
    parent_col: usize,
}

enum Verdict {
    Skip,
    Take,
    Stop,
}

impl Subtree {
    fn judge(&mut self, row: &[QValue]) -> Verdict {
        let id = match row.get(self.ref_col) {
            Some(QValue::Ref { id, .. }) => *id,
            _ => return Verdict::Skip,
        };
        if !self.started {
            if id == self.root {
                self.started = true;
                self.ids.insert(id);
            }
            return Verdict::Skip;
        }
        match row.get(self.parent_col) {
            Some(QValue::Ref { id: p, .. }) if self.ids.contains(p) => {
                self.ids.insert(id);
                Verdict::Take
            }
            _ => Verdict::Stop,
        }
    }
}

struct State {
    src: Src,
    current: Option<Vec<QValue>>,
    sub: Option<Subtree>,
    level: i64,
    levels: Option<std::collections::HashMap<[u8; 16], i64>>,
}

pub struct SelectionObject {
    app: Arc<Application>,
    obj: ObjId,
    ty: TypeId,
    cols: Vec<Option<usize>>,
    ref_col: Option<usize>,
    parent_col: Option<usize>,
    version_col: Option<usize>,
    state: RefCell<State>,
}

impl SelectionObject {
    pub(crate) fn release(&self) {
        if let Ok(mut st) = self.state.try_borrow_mut() {
            st.src = Src::Done;
        }
    }

    fn start(&self, ex: &mut Execution, st: &mut State) -> Result<(), HostError> {
        let Src::Pending(p) = std::mem::replace(&mut st.src, Src::Done) else {
            return Ok(());
        };
        let (ctx, stmt) = *p;
        let security = InterimSelectionSecurity::for_call(ex)?;
        let lowered = bind(&self.app, &ctx, &stmt, &security)?;
        let txn = self.app.store().read().map_err(|e| store_error(&e))?;
        let Some(guard) = StreamGuard::take(self.app.live_streams()) else {
            let cancel = AtomicBool::new(false);
            let env = ExecEnv {
                collation: ctx.collation.as_ref(),
                cancel: &cancel,
                stop: None,
                mem_limit: self.app.options().query_mem_limit,
                max_number_limbs: MAX_LIMBS,
                memory: None,
                lease: None,
                evictor: None,
            };
            let res = execute_store(&lowered, &*txn, &ctx.map, &env).map_err(|e| exec_error(&e))?;
            drop(txn);
            st.src = Src::Rows(res.rows.into());
            return Ok(());
        };
        let env = ExecEnvOwned {
            collation: ctx.collation.clone(),
            cancel: Arc::new(AtomicBool::new(false)),
            mem_limit: self.app.options().query_mem_limit,
            max_number_limbs: MAX_LIMBS,
        };
        let rows =
            stream(Arc::new(lowered), txn, ctx.map.clone(), env).map_err(|e| exec_error(&e))?;
        st.src = Src::Running(rows, guard);
        Ok(())
    }

    fn advance(&self, ex: &mut Execution) -> Result<bool, HostError> {
        let mut st = self
            .state
            .try_borrow_mut()
            .map_err(|_| internal("selection re-entered"))?;
        if matches!(st.src, Src::Pending(_)) {
            self.start(ex, &mut st)?;
        }
        loop {
            let item = match &mut st.src {
                Src::Running(rows, _) => rows.next(),
                Src::Rows(rows) => rows.pop_front().map(Ok),
                _ => return Ok(false),
            };
            let row = match item {
                None => {
                    st.src = Src::Done;
                    return Ok(false);
                }
                Some(Err(e)) => {
                    st.src = Src::Done;
                    return Err(exec_error(&e));
                }
                Some(Ok(row)) => row,
            };
            if let Some(sub) = st.sub.as_mut() {
                match sub.judge(&row) {
                    Verdict::Skip => continue,
                    Verdict::Take => {}
                    Verdict::Stop => {
                        st.src = Src::Done;
                        return Ok(false);
                    }
                }
            }
            st.level = self.level_of(&mut st, &row);
            st.current = Some(row);
            return Ok(true);
        }
    }

    fn level_of(&self, st: &mut State, row: &[QValue]) -> i64 {
        let (Some(levels), Some(rc)) = (st.levels.as_mut(), self.ref_col) else {
            return 0;
        };
        let parent = match self.parent_col.and_then(|c| row.get(c)) {
            Some(QValue::Ref { id, .. }) => levels.get(id).map(|l| l + 1),
            _ => None,
        };
        let level = parent.unwrap_or(0);
        if let Some(QValue::Ref { id, .. }) = row.get(rc) {
            levels.insert(*id, level);
        }
        level
    }

    fn current_ref(&self) -> Option<RefValue> {
        let st = self.state.borrow();
        match st.current.as_ref()?.get(self.ref_col?)? {
            QValue::Ref { id, .. } => Some(RefValue {
                obj: self.obj,
                id: RefId(*id),
            }),
            _ => None,
        }
    }

    fn field(&self, i: usize) -> Result<Value, HostError> {
        let def = self.app.model().obj(self.obj);
        let fd = def.fields.get(i);
        let st = self.state.borrow();
        let Some(row) = st.current.as_ref() else {
            return Ok(Value::Undefined);
        };
        match self
            .cols
            .iter()
            .position(|c| *c == Some(i))
            .and_then(|c| row.get(c))
        {
            Some(q) => to_value(&self.app, q, fd),
            None => match fd {
                Some(fd) => default_of(&self.app, fd),
                None => Ok(Value::Undefined),
            },
        }
    }

    fn data_version(&self) -> Value {
        let st = self.state.borrow();
        match st
            .current
            .as_ref()
            .zip(self.version_col)
            .and_then(|(row, c)| row.get(c))
        {
            Some(QValue::String(t)) => Value::from(t.to_string_lossy().as_str()),
            _ => Value::Undefined,
        }
    }

    fn member(&self, ex: &Execution, m: MemberId) -> Option<(bool, &'static str)> {
        let t = ex.runtime().types().app_type(self.ty)?.template?;
        member_name(platform_catalog::catalog(), t, m).map(|(method, ru, _)| (method, ru))
    }

    fn not_implemented(&self, member: &str) -> HostError {
        let kind = self.app.model().obj(self.obj).kind.name(Lang::Ru);
        HostError::not_implemented(&format!("{kind}Выборка"), member)
    }
}

impl HostObject for SelectionObject {
    fn type_id(&self) -> TypeId {
        self.ty
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn presentation(&self, _ex: &mut Execution) -> CallResult<Option<bsl_prim::Str>> {
        Ok(Some(bsl_prim::Str::new()))
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        if (ATTR_BASE..TAB_BASE).contains(&m.0) {
            return Ok(self.field((m.0 - ATTR_BASE) as usize)?);
        }
        if m.0 >= TAB_BASE {
            let tab = (m.0 - TAB_BASE) as usize;
            return match self.current_ref() {
                Some(r) => tabular_value(ex, &self.app, r, tab),
                None => Ok(Value::Undefined),
            };
        }
        match self.member(ex, m) {
            Some((false, "ВерсияДанных")) => Ok(self.data_version()),
            Some((false, ru)) => {
                let def = self.app.model().obj(self.obj);
                match field_index(def, ru) {
                    Some(i) => Ok(self.field(i)?),
                    None if ru == "ЭтоГруппа" && def.behavior.hierarchical => {
                        Ok(match self.state.borrow().current {
                            Some(_) => Value::Bool(false),
                            None => Value::Undefined,
                        })
                    }
                    None => Err(self.not_implemented(ru).into()),
                }
            }
            Some((true, ru)) => Err(self.not_implemented(ru).into()),
            None => Err(self.not_implemented(&m.0.to_string()).into()),
        }
    }

    fn set(&self, ex: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        let name = self.member(ex, m).map_or("", |n| n.1);
        Err(HostError::key(bsl_stdlib::bind::msg::PROP_NOT_WRITABLE)
            .with_arg(name)
            .raw()
            .into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        match self.member(ex, m) {
            Some((true, "Следующий")) => Ok(Value::Bool(self.advance(ex)?)),
            Some((true, "ПолучитьОбъект")) => match self.current_ref() {
                Some(r) => crate::objects::get_object(ex, &self.app, r),
                None => Err(crate::objects::not_selected().into()),
            },
            Some((true, "УровеньВВыборке")) => {
                Ok(Value::from(self.state.borrow().level))
            }
            Some((_, ru)) => Err(self.not_implemented(ru).into()),
            None => Err(self.not_implemented(&m.0.to_string()).into()),
        }
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        let def = self.app.model().obj(self.obj);
        if let Some(i) = field_index(def, name).and_then(|i| u32::try_from(i).ok()) {
            return Some(MemberId(ATTR_BASE + i));
        }
        tabular_index(def, name)
            .and_then(|i| u32::try_from(i).ok())
            .map(|i| MemberId(TAB_BASE + i))
    }

    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        (m.0 >= ATTR_BASE).then_some(MemberKind::Property {
            readable: true,
            writable: false,
        })
    }
}

pub(crate) fn select(
    ex: &mut Execution,
    app: &Application,
    obj: ObjId,
    spec: SelectSpec,
) -> CallResult<Value> {
    InterimSelectionSecurity::for_call(ex)?;
    let arc = app
        .arc()
        .ok_or_else(|| internal("the application is gone"))?;
    let p = prepare(app, obj, &spec)?;
    let ty = app
        .types()
        .and_then(|t| t.type_of(obj, Category::Selection))
        .ok_or_else(|| internal("no selection type"))?;
    let sub = p.subtree.and_then(|root| {
        Some(Subtree {
            ids: HashSet::new(),
            root: root.0,
            started: false,
            ref_col: p.ref_col?,
            parent_col: p.parent_col?,
        })
    });
    let src = if p.empty {
        Src::Done
    } else {
        Src::Pending(Box::new((p.ctx.clone(), p.stmt)))
    };
    let sel = Rc::new(SelectionObject {
        app: arc,
        obj,
        ty,
        cols: p.cols,
        ref_col: p.ref_col,
        parent_col: p.parent_col,
        version_col: p.version_col,
        state: RefCell::new(State {
            src,
            current: None,
            sub,
            level: -1,
            levels: (spec.hierarchical && p.parent_col.is_some()).then(Default::default),
        }),
    });
    if let Some(c) = call_ctx(ex) {
        if c.selections.len() >= 64 {
            c.selections.retain(|w| w.strong_count() > 0);
        }
        c.selections.push(Rc::downgrade(&sel));
    }
    Ok(Value::Object(sel))
}

pub(crate) fn find_one(
    ex: &mut Execution,
    app: &Application,
    obj: ObjId,
    mut spec: SelectSpec,
) -> CallResult<Value> {
    let security = InterimSelectionSecurity::for_call(ex)?;
    spec.top = Some(1);
    let p = prepare(app, obj, &spec)?;
    let found = if p.empty {
        None
    } else {
        let lowered = bind(app, &p.ctx, &p.stmt, &security)?;
        let txn = app.store().read().map_err(|e| store_error(&e))?;
        let cancel = AtomicBool::new(false);
        let env = ExecEnv {
            collation: p.ctx.collation.as_ref(),
            cancel: &cancel,
            stop: None,
            mem_limit: app.options().query_mem_limit,
            max_number_limbs: MAX_LIMBS,
            memory: None,
            lease: None,
            evictor: None,
        };
        let res = execute_store(&lowered, &*txn, &p.ctx.map, &env).map_err(|e| exec_error(&e))?;
        drop(txn);
        res.rows.into_iter().next()
    };
    let id = found
        .as_ref()
        .and_then(|row| row.get(p.ref_col?))
        .and_then(|q| match q {
            QValue::Ref { id, .. } => Some(RefId(*id)),
            _ => None,
        });
    match id {
        Some(id) => Ok(crate::values::refs::ref_value(app, RefValue { obj, id })?),
        None => Ok(empty_ref(app, obj)?),
    }
}

pub(crate) fn serves(name: &str) -> bool {
    matches!(
        name,
        "Выбрать"
            | "ВыбратьИерархически"
            | "НайтиПоКоду"
            | "НайтиПоНаименованию"
            | "НайтиПоРеквизиту"
            | "НайтиПоНомеру"
    )
}

fn parse_order(
    app: &Application,
    obj: ObjId,
    text: &str,
) -> Result<Vec<(String, bool)>, HostError> {
    let def = app.model().obj(obj);
    let mut out = Vec::new();
    for part in text.split(',') {
        let mut words = part.split_whitespace();
        let Some(name) = words.next() else {
            if part.trim().is_empty() && text.trim().is_empty() {
                break;
            }
            return Err(cant_order());
        };
        let desc = match words.next().map(bsl_prim::fold_name).as_deref() {
            None | Some("возр" | "asc") => false,
            Some("убыв" | "desc") => true,
            Some(_) => return Err(cant_order()),
        };
        if words.next().is_some() {
            return Err(cant_order());
        }
        let ok = field_index(def, name)
            .and_then(|i| def.fields.get(i))
            .is_some_and(|fd| fd.std.is_some() || fd.indexed);
        if !ok {
            return Err(cant_order());
        }
        out.push((name.to_string(), desc));
    }
    Ok(out)
}

fn parse_filter(
    app: &Application,
    obj: ObjId,
    v: &Value,
    number: usize,
) -> Result<Option<(String, Value)>, HostError> {
    let def = app.model().obj(obj);
    let fields = bsl_stdlib::structure_fields(v).ok_or_else(|| type_mismatch(number))?;
    let mut it = fields.into_iter();
    let (Some((key, value)), None) = (it.next(), it.next()) else {
        return if bsl_stdlib::structure_fields(v).is_some_and(|f| f.is_empty()) {
            Ok(None)
        } else {
            Err(value_mismatch(number))
        };
    };
    let ok = field_index(def, &key)
        .and_then(|i| def.fields.get(i))
        .is_some_and(|fd| fd.std.is_some() || fd.indexed);
    if !ok {
        return Err(value_mismatch(number));
    }
    Ok(Some((key, value)))
}

fn rounded_number(v: &Value) -> Option<bsl_prim::Number> {
    let n = match v {
        Value::Number(n) => n.clone(),
        Value::String(s) => bsl_prim::Number::parse(s.to_string_lossy().trim())?,
        _ => return None,
    };
    let r = n.round(0, bsl_prim::RoundMode::HalfAwayFromZero);
    (!r.is_negative()).then_some(r)
}

fn trimmed_sample(s: &bsl_prim::Str) -> Value {
    Value::from(s.to_string_lossy().trim_end_matches(' '))
}

fn digits_of(n: &bsl_prim::Number) -> Option<Value> {
    n.is_integer()
        .then(|| Value::from(n.to_xml_string().as_str()))
}

fn code_value(def: &crate::model::ObjDef, v: &Value) -> Option<Value> {
    match (def.behavior.code_type, v) {
        (CodeType::String, Value::String(s)) => Some(trimmed_sample(s)),
        (CodeType::String, Value::Number(n)) => digits_of(n),
        (CodeType::Number, v) => rounded_number(v).map(Value::Number),
        _ => None,
    }
}

fn number_period(d: BslDate, p: NumberPeriodicity) -> Option<(BslDate, BslDate)> {
    let q = d.parts();
    let (y, m) = (q.year, q.month);
    let start = |y, m, d| BslDate::from_parts(y, m, d, 0, 0, 0);
    let (lo, next) = match p {
        NumberPeriodicity::Nonperiodical => return None,
        NumberPeriodicity::Year => (start(y, 1, 1)?, start(y.checked_add(1)?, 1, 1)),
        NumberPeriodicity::Quarter => {
            let m0 = (m - 1) / 3 * 3 + 1;
            let next = if m0 + 3 > 12 {
                start(y.checked_add(1)?, 1, 1)
            } else {
                start(y, m0 + 3, 1)
            };
            (start(y, m0, 1)?, next)
        }
        NumberPeriodicity::Month => (
            start(y, m, 1)?,
            if m == 12 {
                start(y.checked_add(1)?, 1, 1)
            } else {
                start(y, m + 1, 1)
            },
        ),
        NumberPeriodicity::Day => (
            start(y, m, q.day)?,
            BslDate::from_ticks(start(y, m, q.day)?.ticks() + 86_400 * BslDate::TICKS_PER_SECOND),
        ),
    };
    let hi = match next {
        Some(n) => BslDate::from_ticks(n.ticks() - 1)?,
        None => BslDate::MAX,
    };
    Some((lo, hi))
}

pub(crate) fn manager_call(
    ex: &mut Execution,
    app: &Application,
    obj: ObjId,
    name: &str,
    p: &Params,
) -> CallResult<Value> {
    let def = app.model().obj(obj);
    let mut spec = SelectSpec {
        parent: p.opt("Родитель").cloned(),
        owner: p.opt("Владелец").cloned(),
        parent_no: p.number("Родитель"),
        owner_no: p.number("Владелец"),
        ..SelectSpec::default()
    };
    match name {
        "Выбрать" | "ВыбратьИерархически" => {
            spec.hierarchical = name == "ВыбратьИерархически";
            if p.raw("ДатаНачала").is_some() || p.raw("ДатаОкончания").is_some()
            {
                spec.period = Some((
                    p.raw("ДатаНачала").cloned().unwrap_or(Value::Undefined),
                    p.raw("ДатаОкончания").cloned().unwrap_or(Value::Undefined),
                ));
            }
            if let Some(f) = p.opt("Отбор") {
                spec.filter = parse_filter(app, obj, f, p.number("Отбор"))?;
            }
            if let Some(o) = p.opt("Порядок") {
                let Value::String(s) = o else {
                    return Err(wrong_attribute().into());
                };
                spec.order = parse_order(app, obj, &s.to_string_lossy())?;
                if spec.hierarchical && spec.order.len() > 1 {
                    return Err(wrong_attribute().into());
                }
            }
            select(ex, app, obj, spec)
        }
        "НайтиПоКоду" => {
            if field_index(def, "Код").is_none() {
                return Ok(Value::Undefined);
            }
            let code = p.raw("Код").cloned().unwrap_or(Value::Undefined);
            let by_full = matches!(p.opt("ПоискПоПолномуКоду"), Some(Value::Bool(true)))
                && def.behavior.hierarchical;
            let Some(code) = code_value(def, &code) else {
                return Ok(empty_ref(app, obj)?);
            };
            spec.ref_order = true;
            let (Value::String(text), true) = (&code, by_full) else {
                let code = match (&code, def.behavior.code_length) {
                    (Value::String(t), n) if n > 0 => {
                        let cut: String = t.to_string_lossy().chars().take(n as usize).collect();
                        Value::from(cut.as_str())
                    }
                    _ => code,
                };
                spec.exact = Some(("Код".to_string(), code));
                return find_one(ex, app, obj, spec);
            };
            let text = text.to_string_lossy();
            let text = text.trim_matches('/');
            let mut parent = spec.parent.clone().unwrap_or(empty_ref(app, obj)?);
            let mut last = empty_ref(app, obj)?;
            if text.is_empty() {
                return Ok(last);
            }
            for seg in text.split('/') {
                let step = SelectSpec {
                    parent: Some(parent.clone()),
                    owner: spec.owner.clone(),
                    parent_no: spec.parent_no,
                    owner_no: spec.owner_no,
                    exact: Some(("Код".to_string(), Value::from(seg))),
                    ref_order: true,
                    ..SelectSpec::default()
                };
                last = find_one(ex, app, obj, step)?;
                if as_ref(&last).is_none_or(|r| r.is_empty()) {
                    return Ok(last);
                }
                parent = last.clone();
            }
            Ok(last)
        }
        "НайтиПоНаименованию" => {
            let Some(Value::String(s)) = p.opt("Наименование") else {
                return Ok(empty_ref(app, obj)?);
            };
            let exact = matches!(p.opt("ТочноеСоответствие"), Some(Value::Bool(true)));
            if exact {
                if s.to_string_lossy().is_empty() {
                    return Ok(empty_ref(app, obj)?);
                }
                spec.exact = Some(("Наименование".to_string(), Value::String(s.clone())));
                spec.ref_order = true;
            } else {
                spec.starts_with = Some(("Наименование".to_string(), s.to_string_lossy()));
            }
            find_one(ex, app, obj, spec)
        }
        "НайтиПоРеквизиту" => {
            let Some(Value::String(n)) = p.opt("ИмяРеквизита") else {
                return Err(wrong_attribute().into());
            };
            let n = n.to_string_lossy();
            let fd = field_index(def, &n).and_then(|i| def.fields.get(i));
            let ok = fd.is_some_and(|fd| {
                fd.std.is_none()
                    && !(fd.types.atoms.contains(&TypeAtom::String)
                        && fd.types.string.is_none_or(|s| s.length == 0))
            });
            if !ok {
                return Err(wrong_attribute().into());
            }
            spec.exact = Some((
                n,
                p.raw("ЗначениеРеквизита")
                    .cloned()
                    .unwrap_or(Value::Undefined),
            ));
            spec.ref_order = true;
            find_one(ex, app, obj, spec)
        }
        "НайтиПоНомеру" => {
            let number = number_value(def, p.raw("НомерДокумента"));
            let Some(number) = number else {
                return Ok(empty_ref(app, obj)?);
            };
            if p.opt("ДатаИнтервала")
                .is_some_and(|d| !matches!(d, Value::Date(_)))
            {
                return Ok(empty_ref(app, obj)?);
            }
            spec.exact = Some(("Номер".to_string(), number));
            spec.ref_order = true;
            if let Some(Value::Date(d)) = p.opt("ДатаИнтервала")
                && let Some((lo, hi)) = number_period(*d, def.behavior.number_periodicity)
            {
                spec.period = Some((Value::Date(lo), Value::Date(hi)));
            }
            find_one(ex, app, obj, spec)
        }
        _ => Err(HostError::not_implemented("Менеджер", name).into()),
    }
}

fn number_value(def: &crate::model::ObjDef, v: Option<&Value>) -> Option<Value> {
    let v = v?;
    match (def.behavior.number_type, v) {
        (CodeType::String, Value::String(s)) => Some(trimmed_sample(s)),
        (CodeType::String, Value::Number(n)) => digits_of(n),
        (CodeType::Number, v) => rounded_number(v).map(Value::Number),
        _ => None,
    }
}

pub(crate) fn selectable(kind: ObjKind) -> bool {
    kind.is_reference() && kind != ObjKind::Enum
}
