use crate::Application;
use crate::errors::{self, keys};
use crate::exchange_params::ExchangeParams;
use crate::meta::{arg_value, member_name, type_mismatch};
use crate::model::{ObjId, ObjKind, StdAttr, TypeAtom, Usage};
use crate::objtab::{ObjTab, TabState, not_writable};
use crate::read::{ReadRow, field_index, is_std_name, read_fresh, read_tabular, tabular_index};
use crate::txn::CallTxn;
use crate::types::Category;
use crate::values::refs::{RefValue, as_ref, empty_ref, ref_value};
use bsl_prim::fold_name;
use bsl_syntax::ast::MethodKind;
use bsl_vm::{
    Arg, CallError, CallResult, Category as ErrCat, CategorySet, Exception, Execution, HostError,
    HostObject, MemberId, MemberKind, ModuleContext, ModuleInstanceRef, ObjRef, RetainedMemory,
    TypeId, Value,
};
use ibschema::values::RefId;
use platform_catalog::messages::Lang;
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;

const ATTR_BASE: u32 = 1 << 20;
const TAB_BASE: u32 = 1 << 21;
const VAR_BASE: u32 = 1 << 22;
const METHOD_BASE: u32 = 1 << 23;

const EXPORT_CALL_WEIGHT: u32 = 120;

const BODY_WEIGHT: u32 = 516;

const NEW_VERSION: &str = "            ";

#[derive(Clone)]
pub(crate) struct WriteStamp {
    pub(crate) txn: Weak<CallTxn>,
    pub(crate) id: u64,
    pub(crate) was_new: bool,
}

pub(crate) struct State {
    pub(crate) r: Option<RefValue>,
    pub(crate) row: Rc<ReadRow>,
    pub(crate) tabular: Vec<Rc<ObjTab>>,
    pub(crate) is_new: bool,
    pub(crate) modified: Rc<Cell<bool>>,
    pub(crate) data_version: String,
    pub(crate) extra: Option<Value>,
    pub(crate) exchange: Option<Rc<ExchangeParams>>,
    pub(crate) module: Option<ModuleInstanceRef>,
    pub(crate) attributes: Vec<Option<Value>>,
    pub(crate) new_ref: Option<RefValue>,
    pub(crate) dead: bool,
    pub(crate) written: Option<WriteStamp>,
}

impl State {
    fn new_write_rolled_back(&self) -> bool {
        self.written.as_ref().is_some_and(|w| {
            w.was_new && w.txn.upgrade().is_some_and(|txn| txn.was_rolled_back(w.id))
        })
    }
}

pub(crate) struct Snapshot {
    r: Option<RefValue>,
    row: Rc<ReadRow>,
    is_new: bool,
    modified: bool,
    data_version: String,
    attributes: Vec<Option<Value>>,
    new_ref: Option<RefValue>,
    written: Option<WriteStamp>,
    tabs: Vec<TabState>,
}

pub struct ObjectInstance {
    pub(crate) app: Arc<Application>,
    pub(crate) obj: ObjId,
    ty: TypeId,
    me: Weak<ObjectInstance>,
    pub(crate) state: RefCell<State>,
}

pub(crate) fn not_selected() -> HostError {
    let mut e = match errors::msg(keys::ELEMENT_NOT_SELECTED) {
        Some(m) => HostError::text(m.ru, m.en),
        None => internal(keys::ELEMENT_NOT_SELECTED),
    };
    e.categories = Some(CategorySet::of(&[
        ErrCat::AllErrors,
        ErrCat::ScriptRuntimeError,
        ErrCat::ScriptUseError,
    ]));
    e
}

fn internal(text: &str) -> HostError {
    HostError::text(&format!("internal: {text}"), &format!("internal: {text}"))
}

pub fn get_object(ex: &mut Execution, app: &Application, r: RefValue) -> CallResult<Value> {
    if r.is_empty() {
        return Err(not_selected().into());
    }
    let row = read_fresh(ex, app, r)?;
    if !row.found {
        return Ok(Value::Undefined);
    }
    Ok(Value::Object(instance(ex, app, r, row, false, false)?))
}

pub(crate) fn create_item(ex: &mut Execution, app: &Application, obj: ObjId) -> CallResult<Value> {
    Ok(Value::Object(new_instance(ex, app, obj, false)?))
}

