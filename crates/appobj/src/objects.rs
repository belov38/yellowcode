use crate::Application;
use crate::errors::{self, keys};
use crate::meta::member_name;
use crate::model::{ObjId, ObjKind};
use crate::read::{ReadRow, field_index, is_std_name, read_fresh, tabular_index};
use crate::readtab::{retained_bytes, tabular_value_with_live};
use crate::types::Category;
use crate::values::refs::{RefValue, empty_ref, ref_value};
use bsl_prim::fold_name;
use bsl_syntax::ast::MethodKind;
use bsl_vm::{
    Arg, CallError, CallResult, Category as ErrCat, CategorySet, Execution, HostError, HostObject,
    MemberId, MemberKind, ModuleContext, ModuleInstanceRef, ObjRef, TypeId, Value,
};
use platform_catalog::messages::Lang;
use std::any::Any;
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;

const ATTR_BASE: u32 = 1 << 20;
const TAB_BASE: u32 = 1 << 21;
const VAR_BASE: u32 = 1 << 22;
const METHOD_BASE: u32 = 1 << 23;

const EXPORT_CALL_WEIGHT: u32 = 120;

const BODY_WEIGHT: u32 = 516;

struct State {
    r: Option<RefValue>,
    row: Rc<ReadRow>,
    tabular: Vec<Option<Value>>,
    is_new: bool,
    modified: bool,
    data_version: String,
    #[allow(dead_code)]
    extra: Value,
    module: Option<ModuleInstanceRef>,
    attributes: Vec<Option<Value>>,
}

pub struct ObjectInstance {
    app: Arc<Application>,
    obj: ObjId,
    ty: TypeId,
    me: Weak<ObjectInstance>,
    state: RefCell<State>,
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
    instance(ex, app, r, row, false)
}

pub(crate) fn create_item(ex: &mut Execution, app: &Application, obj: ObjId) -> CallResult<Value> {
    let r = RefValue {
        obj,
        id: ibschema::values::RefId::NIL,
    };
    let row = read_fresh(ex, app, r)?;
    instance(ex, app, r, row, true)
}

fn instance(
    ex: &mut Execution,
    app: &Application,
    r: RefValue,
    row: ReadRow,
    is_new: bool,
) -> CallResult<Value> {
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
    let mut tabular = Vec::with_capacity(def.tabular.len());
    for i in 0..def.tabular.len() {
        let value = tabular_value_with_live(ex, &arc, r, i, live)?;
        live = live.saturating_add(retained_bytes(&value));
        crate::read_budget::reserve(ex, live)?;
        tabular.push(Some(value));
    }
    let ty = app
        .types()
        .and_then(|t| t.type_of(r.obj, Category::Object))
        .ok_or_else(|| internal("no object type"))?;
    let data_version = row.version.clone();
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
            modified: false,
            data_version,
            extra: Value::Undefined,
            module: None,
            attributes: if is_new {
                vec![None; def.fields.len()]
            } else {
                Vec::new()
            },
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
    Ok(Value::Object(inst))
}

impl ObjectInstance {
    pub(crate) fn presentation_parts(&self) -> (ObjId, Option<RefValue>, Rc<ReadRow>) {
        let st = self.state.borrow();
        (self.obj, st.r, st.row.clone())
    }

    fn module(&self) -> Option<ModuleInstanceRef> {
        self.state.borrow().module.clone()
    }

    fn type_name(&self) -> String {
        format!(
            "{}Объект",
            self.app.model().obj(self.obj).kind.name(Lang::Ru)
        )
    }

    fn not_implemented(&self, member: &str) -> HostError {
        HostError::not_implemented(&self.type_name(), member)
    }

    fn member(&self, ex: &Execution, m: MemberId) -> Option<(bool, &'static str)> {
        let t = ex.runtime().types().app_type(self.ty)?.template?;
        member_name(platform_catalog::catalog(), t, m).map(|(method, ru, _)| (method, ru))
    }

