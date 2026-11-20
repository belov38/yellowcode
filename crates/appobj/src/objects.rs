mod fill_names;

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

pub(crate) const BODY_WEIGHT: u32 = 516;

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
    pub(crate) movements: Option<Value>,
    pub(crate) module: Option<ModuleInstanceRef>,
    pub(crate) attributes: Vec<Option<Value>>,
    pub(crate) new_ref: Option<RefValue>,
    pub(crate) dead: bool,
    pub(crate) written: Option<WriteStamp>,
    pub(crate) group_without_hierarchy: bool,
    pub(crate) issued: Option<crate::codes::IssuedInTxn>,
    pub(crate) created_in: Option<(std::rc::Weak<crate::txn::CallTxn>, u64)>,
    pub(crate) failed_new_write: Option<WriteStamp>,
    pub(crate) document_write: Option<(WriteStamp, bool)>,
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
    issued: Option<crate::codes::IssuedInTxn>,
    tabs: Vec<TabState>,
}

pub struct ObjectInstance {
    pub(crate) app: Arc<Application>,
    pub(crate) obj: ObjId,
    ty: TypeId,
    me: Weak<ObjectInstance>,
    pub(crate) state: RefCell<State>,
    pub(crate) code_holder: Arc<crate::numerator::HolderMark>,
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

fn document_not_selected() -> HostError {
    let mut e = match errors::msg(keys::DOCUMENT_NOT_SELECTED) {
        Some(m) => HostError::text(m.ru, m.en),
        None => internal(keys::DOCUMENT_NOT_SELECTED),
    };
    e.categories = not_selected().categories;
    e
}

fn internal(text: &str) -> HostError {
    HostError::text(&format!("internal: {text}"), &format!("internal: {text}"))
}

pub fn get_object(ex: &mut Execution, app: &Application, r: RefValue) -> CallResult<Value> {
    if r.is_empty() {
        return Err(not_selected().into());
    }
    Ok(match stored_instance(ex, app, r)? {
        Some(inst) => Value::Object(inst),
        None => Value::Undefined,
    })
}

pub(crate) fn stored_instance(
    ex: &mut Execution,
    app: &Application,
    r: RefValue,
) -> CallResult<Option<Rc<ObjectInstance>>> {
    let row = read_fresh(ex, app, r)?;
    if !row.found {
        return Ok(None);
    }
    instance(ex, app, r, row, false, false).map(Some)
}

pub(crate) fn create_document(
    ex: &mut Execution,
    app: &Application,
    obj: ObjId,
) -> CallResult<Value> {
    Ok(Value::Object(new_instance(ex, app, obj, false)?))
}

pub(crate) fn create_item(ex: &mut Execution, app: &Application, obj: ObjId) -> CallResult<Value> {
    Ok(Value::Object(new_instance(ex, app, obj, false)?))
}

pub(crate) fn create_group(ex: &mut Execution, app: &Application, obj: ObjId) -> CallResult<Value> {
    let def = app.model().obj(obj);
    let folders = def.behavior.hierarchy_folders
        && def.fields.iter().any(|f| f.std == Some(StdAttr::IsFolder));
    if def.behavior.hierarchical && !folders {
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
    let inst = instance(ex, app, r, row, true, folder)?;
    if !folder
        && let Some(txn) = crate::txn::CallTxn::of_call(ex)
        && let Some(id) = txn.id()
    {
        inst.state.borrow_mut().created_in = Some((Rc::downgrade(&txn), id));
    }
    Ok(inst)
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
    let folder_field = def
        .fields
        .iter()
        .position(|f| f.std == Some(StdAttr::IsFolder));
    if folder && let Some(slot) = folder_field.and_then(|i| attributes.get_mut(i)) {
        *slot = Some(Value::Bool(true));
    }
    let inst = Rc::new_cyclic(|me| ObjectInstance {
        app: arc,
        obj: r.obj,
        ty,
        me: me.clone(),
        code_holder: crate::numerator::HolderMark::new(),
        state: RefCell::new(State {
            r: (!is_new).then_some(r),
            row: Rc::new(row),
            tabular,
            is_new,
            modified,
            data_version,
            extra: None,
            exchange: None,
            movements: None,
            module: None,
            attributes,
            new_ref: None,
            dead: false,
            written: None,
            group_without_hierarchy: folder && folder_field.is_none(),
            issued: None,
            created_in: None,
            failed_new_write: None,
            document_write: None,
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

    fn document_after_rollback(&self, method: &str) -> Option<HostError> {
        let st = self.state.borrow();
        let failed = st
            .failed_new_write
            .as_ref()
            .is_some_and(|w| w.txn.upgrade().is_some_and(|txn| txn.was_rolled_back(w.id)));
        if failed {
            return Some(self.not_implemented(&format!(
                "{method}: после отката транзакции с несостоявшейся записью нового документа не наблюдено"
            )));
        }
        if !st.new_write_rolled_back() {
            return None;
        }
        let set_04 = st.document_write.as_ref().is_some_and(|(w, single)| {
            *single && w.txn.upgrade().is_some_and(|txn| txn.was_rolled_back(w.id))
        });
        if method == "ЭтоНовый" && set_04 {
            return Some(document_not_selected());
        }
        Some(self.not_implemented(&format!(
            "{method}: после отката записи нового документа не наблюдено"
        )))
    }

    fn document_property_after_rollback(
        &self,
        ex: &mut Execution,
        m: MemberId,
    ) -> Option<HostError> {
        let def = self.app.model().obj(self.obj);
        if def.kind != ObjKind::Document {
            return None;
        }
        self.document_after_rollback("")?;
        let name = if (ATTR_BASE..TAB_BASE).contains(&m.0) {
            def.fields.get((m.0 - ATTR_BASE) as usize)?.name.clone()
        } else if (TAB_BASE..VAR_BASE).contains(&m.0) {
            def.tabular.get((m.0 - TAB_BASE) as usize)?.name.clone()
        } else if (VAR_BASE..METHOD_BASE).contains(&m.0) {
            "переменная модуля".to_string()
        } else {
            match self.member(ex, m) {
                Some((false, "Движения")) => return None,
                Some((_, ru)) => ru.to_string(),
                None => m.0.to_string(),
            }
        };
        self.document_after_rollback(&name)
    }

    pub(crate) fn note_document_write(&self, txn: &Rc<CallTxn>, plain: bool) {
        let Some(id) = txn.id() else { return };
        let mut st = self.state.borrow_mut();
        let first = !st
            .document_write
            .as_ref()
            .is_some_and(|(w, _)| w.id == id && std::ptr::eq(w.txn.as_ptr(), Rc::as_ptr(txn)));
        st.document_write = Some((
            WriteStamp {
                txn: Rc::downgrade(txn),
                id,
                was_new: true,
            },
            first && plain,
        ));
    }

    pub(crate) fn note_failed_new_write(&self, txn: &Rc<CallTxn>) {
        self.state.borrow_mut().failed_new_write = Some(WriteStamp {
            txn: Rc::downgrade(txn),
            id: txn.id().unwrap_or(0),
            was_new: true,
        });
    }

    pub(crate) fn refuse_group_without_hierarchy(&self, method: &str) -> Result<(), HostError> {
        if self.state.borrow().group_without_hierarchy {
            return Err(self.not_implemented(&format!("{method}: группа справочника без иерархии")));
        }
        Ok(())
    }

    pub(crate) fn take_ref(&self, txn: &Rc<CallTxn>, id: RefId) {
        let mut st = self.state.borrow_mut();
        st.r = Some(RefValue { obj: self.obj, id });
        st.is_new = false;
        st.new_ref = None;
        st.written = Some(WriteStamp {
            txn: Rc::downgrade(txn),
            id: txn.id().unwrap_or(0),
            was_new: true,
        });
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
            issued: st.issued.clone(),
            tabs: st.tabular.iter().map(|t| t.state()).collect(),
        }
    }

    pub(crate) fn forget(&self, blank: ReadRow) {
        let mut st = self.state.borrow_mut();
        st.dead = true;
        st.r = None;
        st.new_ref = None;
        st.row = Rc::new(blank);
        for slot in st.attributes.iter_mut() {
            *slot = None;
        }
        for tab in &st.tabular {
            tab.replace(Vec::new());
        }
        st.modified.set(false);
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
        st.issued = s.issued;
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

    fn std_without_field(&self, ru: &str) -> CallResult<Value> {
        let Some(attr) = crate::read::std_attr_named(ru) else {
            return Err(self.not_implemented(ru).into());
        };
        if attr == StdAttr::IsFolder {
            self.refuse_group_without_hierarchy(ru)?;
        }
        match crate::read::catalog_std_default(&self.app, self.obj, attr)? {
            Some(v) => Ok(v),
            None => Err(Self::no_field(ru).into()),
        }
    }

    fn indexed(&self, ex: &Execution, key: &Value) -> Result<(MemberId, bool, String), HostError> {
        let Value::String(name) = key else {
            return Err(self.not_implemented("[ключ не строка]"));
        };
        let name = name.to_string_lossy();
        if self.app.model().obj(self.obj).kind != ObjKind::Catalog {
            return Err(self.not_implemented("[индекс]"));
        }
        if let Some((m, MemberKind::Property { writable, .. })) =
            ex.runtime().types().member(self.ty, &name)
        {
            return Ok((m, writable, name));
        }
        match self.find_member(&name) {
            Some(m) if (ATTR_BASE..VAR_BASE).contains(&m.0) => Ok((m, true, name)),
            _ => Err(self.not_implemented(&format!("[{name}]"))),
        }
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

    pub(crate) fn set_posted(&self, posted: bool) -> Result<(), HostError> {
        let def = self.app.model().obj(self.obj);
        let i = def
            .fields
            .iter()
            .position(|f| f.std == Some(StdAttr::Posted))
            .ok_or_else(|| internal("a document without Проведен"))?;
        let mut st = self.state.borrow_mut();
        if st.attributes.len() < def.fields.len() {
            st.attributes.resize(def.fields.len(), None);
        }
        let slot = st
            .attributes
            .get_mut(i)
            .ok_or_else(|| internal("no such object attribute"))?;
        *slot = Some(Value::Bool(posted));
        Ok(())
    }

    pub(crate) fn written_in(&self, txn: &CallTxn) -> bool {
        let id = txn.id();
        self.state
            .borrow()
            .written
            .as_ref()
            .is_some_and(|w| id == Some(w.id))
    }

    pub(crate) fn movements_of(&self, ex: &mut Execution) -> CallResult<Value> {
        self.movements(ex)
    }

    fn movements(&self, ex: &mut Execution) -> CallResult<Value> {
        if let Some(v) = self.state.borrow().movements.clone() {
            return Ok(v);
        }
        if let Some(e) = self.document_after_rollback("Движения")
            && self.app.model().obj(self.obj).kind == ObjKind::Document
        {
            return Err(e.into());
        }
        let r = {
            let st = self.state.borrow();
            st.r.filter(|_| !st.is_new && !st.new_write_rolled_back())
        };
        let v = crate::regread::Movements::create(ex, &self.app, self.obj, r)?;
        self.state.borrow_mut().movements = Some(v.clone());
        Ok(v)
    }

    pub(crate) fn exchange(&self) -> Result<Rc<ExchangeParams>, HostError> {
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
        let storage = def.kind == crate::model::ObjKind::Catalog
            && matches!(field.types.single(), Some(TypeAtom::ValueStorage))
            && bsl_stdlib::value_storage_bytes(&v).is_some();
        let cast = if storage {
            v
        } else {
            crate::values::typedesc::cast_to_field(&self.app, ex, field, v)?
        };
        if field.std == Some(StdAttr::PredefinedDataName) {
            self.check_predefined_name(&cast)?;
        }
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
        crate::codes::manual_code(ex, self, index, &cast)?;
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

    fn check_predefined_name(&self, v: &Value) -> Result<(), HostError> {
        let name = match v {
            Value::String(s) => s.to_string_lossy(),
            _ => return Ok(()),
        };
        if name.is_empty() {
            return Ok(());
        }
        let folded = fold_name(&name);
        let known = self
            .app
            .predefined()
            .names(&self.app, self.obj)
            .iter()
            .any(|(_, n)| fold_name(n) == folded);
        if known {
            Ok(())
        } else {
            Err(crate::write::use_error(keys::PREDEFINED_ITEM_NOT_EXISTS))
        }
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
            return self.read_new();
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

    fn read_new(&self) -> CallResult<Value> {
        self.refuse_group_without_hierarchy("Прочитать")?;
        let unknown = {
            let st = self.state.borrow();
            if self.folder_flag() == Some(true) {
                Some("новая группа")
            } else if st.tabular.iter().any(|t| !t.is_empty()) {
                Some("новый объект со строками табличной части")
            } else if st.new_ref.is_some() {
                Some("новый объект со ссылкой УстановитьСсылкуНового")
            } else {
                None
            }
        };
        if let Some(what) = unknown {
            return Err(self.not_implemented(&format!("Прочитать: {what}")).into());
        }
        let mut st = self.state.borrow_mut();
        for slot in st.attributes.iter_mut() {
            *slot = None;
        }
        st.modified.set(false);
        Ok(Value::Undefined)
    }

    fn copy(&self, ex: &mut Execution) -> CallResult<Value> {
        self.refuse_group_without_hierarchy("Скопировать")?;
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
            if (matches!(v, Value::Undefined) || crate::values::field::is_empty_storage(&v))
                && f.types
                    .atoms
                    .iter()
                    .any(|a| matches!(a, TypeAtom::ValueStorage | TypeAtom::Binary))
            {
                continue;
            }
            if def.kind != crate::model::ObjKind::Catalog
                && bsl_stdlib::value_storage_bytes(&v).is_some()
            {
                return Err(self
                    .not_implemented(&format!("Скопировать: {} со значением", f.name))
                    .into());
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
            st.modified.set(true);
        }
        crate::write_hooks::after_copy(ex, &new, self)?;
        new.state.borrow().modified.set(false);
        Ok(Value::Object(new))
    }

    fn level(&self, ex: &mut Execution) -> CallResult<Value> {
        let stored = {
            let st = self.state.borrow();
            if st.is_new { None } else { st.r }
        };
        if let Some(r) = stored {
            return crate::hierarchy::call(ex, &self.app, r, "Уровень", &[]);
        }
        let parent = match crate::hierarchy::parent_field(&self.app, self.obj) {
            Some(i) => as_ref(&self.attr(ex, i)?).filter(|p| !p.is_empty()),
            None => None,
        };
        let Some(parent) = parent else {
            return Err(self.not_implemented("Уровень: новый объект").into());
        };
        let up = crate::hierarchy::call(ex, &self.app, parent, "Уровень", &[])?;
        match &up {
            Value::Number(n) => match n.to_i64().and_then(|n| n.checked_add(1)) {
                Some(level) => Ok(Value::from(level)),
                None => Err(self.not_implemented("Уровень: новый объект").into()),
            },
            _ => Err(self.not_implemented("Уровень: новый объект").into()),
        }
    }

    fn fill(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let data = arg_value(ex, args, 0)?;
        let def = self.app.model().obj(self.obj);
        if def.kind != ObjKind::Catalog {
            if !self.state.borrow().is_new {
                return Err(self.not_implemented("Заполнить").into());
            }
            let document_ref = def.kind == ObjKind::Document && as_ref(&data).is_some();
            if !matches!(data, Value::Undefined) && !document_ref {
                return Err(self.not_implemented("Заполнить: данные заполнения").into());
            }
        }
        let text = bsl_vm::Place::temp(Value::Undefined);
        let standard = bsl_vm::Place::temp(Value::Bool(true));
        crate::events::filling(ex, &self.rc()?, data.clone(), &text, &standard)?;
        if def.kind == ObjKind::Catalog {
            let on = !matches!(standard.temp_value(), Some(Value::Bool(false)));
            if on {
                if let Some(r) = as_ref(&data)
                    && let Some(name) = self.ref_fill_target(r)
                {
                    return Err(self
                        .not_implemented(&format!(
                            "Заполнить: ссылка как данные заполнения ({name})"
                        ))
                        .into());
                }
                self.fill_from_structure(ex, &data)?;
                self.apply_fill_values(ex)?;
            }
            self.state.borrow().modified.set(true);
        } else if def.kind == ObjKind::Document {
            let on = !matches!(standard.temp_value(), Some(Value::Bool(false)));
            if on {
                if let Some(r) = as_ref(&data) {
                    self.document_ref_fill(ex, r)?;
                }
                self.apply_fill_values(ex)?;
            }
            self.state.borrow().modified.set(true);
        }
        Ok(Value::Undefined)
    }

    fn document_ref_fill(&self, ex: &mut Execution, r: RefValue) -> CallResult<()> {
        for i in self.ref_fill_fields(r) {
            if as_ref(&self.attr(ex, i)?) != Some(r) {
                let model = self.app.model();
                let name = model
                    .obj(self.obj)
                    .fields
                    .get(i)
                    .map_or("", |f| f.name.as_str());
                return Err(self
                    .not_implemented(&format!("Заполнить: ссылка как данные заполнения ({name})"))
                    .into());
            }
        }
        Ok(())
    }

    fn fill_from_structure(&self, ex: &mut Execution, data: &Value) -> CallResult<()> {
        let def = self.app.model().obj(self.obj);
        if let Some(md) = self.app.model().config.object(def.md) {
            for f in def.fields.iter().filter(|f| f.std.is_none()) {
                let on =
                    f.md.and_then(|id| md.children.iter().find(|c| c.id == id))
                        .and_then(|c| c.props.iter().find(|p| p.name == "FillFromFillingValue"))
                        .is_some_and(|p| match &p.value {
                            md_model::Value::Bool(b) => *b,
                            md_model::Value::Text(t) => t == "true",
                            _ => false,
                        });
                if on && bsl_stdlib::structure_get(data, &f.name).is_some() {
                    return Err(self
                        .not_implemented(&format!("Заполнить: {} из структуры", f.name))
                        .into());
                }
            }
        }
        for attr in [StdAttr::Parent, StdAttr::Owner] {
            let name = attr.names().0;
            let Some(v) = bsl_stdlib::structure_get(data, name) else {
                continue;
            };
            if attr == StdAttr::Owner && matches!(v, Value::String(_)) {
                continue;
            }
            let Some((i, field)) = def
                .fields
                .iter()
                .enumerate()
                .find(|(_, f)| f.std == Some(attr))
            else {
                continue;
            };
            let plain = platform_catalog::catalog()
                .find_type("Структура")
                .is_some_and(|t| data.type_id() == TypeId::of_catalog(t));
            let fits = as_ref(&v).is_some_and(|r| {
                let kind = self.app.model().obj(r.obj).kind;
                field.types.atoms.iter().any(|a| match a {
                    TypeAtom::Ref(o) => *o == r.obj,
                    TypeAtom::AnyRef(k) => k.is_none_or(|k| k == kind),
                    _ => false,
                })
            });
            if !plain || !fits {
                return Err(self
                    .not_implemented(&format!("Заполнить: {name} из структуры"))
                    .into());
            }
            self.assign(ex, i, v)?;
        }
        Ok(())
    }

    fn ref_fill_target(&self, r: RefValue) -> Option<String> {
        let first = *self.ref_fill_fields(r).first()?;
        Some(
            self.app
                .model()
                .obj(self.obj)
                .fields
                .get(first)?
                .name
                .clone(),
        )
    }

    fn ref_fill_fields(&self, r: RefValue) -> Vec<usize> {
        let model = self.app.model();
        let def = model.obj(self.obj);
        let kind = model.obj(r.obj).kind;
        let md = model.config.object(def.md);
        def.fields
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                let candidate = match (f.std, f.md) {
                    (Some(StdAttr::Parent | StdAttr::Owner), _) => true,
                    (_, Some(id)) => md
                        .and_then(|md| md.children.iter().find(|c| c.id == id))
                        .and_then(|c| c.props.iter().find(|p| p.name == "FillFromFillingValue"))
                        .is_some_and(|p| match &p.value {
                            md_model::Value::Bool(b) => *b,
                            md_model::Value::Text(t) => t == "true",
                            _ => false,
                        }),
                    _ => false,
                };
                candidate
                    && f.types.atoms.iter().any(|a| match a {
                        TypeAtom::Ref(o) => *o == r.obj,
                        TypeAtom::AnyRef(k) => k.is_none_or(|k| k == kind),
                        TypeAtom::Platform(_) => true,
                        _ => false,
                    })
            })
            .map(|(i, _)| i)
            .collect()
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
                Some(md_model::Value::Typed(md_model::TypedValue::DesignTimeRef(t)))
                    if t.ends_with(".EmptyRef")
                        && f.types.single().is_some()
                        && as_ref(&self.attr(ex, i)?).is_some() =>
                {
                    continue;
                }
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
        if let Some(e) = self.document_property_after_rollback(ex, m) {
            return Err(e.into());
        }
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
            Some((false, "Движения"))
                if self.app.model().obj(self.obj).kind == ObjKind::Document =>
            {
                self.movements(ex)
            }
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
                    None if def.kind == ObjKind::Catalog => self.std_without_field(ru),
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
            let document_attr =
                def.kind == ObjKind::Document && matches!(field.std, None | Some(StdAttr::Date));
            if def.kind != ObjKind::Catalog && !document_attr {
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
                    Some(StdAttr::Owner) => {
                        Err(crate::write::use_error(keys::CATALOG_HAS_NO_OWNER).into())
                    }
                    Some(StdAttr::Parent) => {
                        Err(crate::write::use_error(keys::CATALOG_NOT_MULTILEVEL).into())
                    }
                    Some(attr)
                        if !matches!(
                            attr,
                            StdAttr::Ref | StdAttr::Predefined | StdAttr::IsFolder
                        ) =>
                    {
                        Err(self
                            .not_implemented(&format!("{ru}: реквизит нулевой длины"))
                            .into())
                    }
                    _ => Err(not_writable(ru).into()),
                },
            },
            Some((false, ru)) if def.kind == ObjKind::Document => match field_index(def, ru) {
                Some(i)
                    if matches!(
                        def.fields.get(i).and_then(|f| f.std),
                        None | Some(StdAttr::Date)
                    ) =>
                {
                    self.assign(ex, i, v)
                }
                _ => Err(self.not_implemented(ru).into()),
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
        let kind = self.app.model().obj(self.obj).kind;
        if kind == ObjKind::Document
            && let Some((true, method)) = member
            && let Some(e) = self.document_after_rollback(method)
        {
            return Err(e.into());
        }
        if member.is_some_and(|(method, _)| method) {
            self.live()?;
        }
        let catalog = kind == ObjKind::Catalog;
        let document = kind == ObjKind::Document;
        match member {
            Some((true, "Записать")) if document => {
                crate::write::write_document(ex, &self.rc()?, args)?;
                Ok(Value::Undefined)
            }
            Some((true, "УстановитьНовыйНомер")) if document => {
                crate::codes::set_new_number_method(ex, &self.rc()?, args)
            }
            Some((true, "Удалить")) if document => crate::delete::delete_document(ex, self),
            Some((true, "УстановитьПометкуУдаления")) if document => {
                crate::delete::set_mark_document(ex, self, args)
            }
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
            Some((true, "ПроверитьЗаполнение")) if catalog || document => {
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
                crate::write_hooks::delete(ex, self, ru, args)
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

    fn property_names(
        &self,
        ex: &mut Execution,
        retained: u64,
    ) -> CallResult<Option<Vec<bsl_prim::Str>>> {
        fill_names::names(self, ex, retained)
    }

    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let (m, _, _) = self.indexed(ex, key)?;
        self.get(ex, m)
    }

    fn index_set(&self, ex: &mut Execution, key: &Value, v: Value) -> CallResult<()> {
        let (_, writable, name) = self.indexed(ex, key)?;
        let def = self.app.model().obj(self.obj);
        let zero_length = field_index(def, &name).is_none()
            && matches!(
                crate::read::std_attr_named(&name),
                Some(StdAttr::Code | StdAttr::Description)
            );
        if !writable || zero_length {
            return Err(not_writable(&name).into());
        }
        let me = self.this()?;
        ex.set_member(&me, &name, v).map_err(CallError::Exc)
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

    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        self.target()?.index_get(ex, key)
    }

    fn index_set(&self, ex: &mut Execution, key: &Value, v: Value) -> CallResult<()> {
        self.target()?.index_set(ex, key, v)
    }

    fn property_names(
        &self,
        ex: &mut Execution,
        retained: u64,
    ) -> CallResult<Option<Vec<bsl_prim::Str>>> {
        self.target()?.property_names(ex, retained)
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let t = self.inst.upgrade()?;
        t.equals(other)
    }
}