pub(crate) fn create_group(ex: &mut Execution, app: &Application, obj: ObjId) -> CallResult<Value> {
    let def = app.model().obj(obj);
    let folders = def.behavior.hierarchical
        && def.behavior.hierarchy_folders
        && def.fields.iter().any(|f| f.std == Some(StdAttr::IsFolder));
    if !folders {
        return Err(crate::write::use_error(keys::CATALOG_HAS_NO_FOLDERS).into());
    }
    Ok(Value::Object(new_instance(ex, app, obj, true)?))
}

fn new_instance(
    ex: &mut Execution,
    app: &Application,
    obj: ObjId,
    folder: bool,
) -> CallResult<Rc<ObjectInstance>> {
    let r = RefValue {
        obj,
        id: RefId::NIL,
    };
    let row = read_fresh(ex, app, r)?;
    instance(ex, app, r, row, true, folder)
}

fn instance(
    ex: &mut Execution,
    app: &Application,
    r: RefValue,
    row: ReadRow,
    is_new: bool,
    folder: bool,
) -> CallResult<Rc<ObjectInstance>> {
    let arc = app
        .arc()
        .ok_or_else(|| internal("the application is gone"))?;
    let def = app.model().obj(r.obj);
    let mut live = row
        .bytes()
        .saturating_add(crate::read_budget::rc_size::<ObjectInstance>())
        .saturating_add(crate::read_budget::slots::<Option<Value>>(
            def.tabular
                .len()
                .saturating_add(if is_new { def.fields.len() } else { 0 }),
        ))
        .saturating_add(row.version.len() as u64);
    crate::read_budget::reserve(ex, live)?;
    let modified = Rc::new(Cell::new(false));
    let mut tabular = Vec::with_capacity(def.tabular.len());
    for i in 0..def.tabular.len() {
        let rows = if is_new {
            Vec::new()
        } else {
            read_tabular(ex, &arc, r, i, live)?
        };
        let tab = ObjTab::new(&arc, r.obj, i, rows, modified.clone())?;
        live = live.saturating_add(tab.bytes());
        crate::read_budget::reserve(ex, live)?;
        tabular.push(tab);
    }
    let ty = app
        .types()
        .and_then(|t| t.type_of(r.obj, Category::Object))
        .ok_or_else(|| internal("no object type"))?;
    let data_version = if is_new {
        NEW_VERSION.to_string()
    } else {
        row.version.clone()
    };
    let mut attributes = if is_new {
        vec![None; def.fields.len()]
    } else {
        Vec::new()
    };
    if folder
        && let Some(slot) = def
            .fields
            .iter()
            .position(|f| f.std == Some(StdAttr::IsFolder))
            .and_then(|i| attributes.get_mut(i))
    {
        *slot = Some(Value::Bool(true));
    }
    let inst = Rc::new_cyclic(|me| ObjectInstance {
        app: arc,
        obj: r.obj,
        ty,
        me: me.clone(),
        state: RefCell::new(State {
            r: (!is_new).then_some(r),
            row: Rc::new(row),
            tabular,
            is_new,
            modified,
            data_version,
            extra: None,
            exchange: None,
            module: None,
            attributes,
            new_ref: None,
            dead: false,
            written: None,
        }),
    });
    if let Some(m) = app
        .modules()
        .of_object(r.obj, md_model::ModuleKind::ObjectModule)
    {
        let compiled = app
            .modules()
            .compiled_or_fatal(m, ex.lang())
            .map_err(CallError::Exc)?;
        let ctx: ObjRef = Rc::new(ObjectCtx {
            inst: Rc::downgrade(&inst),
            ty,
        });
        let mi = ex
            .instantiate_weighted(&compiled, ModuleContext::Object(ctx), BODY_WEIGHT)
            .map_err(CallError::Exc)?;
        inst.state.borrow_mut().module = Some(mi);
    }
    Ok(inst)
}

impl ObjectInstance {
    pub(crate) fn presentation_parts(&self) -> (ObjId, Option<RefValue>, Rc<ReadRow>) {
        let st = self.state.borrow();
        (self.obj, st.r, st.row.clone())
    }