    fn attr(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
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
            Some(Some(v)) => Ok(v.clone()),
            _ => Err(internal("no such tabular section").into()),
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
}

impl HostObject for ObjectInstance {
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
                let r = self.state.borrow().r;
                match r {
                    Some(r) => Ok(ref_value(&self.app, r)?),
                    None => Ok(empty_ref(&self.app, self.obj)?),
                }
            }
            Some((false, "ВерсияДанных")) => {
                Ok(Value::from(self.state.borrow().data_version.as_str()))
            }
            Some((false, "ЭтотОбъект")) => self.this(),
            Some((false, "ДополнительныеСвойства")) => {
                Err(self.not_implemented("ДополнительныеСвойства").into())
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
        if (ATTR_BASE..TAB_BASE).contains(&m.0) && self.state.borrow().is_new {
            let index = (m.0 - ATTR_BASE) as usize;
            let field = self
                .app
                .model()
                .obj(self.obj)
                .fields
                .get(index)
                .ok_or_else(|| internal("no such object field"))?;
            if field.md.is_some() && crate::session::accepts(&field.types, &v) {
                let bytes = {
                    let state = self.state.borrow();
                    state
                        .attributes
                        .iter()
                        .flatten()
                        .fold(state.row.bytes(), |sum, value| {
                            sum.saturating_add(crate::read_budget::value_heap(value))
                        })
                        .saturating_add(crate::read_budget::value_heap(&v))
                        .saturating_add(crate::read_budget::slots::<Option<Value>>(
                            state.attributes.capacity(),
                        ))
                };
                crate::read_budget::reserve(ex, bytes)?;
                let mut state = self.state.borrow_mut();
                let slot = state
                    .attributes
                    .get_mut(index)
                    .ok_or_else(|| internal("no such object attribute"))?;
                *slot = Some(v);
                state.modified = true;
                return Ok(());
            }
        }
        let name = if (ATTR_BASE..TAB_BASE).contains(&m.0) {
            self.app
                .model()
                .obj(self.obj)
                .fields
                .get((m.0 - ATTR_BASE) as usize)
                .map(|f| f.name.clone())
        } else if (TAB_BASE..VAR_BASE).contains(&m.0) {
            self.app
                .model()
                .obj(self.obj)
                .tabular
                .get((m.0 - TAB_BASE) as usize)
                .map(|t| t.name.clone())
        } else {
            self.member(ex, m).map(|n| n.1.to_string())
        };
        Err(self.not_implemented(&name.unwrap_or_default()).into())
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
        match self.member(ex, m) {
            Some((true, "Заполнить")) if self.state.borrow().is_new => {
                let data = crate::meta::arg_value(ex, args, 0)?;
                if !matches!(data, Value::Undefined) {
                    return Err(self.not_implemented("Заполнить: данные заполнения").into());
                }
                if let Some(module) = self.module()
                    && module.module().method("ОбработкаЗаполнения").is_some()
                {
                    ex.call_weighted(
                        &module,
                        "ОбработкаЗаполнения",
                        &mut [
                            Arg::Val(data),
                            Arg::Place(bsl_vm::Place::temp(Value::from(""))),
                            Arg::Place(bsl_vm::Place::temp(Value::Bool(true))),
                        ],
                        EXPORT_CALL_WEIGHT,
                    )
                    .map_err(CallError::Exc)?;
                }
                Ok(Value::Undefined)
            }
            Some((true, "ЭтоНовый")) => Ok(Value::Bool(self.state.borrow().is_new)),
            Some((true, "Модифицированность")) => {
                Ok(Value::Bool(self.state.borrow().modified))
            }
            Some((true, "Метаданные")) => {
                use crate::meta::{MdKey, MdPath, value_of};
                let cx = self
                    .app
                    .md_ctx()
                    .ok_or_else(|| internal("no Метаданные context"))?;
                Ok(value_of(&cx, ex, MdKey::Path(MdPath::Object(self.obj))))
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