    pub(crate) fn module(&self) -> Option<ModuleInstanceRef> {
        self.state.borrow().module.clone()
    }

    pub(crate) fn rc(&self) -> Result<Rc<ObjectInstance>, HostError> {
        self.me
            .upgrade()
            .ok_or_else(|| internal("the object instance is gone"))
    }

    pub(crate) fn type_name(&self) -> String {
        format!(
            "{}Объект",
            self.app.model().obj(self.obj).kind.name(Lang::Ru)
        )
    }

    fn not_implemented(&self, member: &str) -> HostError {
        HostError::not_implemented(&self.type_name(), member)
    }

    pub(crate) fn live(&self) -> Result<(), HostError> {
        let mut st = self.state.borrow_mut();
        if st.dead {
            return Err(not_selected());
        }
        if st.new_write_rolled_back() {
            st.dead = true;
            return Err(not_selected());
        }
        Ok(())
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
        let st = self.state.borrow();
        Snapshot {
            r: st.r,
            row: st.row.clone(),
            is_new: st.is_new,
            modified: st.modified.get(),
            data_version: st.data_version.clone(),
            attributes: st.attributes.clone(),
            new_ref: st.new_ref,
            written: st.written.clone(),
            tabs: st.tabular.iter().map(|t| t.state()).collect(),
        }
    }

    pub(crate) fn restore(&self, s: Snapshot) {
        let mut st = self.state.borrow_mut();
        st.r = s.r;
        st.row = s.row;
        st.is_new = s.is_new;
        st.modified.set(s.modified);
        st.data_version = s.data_version;
        st.attributes = s.attributes;
        st.new_ref = s.new_ref;
        st.written = s.written;
        for (tab, state) in st.tabular.iter().zip(s.tabs) {
            tab.set_state(state);
        }
    }

    fn member(&self, ex: &Execution, m: MemberId) -> Option<(bool, &'static str)> {
        let t = ex.runtime().types().app_type(self.ty)?.template?;
        member_name(platform_catalog::catalog(), t, m).map(|(method, ru, _)| (method, ru))
    }

    pub(crate) fn folder_flag(&self) -> Option<bool> {
        let def = self.app.model().obj(self.obj);
        let i = def
            .fields
            .iter()
            .position(|f| f.std == Some(StdAttr::IsFolder))?;
        let st = self.state.borrow();
        Some(match st.attributes.get(i) {
            Some(Some(Value::Bool(b))) => *b,
            _ => matches!(st.row.fields.get(i), Some(Value::Bool(true))),
        })
    }

    pub(crate) fn hidden(&self, i: usize) -> bool {
        let def = self.app.model().obj(self.obj);
        if !def.behavior.hierarchy_folders {
            return false;
        }
        let Some(f) = def.fields.get(i).filter(|f| f.std.is_none()) else {
            return false;
        };
        let Some(folder) = self.folder_flag() else {
            return false;
        };
        match f.usage {
            Usage::ForItem => folder,
            Usage::ForFolder => !folder,
            Usage::ForFolderAndItem => false,
        }
    }

    pub(crate) fn attr(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        if self.hidden(i) {
            return Ok(Value::Null);
        }
        {
            let state = self.state.borrow();
            if let Some(Some(value)) = state.attributes.get(i) {
                crate::read_budget::reserve(
                    ex,
                    state
                        .row
                        .bytes()
                        .saturating_add(crate::read_budget::slots::<Option<Value>>(
                            state.attributes.capacity(),
                        ))
                        .saturating_add(crate::read_budget::value_heap(value)),
                )?;
                return Ok(value.clone());
            }
        }
        let row = self.state.borrow().row.clone();
        row.value(ex, &self.app, self.obj, i)
    }

    fn section(&self, i: usize) -> CallResult<Value> {
        match self.state.borrow().tabular.get(i) {
            Some(tab) => {
                let tab: ObjRef = tab.clone();
                Ok(Value::Object(tab))
            }
            None => Err(internal("no such tabular section").into()),
        }
    }

    fn this(&self) -> CallResult<Value> {
        match self.me.upgrade() {
            Some(o) => Ok(Value::Object(o)),
            None => Err(internal("the object instance is gone").into()),
        }
    }

    fn no_field(name: &str) -> HostError {
        HostError::key("bsl:IDS_RTE_VALPROP_NOTFOUND")
            .with_arg(name)
            .raw()
    }

    fn var_place(&self, i: u32) -> Result<bsl_vm::Place, HostError> {
        let module = self
            .module()
            .ok_or_else(|| internal("the object has no module"))?;
        let name = module
            .module()
            .vars
            .get(i.saturating_sub(VAR_BASE) as usize)
            .map(|v| v.name.name.to_string())
            .ok_or_else(|| internal("no such module variable"))?;
        module
            .export_var(&name)
            .ok_or_else(|| internal("the variable is not exported"))
    }

    fn exported_var(module: &ModuleInstanceRef, name: &str) -> Option<usize> {
        let n = fold_name(name);
        module
            .module()
            .vars
            .iter()
            .position(|v| v.export && *v.name.folded == *n)
    }

    fn extra(&self, ex: &mut Execution) -> CallResult<Value> {
        if let Some(v) = self.state.borrow().extra.clone() {
            return Ok(v);
        }
        crate::read_budget::reserve(ex, 512)?;
        let v = bsl_stdlib::new_structure(Vec::new());
        self.state.borrow_mut().extra = Some(v.clone());
        Ok(v)
    }

    fn exchange(&self) -> Result<Rc<ExchangeParams>, HostError> {
        if let Some(e) = self.state.borrow().exchange.clone() {
            return Ok(e);
        }
        let e = ExchangeParams::new()?;
        self.state.borrow_mut().exchange = Some(e.clone());
        Ok(e)
    }

    pub(crate) fn loading(&self) -> bool {
        self.state
            .borrow()
            .exchange
            .as_ref()
            .is_some_and(|e| e.loading())
    }

    pub(crate) fn assign(&self, ex: &mut Execution, index: usize, v: Value) -> CallResult<()> {
        let def = self.app.model().obj(self.obj);
        let field = def
            .fields
            .get(index)
            .ok_or_else(|| internal("no such object field"))?;
        if matches!(
            field.std,
            Some(StdAttr::Ref | StdAttr::Predefined | StdAttr::IsFolder)
        ) {
            return Err(not_writable(&field.name).into());
        }
        if self.hidden(index) {
            return Err(if self.folder_flag() == Some(true) {
                crate::write::use_error(keys::ATTRIBUTE_NOT_FOR_FOLDER)
            } else {
                self.not_implemented(&format!("{}: реквизит группы у элемента", field.name))
            }
            .into());
        }
        let cast = crate::values::typedesc::cast_to_field(&self.app, ex, field, v)?;
        let bytes = {
            let state = self.state.borrow();
            state
                .attributes
                .iter()
                .flatten()
                .fold(state.row.bytes(), |sum, value| {
                    sum.saturating_add(crate::read_budget::value_heap(value))
                })
                .saturating_add(crate::read_budget::value_heap(&cast))
                .saturating_add(crate::read_budget::slots::<Option<Value>>(
                    def.fields.len().max(state.attributes.capacity()),
                ))
        };
        crate::read_budget::reserve(ex, bytes)?;
        let mut state = self.state.borrow_mut();
        if state.attributes.len() < def.fields.len() {
            state.attributes.resize(def.fields.len(), None);
        }
        let slot = state
            .attributes
            .get_mut(index)
            .ok_or_else(|| internal("no such object attribute"))?;
        *slot = Some(cast);
        state.modified.set(true);
        Ok(())
    }

    fn set_new_ref(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        if !self.state.borrow().is_new {
            return Err(crate::write::use_error(keys::CANT_SET_NEW_REF).into());
        }
        let v = arg_value(ex, args, 0)?;
        let r = as_ref(&v)
            .filter(|r| r.obj == self.obj)
            .ok_or_else(|| type_mismatch(1))?;
        if !r.is_empty() {
            self.state.borrow_mut().new_ref = Some(r);
        }
        Ok(Value::Undefined)
    }

    fn get_new_ref(&self) -> CallResult<Value> {
        let (is_new, set) = {
            let st = self.state.borrow();
            (st.is_new, st.new_ref)
        };
        if !is_new {
            return Err(crate::write::use_error(keys::CANT_GET_NEW_REF).into());
        }
        match set {
            Some(r) => Ok(ref_value(&self.app, r)?),
            None => Ok(empty_ref(&self.app, self.obj)?),
        }
    }

    fn read_again(&self, ex: &mut Execution) -> CallResult<Value> {
        let (r, is_new) = {
            let st = self.state.borrow();
            (st.r, st.is_new)
        };
        let Some(r) = r.filter(|_| !is_new) else {
            return Ok(Value::Undefined);
        };
        let row = read_fresh(ex, &self.app, r)?;
        if !row.found {
            return Err(not_selected().into());
        }
        let tabs: Vec<Rc<ObjTab>> = self.state.borrow().tabular.clone();
        let mut live = row.bytes();
        let mut fresh = Vec::with_capacity(tabs.len());
        for i in 0..tabs.len() {
            let rows = read_tabular(ex, &self.app, r, i, live)?;
            live = live.saturating_add(crate::read_budget::rows(&rows, rows.capacity()));
            crate::read_budget::reserve(ex, live)?;
            fresh.push(rows);
        }
        let version = row.version.clone();
        let mut st = self.state.borrow_mut();
        st.row = Rc::new(row);
        for slot in st.attributes.iter_mut() {
            *slot = None;
        }
        st.data_version = version;
        st.modified.set(false);
        for (tab, rows) in tabs.iter().zip(fresh) {
            tab.replace(rows);
        }
        Ok(Value::Undefined)
    }

    fn copy(&self, ex: &mut Execution) -> CallResult<Value> {
        let new = new_instance(ex, &self.app, self.obj, self.folder_flag().unwrap_or(false))?;
        let def = self.app.model().obj(self.obj);
        let mut values: Vec<(usize, Value)> = Vec::with_capacity(def.fields.len());
        for (i, f) in def.fields.iter().enumerate() {
            if matches!(
                f.std,
                Some(
                    StdAttr::Ref
                        | StdAttr::Predefined
                        | StdAttr::PredefinedDataName
                        | StdAttr::DeletionMark
                        | StdAttr::Code
                        | StdAttr::IsFolder
                )
            ) || self.hidden(i)
            {
                continue;
            }
            let v = self.attr(ex, i)?;
            if matches!(v, Value::Undefined)
                && f.types
                    .atoms
                    .iter()
                    .any(|a| matches!(a, TypeAtom::ValueStorage | TypeAtom::Binary))
            {
                continue;
            }
            values.push((i, v));
        }
        let heap = values.iter().fold(0u64, |n, (_, v)| {
            n.saturating_add(crate::read_budget::value_heap(v))
        });
        crate::read_budget::reserve(ex, heap.saturating_add(new.state.borrow().row.bytes()))?;
        {
            let mut st = new.state.borrow_mut();
            for (i, v) in values {
                if let Some(slot) = st.attributes.get_mut(i) {
                    *slot = Some(v);
                }
            }
            let sources: Vec<Vec<Vec<Value>>> = self
                .state
                .borrow()
                .tabular
                .iter()
                .map(|t| t.lines())
                .collect();
            for (tab, lines) in st.tabular.iter().zip(sources) {
                tab.fill_from(lines);
            }
            st.modified.set(false);
        }
        crate::write_hooks::after_copy(ex, &new, self)?;
        Ok(Value::Object(new))
    }

    fn level(&self, ex: &mut Execution) -> CallResult<Value> {
        let stored = {
            let st = self.state.borrow();
            if st.is_new { None } else { st.r }
        };
        match stored {
            Some(r) => crate::hierarchy::call(ex, &self.app, r, "Уровень", &[]),
            None => Err(self.not_implemented("Уровень: новый объект").into()),
        }
    }

    fn fill(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let data = arg_value(ex, args, 0)?;
        let def = self.app.model().obj(self.obj);
        if def.kind != ObjKind::Catalog {
            if !self.state.borrow().is_new {
                return Err(self.not_implemented("Заполнить").into());
            }
            if !matches!(data, Value::Undefined) {
                return Err(self.not_implemented("Заполнить: данные заполнения").into());
            }
        }
        let text = bsl_vm::Place::temp(Value::from(""));
        let standard = bsl_vm::Place::temp(Value::Bool(true));
        crate::events::filling(ex, &self.rc()?, data.clone(), &text, &standard)?;
        if def.kind == ObjKind::Catalog {
            let on = !matches!(standard.temp_value(), Some(Value::Bool(false)));
            if on {
                if as_ref(&data).is_some() {
                    return Err(self
                        .not_implemented("Заполнить: ссылка как данные заполнения")
                        .into());
                }
                self.apply_fill_values(ex)?;
            }
            self.state.borrow().modified.set(true);
        }
        Ok(Value::Undefined)
    }

    fn apply_fill_values(&self, ex: &mut Execution) -> CallResult<()> {
        let def = self.app.model().obj(self.obj);
        let Some(md) = self.app.model().config.object(def.md) else {
            return Ok(());
        };
        for (i, f) in def.fields.iter().enumerate() {
            let Some(id) = f.md else {
                continue;
            };
            let Some(child) = md.children.iter().find(|c| c.id == id) else {
                continue;
            };
            let prop = |name: &str| {
                child
                    .props
                    .iter()
                    .find(|p| p.name == name)
                    .map(|p| &p.value)
            };
            let on = match prop("FillFromFillingValue") {
                Some(md_model::Value::Bool(b)) => *b,
                Some(md_model::Value::Text(t)) => t == "true",
                _ => false,
            };
            if !on || self.hidden(i) {
                continue;
            }
            let fill = match prop("FillValue") {
                Some(md_model::Value::Typed(md_model::TypedValue::Bool(b))) => Value::Bool(*b),
                Some(md_model::Value::Typed(md_model::TypedValue::Str(s))) => {
                    Value::from(s.as_str())
                }
                Some(md_model::Value::Typed(md_model::TypedValue::Number(t))) => {
                    match bsl_prim::Number::parse(t.trim()) {
                        Some(n) => Value::Number(n),
                        None => {
                            return Err(self
                                .not_implemented(&format!(
                                    "Заполнить: значение заполнения {}",
                                    f.name
                                ))
                                .into());
                        }
                    }
                }
                Some(md_model::Value::Typed(md_model::TypedValue::Nil)) | None => continue,
                Some(_) => {
                    return Err(self
                        .not_implemented(&format!("Заполнить: значение заполнения {}", f.name))
                        .into());
                }
            };
            let current = self.attr(ex, i)?;
            if is_empty_value(&current) {
                self.assign(ex, i, fill)?;
            }
        }
        Ok(())
    }
}

fn is_empty_value(v: &Value) -> bool {
    match v {
        Value::Undefined | Value::Null => true,
        Value::Bool(b) => !*b,
        Value::Number(n) => n.is_zero(),
        Value::String(s) => s.units().is_empty(),
        Value::Date(d) => *d == bsl_prim::BslDate::EMPTY,
        other => as_ref(other).is_some_and(|r| r.is_empty()),
    }
}

impl HostObject for ObjectInstance {
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        visit.work(1)?;
        visit.mutable();
        let st = self.state.borrow();
        visit.allocation(Rc::as_ptr(&st.row) as usize, st.row.bytes())?;
        if st.attributes.capacity() != 0
            && visit.allocation(
                st.attributes.as_ptr() as usize,
                crate::read_budget::slots::<Option<Value>>(st.attributes.capacity()),
            )?
        {
            for value in st.attributes.iter().flatten() {
                visit.value(value)?;
            }
        }
        for tab in &st.tabular {
            let tab: ObjRef = tab.clone();
            visit.value(&Value::Object(tab))?;
        }
        if let Some(extra) = &st.extra {
            visit.value(extra)?;
        }
        if let Some(exchange) = &st.exchange {
            let exchange: ObjRef = exchange.clone();
            visit.value(&Value::Object(exchange))?;
        }
        Ok(true)
    }

    fn type_id(&self) -> TypeId {
        self.ty
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        if (ATTR_BASE..TAB_BASE).contains(&m.0) {
            return self.attr(ex, (m.0 - ATTR_BASE) as usize);
        }
        if (TAB_BASE..VAR_BASE).contains(&m.0) {
            return self.section((m.0 - TAB_BASE) as usize);
        }
        if (VAR_BASE..METHOD_BASE).contains(&m.0) {
            let place = self.var_place(m.0)?;
            return place.get(ex).map_err(CallError::Exc);
        }
        match self.member(ex, m) {
            Some((false, "Ссылка")) => {
                let r = {
                    let st = self.state.borrow();
                    st.r.filter(|_| !st.new_write_rolled_back())
                };
                match r {
                    Some(r) => Ok(ref_value(&self.app, r)?),
                    None => Ok(empty_ref(&self.app, self.obj)?),
                }
            }
            Some((false, "ВерсияДанных")) => {
                Ok(Value::from(self.state.borrow().data_version.as_str()))
            }
            Some((false, "ЭтотОбъект")) => self.this(),
            Some((false, "ДополнительныеСвойства")) => self.extra(ex),
            Some((false, "ОбменДанными")) => {
                let e: ObjRef = self.exchange()?;
                Ok(Value::Object(e))
            }
            Some((false, ru)) => {
                let def = self.app.model().obj(self.obj);
                if let Some(i) = tabular_index(def, ru) {
                    return self.section(i);
                }
                match field_index(def, ru) {
                    Some(i) => self.attr(ex, i),
                    None if bsl_prim::fold_name(ru) == "этогруппа" && def.behavior.hierarchical => {
                        Ok(Value::Bool(false))
                    }
                    None if is_std_name(ru) => Err(Self::no_field(ru).into()),
                    None => Err(self.not_implemented(ru).into()),
                }
            }
            Some((true, ru)) => Err(self.not_implemented(ru).into()),
            None => Err(self.not_implemented(&m.0.to_string()).into()),
        }
    }

    fn set(&self, ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        if (VAR_BASE..METHOD_BASE).contains(&m.0) {
            let place = self.var_place(m.0)?;
            return place.set(ex, v).map_err(CallError::Exc);
        }
        let def = self.app.model().obj(self.obj);
        if (ATTR_BASE..TAB_BASE).contains(&m.0) {
            let index = (m.0 - ATTR_BASE) as usize;
            let field = def
                .fields
                .get(index)
                .ok_or_else(|| internal("no such object field"))?;
            if def.kind != ObjKind::Catalog {
                return Err(self.not_implemented(&field.name).into());
            }
            return self.assign(ex, index, v);
        }
        if (TAB_BASE..VAR_BASE).contains(&m.0) {
            let name = def
                .tabular
                .get((m.0 - TAB_BASE) as usize)
                .map(|t| t.name.clone())
                .unwrap_or_default();
            return Err(not_writable(&name).into());
        }
        match self.member(ex, m) {
            Some((false, ru)) if def.kind == ObjKind::Catalog => match field_index(def, ru) {
                Some(i) => self.assign(ex, i, v),
                None => match crate::read::std_attr_named(ru) {
                    Some(attr)
                        if !matches!(
                            attr,
                            StdAttr::Ref | StdAttr::Predefined | StdAttr::IsFolder
                        ) =>
                    {
                        Err(self
                            .not_implemented(&format!("{ru}: у справочника нет такого реквизита"))
                            .into())
                    }
                    _ => Err(not_writable(ru).into()),
                },
            },
            Some((_, ru)) => Err(self.not_implemented(ru).into()),
            None => Err(self.not_implemented(&m.0.to_string()).into()),
        }
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        if m.0 >= METHOD_BASE {
            let module = self
                .module()
                .ok_or_else(|| internal("the object has no module"))?;
            let name = module
                .module()
                .methods
                .get((m.0 - METHOD_BASE) as usize)
                .map(|c| c.name.name.to_string())
                .ok_or_else(|| internal("no such module method"))?;
            return ex
                .call_weighted(&module, &name, args, EXPORT_CALL_WEIGHT)
                .map_err(CallError::Exc);
        }
        let member = self.member(ex, m);
        if member.is_some_and(|(method, _)| method) {
            self.live()?;
        }
        let catalog = self.app.model().obj(self.obj).kind == ObjKind::Catalog;
        match member {
            Some((true, "Заполнить")) => self.fill(ex, args),
            Some((true, "ЭтоНовый")) => Ok(Value::Bool(self.state.borrow().is_new)),
            Some((true, "Модифицированность")) => {
                Ok(Value::Bool(self.state.borrow().modified.get()))
            }
            Some((true, "Метаданные")) => {
                use crate::meta::{MdKey, MdPath, value_of};
                let cx = self
                    .app
                    .md_ctx()
                    .ok_or_else(|| internal("no Метаданные context"))?;
                Ok(value_of(&cx, ex, MdKey::Path(MdPath::Object(self.obj))))
            }
            Some((true, "Записать")) if catalog => {
                crate::write::write(ex, &self.rc()?)?;
                Ok(Value::Undefined)
            }
            Some((true, "Прочитать")) if catalog => self.read_again(ex),
            Some((true, "Уровень")) if catalog => self.level(ex),
            Some((true, "ПроверитьЗаполнение")) if catalog => {
                Ok(Value::Bool(crate::events::check_filling(ex, &self.rc()?)?))
            }
            Some((true, "Скопировать")) if catalog => self.copy(ex),
            Some((true, "УстановитьНовыйКод")) if catalog => {
                crate::codes::set_new_code_method(ex, &self.rc()?, args)
            }
            Some((true, "УстановитьСсылкуНового")) if catalog => {
                self.set_new_ref(ex, args)
            }
            Some((true, "ПолучитьСсылкуНового")) if catalog => {
                self.get_new_ref()
            }
            Some((true, ru @ ("Удалить" | "УстановитьПометкуУдаления"))) if catalog => {
                crate::write_hooks::delete(ex, self, ru)
            }
            Some((_, ru)) => Err(self.not_implemented(ru).into()),
            None => Err(self.not_implemented(&m.0.to_string()).into()),
        }
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        let def = self.app.model().obj(self.obj);
        if def.kind == ObjKind::Enum {
            return None;
        }
        if let Some(i) = field_index(def, name).and_then(|i| u32::try_from(i).ok()) {
            return Some(MemberId(ATTR_BASE + i));
        }
        if let Some(i) = tabular_index(def, name).and_then(|i| u32::try_from(i).ok()) {
            return Some(MemberId(TAB_BASE + i));
        }
        let module = self.module()?;
        if let Some(i) = Self::exported_var(&module, name).and_then(|i| u32::try_from(i).ok()) {
            return Some(MemberId(VAR_BASE + i));
        }
        module
            .module()
            .method(name)
            .filter(|(_, code)| code.export)
            .map(|(i, _)| MemberId(METHOD_BASE + i))
    }

    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        if (ATTR_BASE..METHOD_BASE).contains(&m.0) {
            return Some(MemberKind::Property {
                readable: true,
                writable: true,
            });
        }
        if m.0 < METHOD_BASE {
            return None;
        }
        let module = self.module()?;
        let code = module.module().methods.get((m.0 - METHOD_BASE) as usize)?;
        let clamp = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
        let required = code
            .params
            .iter()
            .rposition(|p| p.default.is_none())
            .map_or(0, |i| i + 1);
        Some(MemberKind::Method {
            is_function: code.kind == MethodKind::Function,
            min_args: clamp(required),
            max_args: Some(clamp(code.params.len())),
        })
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<bsl_prim::Str>> {
        crate::present::object_presentation(ex, &self.app, self).map(Some)
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let any = other.as_any();
        if let Some(o) = any.downcast_ref::<ObjectInstance>() {
            return Some(std::ptr::eq(self, o));
        }
        if let Some(c) = any.downcast_ref::<ObjectCtx>() {
            return Some(std::ptr::eq(self, c.inst.as_ptr()));
        }
        Some(false)
    }
}

struct ObjectCtx {
    inst: Weak<ObjectInstance>,
    ty: TypeId,
}

impl ObjectCtx {
    fn target(&self) -> Result<Rc<ObjectInstance>, HostError> {
        self.inst
            .upgrade()
            .ok_or_else(|| internal("the object instance is gone"))
    }
}

impl HostObject for ObjectCtx {
    fn type_id(&self) -> TypeId {
        self.ty
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        self.target()?.get(ex, m)
    }

    fn set(&self, ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        self.target()?.set(ex, m, v)
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        self.target()?.call(ex, m, args)
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        self.inst.upgrade()?.find_member(name)
    }

    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        self.inst.upgrade()?.member_kind(m)
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let t = self.inst.upgrade()?;
        t.equals(other)
    }
}
