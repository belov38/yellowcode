mod filter;
mod record;
mod store;

use crate::exchange_params::ExchangeParams;
use crate::meta::{MdKey, MdPath, arg_value, member_name, no_field, value_of};
use crate::model::{FieldDef, FieldRole, ObjDef, ObjId, StdAttr, TypeAtom, TypeSet};
use crate::session::internal;
use crate::types::Category;
use crate::values::refs::as_ref;
use crate::{Application, read_budget};
use bsl_prim::fold_name;
use bsl_vm::{
    Arg, CallError, CallResult, Exception, Execution, HostError, HostObject, MemberId, MemberKind,
    ModuleContext, ModuleInstanceRef, ObjRef, RetainedMemory, TypeId, Value,
};
use filter::Filter;
use md_model::{MdKind, MdObject};
use platform_catalog::messages::Lang;
use record::Record;
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;

const FIELD_BASE: u32 = 1 << 20;

const STANDARD: [(StdAttr, bool); 4] = [
    (StdAttr::Period, true),
    (StdAttr::Recorder, true),
    (StdAttr::LineNumber, false),
    (StdAttr::Active, false),
];

#[derive(Clone, Copy)]
pub(crate) enum Source {
    Field(usize),
    Separator(usize),
}

pub(crate) struct Column {
    name: String,
    source: Source,
    std: Option<StdAttr>,
    types: TypeSet,
    key: bool,
}

fn refusal(def: &ObjDef, suffix: &str, member: &str) -> HostError {
    HostError::not_implemented(&format!("{}{suffix}", def.kind.name(Lang::Ru)), member)
}

pub(crate) fn applies(ca: &MdObject, full: &str) -> bool {
    let auto = text(ca, "AutoUse").is_none_or(|u| u == "Use");
    let Some(md_model::Value::Xml(content)) = ca.prop("Content") else {
        return auto;
    };
    for item in content.children.iter().filter(|c| c.name == "xr:Item") {
        let part = |name: &str| {
            item.children
                .iter()
                .find(|c| c.name == name)
                .and_then(|c| c.text.as_deref())
        };
        if part("xr:Metadata") != Some(full) {
            continue;
        }
        return match part("xr:Use") {
            Some("Use") => true,
            Some("DontUse") => false,
            _ => auto,
        };
    }
    auto
}

pub(crate) fn text<'a>(o: &'a MdObject, name: &str) -> Option<&'a str> {
    match o.prop(name) {
        Some(md_model::Value::Text(t)) => Some(t.as_str()),
        _ => None,
    }
}

fn separators<'a>(
    app: &'a Application,
    def: &ObjDef,
) -> Result<Vec<(usize, &'a MdObject)>, HostError> {
    let full = format!("{}.{}", MdKind::InformationRegister.as_str(), def.name);
    let mut out = Vec::new();
    let all = app.model().config.objects.iter().enumerate();
    for (i, ca) in all.filter(|(_, o)| o.kind == MdKind::CommonAttribute) {
        if !applies(ca, &full) {
            continue;
        }
        if text(ca, "DataSeparation") != Some("Separate") {
            return Err(refusal(
                def,
                "НаборЗаписей",
                &format!("ОбщийРеквизит.{}", ca.name),
            ));
        }
        if text(ca, "SeparatedDataUse") == Some("IndependentlyAndSimultaneously") {
            out.push((i, ca));
        }
    }
    Ok(out)
}

fn separator_types(app: &Application, def: &ObjDef, ca: &MdObject) -> Result<TypeSet, HostError> {
    let Some(md_model::Value::Type(td)) = ca.prop("Type") else {
        return Err(refusal(
            def,
            "НаборЗаписей",
            &format!("ОбщийРеквизит.{}", ca.name),
        ));
    };
    let model = app.model();
    let by_type_id = |id| {
        model.objects.iter().find_map(|o| {
            o.generated
                .iter()
                .any(|g| g.category == "Ref" && g.type_id == id)
                .then_some(o.id)
        })
    };
    Ok(TypeSet::of(&model.config, td, &by_type_id))
}

fn columns(app: &Application, obj: ObjId) -> Result<Vec<Column>, HostError> {
    let def = app.model().obj(obj);
    let of_field = |i: usize, f: &FieldDef, key: bool| Column {
        name: f.name.clone(),
        source: Source::Field(i),
        std: f.std,
        types: f.types.clone(),
        key,
    };
    let mut out = Vec::with_capacity(def.fields.len().saturating_add(1));
    for (std, key) in STANDARD {
        let found = def
            .fields
            .iter()
            .enumerate()
            .find(|(_, f)| f.std == Some(std));
        if let Some((i, f)) = found {
            out.push(of_field(i, f, key));
        }
    }
    for (role, key) in [
        (FieldRole::Dimension, true),
        (FieldRole::Resource, false),
        (FieldRole::Attribute, false),
    ] {
        let wanted = def
            .fields
            .iter()
            .enumerate()
            .filter(|(_, f)| f.std.is_none() && f.role == role);
        for (i, f) in wanted {
            out.push(of_field(i, f, key));
        }
    }
    for (i, ca) in separators(app, def)? {
        out.push(Column {
            name: ca.name.clone(),
            source: Source::Separator(i),
            std: None,
            types: separator_types(app, def, ca)?,
            key: true,
        });
    }
    Ok(out)
}

fn empty_value(
    app: &Application,
    ex: &mut Execution,
    obj: ObjId,
    col: &Column,
) -> CallResult<Value> {
    if col.std == Some(StdAttr::Recorder) {
        return Ok(Value::Undefined);
    }
    let ty = column_type(app, ex, obj, col)?;
    let mut args = [Arg::Val(Value::Undefined)];
    Ok(ex.call_method(&ty, "ПривестиЗначение", &mut args, true)?)
}

fn column_type(
    app: &Application,
    ex: &mut Execution,
    obj: ObjId,
    col: &Column,
) -> CallResult<Value> {
    let model = app.model();
    let def = model.obj(obj);
    let described = |id| {
        let owner = model.config.objects.iter().find(|o| o.id == def.md)?;
        owner.children.iter().find(|c| c.id == id)?.type_desc()
    };
    match col.source {
        Source::Field(i) => {
            let f = def
                .fields
                .get(i)
                .ok_or_else(|| internal("a column without a field"))?;
            let Some(id) = f.md.filter(|_| f.std.is_none()) else {
                return crate::values::typedesc::from_standard(app, ex, &f.types);
            };
            let td = described(id).ok_or_else(|| internal("a field without a description"))?;
            crate::values::typedesc::from_metadata(app, ex, td)
        }
        Source::Separator(i) => match model.config.objects.get(i).and_then(|o| o.prop("Type")) {
            Some(md_model::Value::Type(td)) => crate::values::typedesc::from_metadata(app, ex, td),
            _ => Err(internal("a separator without a type").into()),
        },
    }
}

pub(crate) fn field_type(
    app: &Application,
    ex: &mut Execution,
    obj: ObjId,
    i: usize,
) -> CallResult<Value> {
    let f = app
        .model()
        .obj(obj)
        .fields
        .get(i)
        .ok_or_else(|| internal("no field of the register"))?;
    let col = Column {
        name: f.name.clone(),
        source: Source::Field(i),
        std: f.std,
        types: f.types.clone(),
        key: false,
    };
    column_type(app, ex, obj, &col)
}

fn member(ex: &Execution, ty: TypeId, m: MemberId) -> Option<(bool, &'static str, &'static str)> {
    let t = ex.runtime().types().app_type(ty)?.template?;
    member_name(platform_catalog::catalog(), t, m)
}

fn member_ru(ex: &Execution, ty: TypeId, m: MemberId) -> String {
    member(ex, ty, m).map_or_else(|| m.0.to_string(), |n| n.1.to_string())
}

fn type_of(app: &Application, obj: ObjId, cat: Category) -> Result<TypeId, HostError> {
    app.types()
        .and_then(|t| t.type_of(obj, cat))
        .ok_or_else(|| internal("the register has no type of its record set"))
}

pub(crate) fn accepts_key_value(app: &Application, types: &TypeSet, v: &Value) -> bool {
    if !crate::session::accepts(types, v) {
        return false;
    }
    if !matches!(v, Value::Object(_)) {
        return true;
    }
    let takes_any_object = types.atoms.iter().any(|a| {
        matches!(
            a,
            TypeAtom::Platform(_) | TypeAtom::Uuid | TypeAtom::ValueStorage | TypeAtom::Binary
        )
    });
    if takes_any_object {
        return true;
    }
    let Some(r) = as_ref(v) else {
        return false;
    };
    types.atoms.iter().any(|a| match a {
        TypeAtom::Ref(o) => *o == r.obj,
        TypeAtom::AnyRef(kind) => kind.is_none_or(|k| app.model().obj(r.obj).kind == k),
        _ => false,
    })
}

pub(crate) fn is_structure(v: &Value) -> bool {
    platform_catalog::catalog()
        .find_type("Структура")
        .is_some_and(|t| v.type_id() == TypeId::of_catalog(t))
}

pub(crate) fn manager_get(
    ex: &mut Execution,
    app: &Arc<Application>,
    obj: ObjId,
    args: &[Arg],
) -> CallResult<Value> {
    let def = app.model().obj(obj);
    let refuse = |what: &str| -> CallError {
        refusal(def, "Менеджер", &format!("Получить: {what}")).into()
    };
    if def.behavior.write_mode == Some(crate::model::InfoRegWriteMode::RecorderSubordinate) {
        return Err(refuse("регистр, подчинённый регистратору"));
    }
    if def.behavior.periodicity.unwrap_or_default() != crate::model::Periodicity::Nonperiodical {
        return Err(refuse("периодический регистр"));
    }
    let core = SetCore {
        app: app.clone(),
        obj,
        cols: None,
        modified: Cell::new(false),
    };
    let cols = core.cols()?;
    let dims: Vec<usize> = cols
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            core.field(c)
                .is_some_and(|f| f.std.is_none() && f.role == FieldRole::Dimension)
        })
        .map(|(i, _)| i)
        .collect();
    if dims.is_empty() {
        return Err(refuse("регистр без измерений"));
    }
    if !matches!(arg_value(ex, args, 1)?, Value::Undefined | Value::Number(_)) {
        return Err(refuse("второй параметр не число"));
    }
    let given = arg_value(ex, args, 0)?;
    let entries = match &given {
        Value::Undefined => return Err(crate::select::value_mismatch(1).into()),
        _ if !is_structure(&given) => return Err(crate::meta::type_mismatch(1).into()),
        _ => bsl_stdlib::structure_fields(&given).ok_or_else(|| crate::meta::type_mismatch(1))?,
    };
    let mut values: Vec<(usize, Value)> = Vec::with_capacity(dims.len());
    for (name, v) in &entries {
        let wanted = fold_name(name);
        let Some(col) = dims
            .iter()
            .copied()
            .find(|i| cols.get(*i).is_some_and(|c| fold_name(&c.name) == wanted))
        else {
            return Err(crate::select::value_mismatch(1).into());
        };
        let f = cols
            .get(col)
            .and_then(|c| core.field(c))
            .ok_or_else(|| internal("a dimension without a field"))?;
        let cast = crate::values::typedesc::cast_to_field(app, ex, f, v.clone())?;
        values.push((col, cast));
    }
    if dims.iter().any(|i| !values.iter().any(|(c, _)| c == i)) {
        return Err(crate::select::value_mismatch(1).into());
    }
    let core = Rc::new(core);
    let record = store::get_record(ex, &core, &values)?;
    let mut fields = Vec::new();
    for (i, col) in cols.iter().enumerate() {
        if !core
            .field(col)
            .is_some_and(|f| f.std.is_none() && f.role == FieldRole::Resource)
        {
            continue;
        }
        let v = match &record {
            Some(rec) => rec.value(i)?,
            None if matches!(col.types.single(), Some(TypeAtom::ValueStorage)) => {
                crate::values::field::empty_storage()
            }
            None => empty_value(app, ex, obj, col)?,
        };
        fields.push((col.name.as_str(), v));
    }
    let bytes = fields.iter().fold(256u64, |n, (k, v)| {
        n.saturating_add(k.len() as u64 * 2)
            .saturating_add(read_budget::value_heap(v))
            .saturating_add(64)
    });
    read_budget::reserve(ex, bytes)?;
    Ok(bsl_stdlib::new_structure(fields))
}

pub(crate) fn create_key(
    ex: &mut Execution,
    app: &Arc<Application>,
    obj: ObjId,
    args: &[Arg],
) -> CallResult<Value> {
    let ty = type_of(app, obj, Category::RecordKey)?;
    let def = app.model().obj(obj);
    let given = arg_value(ex, args, 0)?;
    let Some(entries) = bsl_stdlib::structure_fields(&given) else {
        return Err(refusal(def, "Менеджер", "СоздатьКлючЗаписи: значение не структура").into());
    };
    let mut fields = Vec::new();
    for col in columns(app, obj)?.iter().filter(|c| c.key) {
        let wanted = fold_name(&col.name);
        let value = match entries.iter().find(|(k, _)| fold_name(k) == wanted) {
            None => None,
            Some((_, Value::Undefined)) => Some(empty_value(app, ex, obj, col)?),
            Some((_, v)) if accepts_key_value(app, &col.types, v) => Some(v.clone()),
            Some(_) => {
                return Err(refusal(
                    def,
                    "Менеджер",
                    &format!("СоздатьКлючЗаписи: значение поля {}", col.name),
                )
                .into());
            }
        };
        fields.push(KeyField {
            name: col.name.clone(),
            value,
        });
    }
    let bytes = fields.iter().fold(
        read_budget::rc_size::<RecordKey>()
            .saturating_add(read_budget::slots::<KeyField>(fields.capacity())),
        |n, f| {
            n.saturating_add(f.name.capacity() as u64)
                .saturating_add(f.value.as_ref().map_or(0, read_budget::value_heap))
        },
    );
    read_budget::reserve(ex, bytes)?;
    Ok(Value::Object(Rc::new(RecordKey {
        app: app.clone(),
        obj,
        ty,
        fields,
    })))
}

pub(crate) struct SetCore {
    pub(crate) app: Arc<Application>,
    pub(crate) obj: ObjId,
    cols: Option<Rc<Vec<Column>>>,
    pub(crate) modified: Cell<bool>,
}

impl SetCore {
    pub(crate) fn def(&self) -> &ObjDef {
        self.app.model().obj(self.obj)
    }

    pub(crate) fn cols(&self) -> Result<Rc<Vec<Column>>, HostError> {
        match &self.cols {
            Some(c) => Ok(c.clone()),
            None => columns(&self.app, self.obj).map(Rc::new),
        }
    }

    pub(crate) fn refusal(&self, member: &str) -> HostError {
        refusal(self.def(), "НаборЗаписей", member)
    }

    pub(crate) fn field(&self, col: &Column) -> Option<&FieldDef> {
        match col.source {
            Source::Field(i) => self.def().fields.get(i),
            Source::Separator(_) => None,
        }
    }
}

pub(crate) fn off_bound() -> HostError {
    HostError::key(bsl_stdlib::bind::msg::RANGE)
}

pub(crate) fn create_set(
    ex: &mut Execution,
    app: &Arc<Application>,
    obj: ObjId,
) -> CallResult<Value> {
    let ty = type_of(app, obj, Category::RecordSet)?;
    read_budget::reserve(
        ex,
        read_budget::rc_size::<RecordSet>().saturating_add(read_budget::rc_size::<SetCore>()),
    )?;
    let core = Rc::new(SetCore {
        app: app.clone(),
        obj,
        cols: columns(app, obj).ok().map(Rc::new),
        modified: Cell::new(false),
    });
    let set = Rc::new_cyclic(|me| RecordSet {
        core,
        ty,
        me: me.clone(),
        state: RefCell::new(SetState::default()),
    });
    if let Some(m) = app
        .modules()
        .of_object(obj, md_model::ModuleKind::RecordSetModule)
    {
        let compiled = app
            .modules()
            .compiled_or_fatal(m, ex.lang())
            .map_err(CallError::Exc)?;
        let ctx: ObjRef = Rc::new(SetCtx {
            set: Rc::downgrade(&set),
            ty,
        });
        let mi = ex
            .instantiate_weighted(
                &compiled,
                ModuleContext::Object(ctx),
                crate::objects::BODY_WEIGHT,
            )
            .map_err(CallError::Exc)?;
        set.state.borrow_mut().module = Some(mi);
    }
    Ok(Value::Object(set))
}

#[derive(Default)]
struct SetState {
    records: Vec<Rc<Record>>,
    filter: Option<Rc<Filter>>,
    extra: Option<Value>,
    exchange: Option<Rc<ExchangeParams>>,
    module: Option<ModuleInstanceRef>,
}

pub(crate) struct RecordSet {
    pub(crate) core: Rc<SetCore>,
    ty: TypeId,
    me: Weak<RecordSet>,
    state: RefCell<SetState>,
}

impl RecordSet {
    fn refusal(&self, member: &str) -> HostError {
        self.core.refusal(member)
    }

    pub(crate) fn rc(&self) -> Result<Rc<RecordSet>, HostError> {
        self.me
            .upgrade()
            .ok_or_else(|| internal("the record set is gone"))
    }

    pub(crate) fn records(&self) -> Vec<Rc<Record>> {
        self.state.borrow().records.clone()
    }

    pub(crate) fn replace_records(&self, records: Vec<Rc<Record>>) {
        self.state.borrow_mut().records = records;
    }

    pub(crate) fn module(&self) -> Option<ModuleInstanceRef> {
        self.state.borrow().module.clone()
    }

    pub(crate) fn filter(&self, ex: &mut Execution) -> CallResult<Rc<Filter>> {
        if let Some(f) = self.state.borrow().filter.clone() {
            return Ok(f);
        }
        let f = Filter::new(ex, &self.core)?;
        self.state.borrow_mut().filter = Some(f.clone());
        Ok(f)
    }

    fn extra(&self, ex: &mut Execution) -> CallResult<Value> {
        if let Some(v) = self.state.borrow().extra.clone() {
            return Ok(v);
        }
        read_budget::reserve(ex, 512)?;
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

    fn record_at(&self, i: &Value) -> CallResult<Value> {
        let Value::Number(n) = i else {
            return Err(crate::meta::type_mismatch(1).into());
        };
        let i = usize::try_from(n.trunc_to_i64_saturating()).map_err(|_| off_bound())?;
        let st = self.state.borrow();
        let r = st.records.get(i).ok_or_else(off_bound)?;
        let r: ObjRef = r.clone();
        Ok(Value::Object(r))
    }

    fn add(&self, ex: &mut Execution) -> CallResult<Value> {
        let cols = self.core.cols()?;
        let rec = Record::blank(ex, &self.core, cols)?;
        self.state.borrow_mut().records.push(rec.clone());
        self.core.modified.set(true);
        let rec: ObjRef = rec;
        Ok(Value::Object(rec))
    }

    fn delete(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let at = match arg_value(ex, args, 0)? {
            Value::Number(n) => {
                let i = usize::try_from(n.trunc_to_i64_saturating()).map_err(|_| off_bound())?;
                if i >= self.state.borrow().records.len() {
                    return Err(off_bound().into());
                }
                i
            }
            Value::Object(o) => {
                let Some(r) = o.as_any().downcast_ref::<Record>() else {
                    return Err(self.refusal("Удалить: не запись и не индекс").into());
                };
                self.state
                    .borrow()
                    .records
                    .iter()
                    .position(|x| std::ptr::eq(&**x, r))
                    .ok_or_else(|| self.refusal("Удалить: запись другого набора"))?
            }
            _ => return Err(self.refusal("Удалить: не запись и не индекс").into()),
        };
        self.state.borrow_mut().records.remove(at);
        self.core.modified.set(true);
        Ok(Value::Undefined)
    }
}

impl HostObject for RecordSet {
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        visit.work(1)?;
        visit.mutable();
        let st = self.state.borrow();
        visit.bytes(read_budget::slots::<Rc<Record>>(st.records.capacity()))?;
        for r in &st.records {
            let r: ObjRef = r.clone();
            visit.value(&Value::Object(r))?;
        }
        if let Some(extra) = &st.extra {
            visit.value(extra)?;
        }
        if let Some(exchange) = &st.exchange {
            let exchange: ObjRef = exchange.clone();
            visit.value(&Value::Object(exchange))?;
        }
        if let Some(filter) = &st.filter {
            let filter: ObjRef = filter.clone();
            visit.value(&Value::Object(filter))?;
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
        match member(ex, self.ty, m) {
            Some((false, "ДополнительныеСвойства", _)) => self.extra(ex),
            Some((false, "ОбменДанными", _)) => {
                let e: ObjRef = self.exchange()?;
                Ok(Value::Object(e))
            }
            Some((false, "Отбор", _)) => {
                let f: ObjRef = self.filter(ex)?;
                Ok(Value::Object(f))
            }
            Some((false, "ЭтотОбъект", _)) => {
                let me: ObjRef = self.rc()?;
                Ok(Value::Object(me))
            }
            _ => Err(self.refusal(&member_ru(ex, self.ty, m)).into()),
        }
    }

    fn set(&self, ex: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(self.refusal(&member_ru(ex, self.ty, m)).into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match member(ex, self.ty, m) {
            Some((true, "Количество", _)) => Ok(Value::from(
                i64::try_from(self.state.borrow().records.len()).unwrap_or(i64::MAX),
            )),
            Some((true, "Метаданные", _)) => {
                let cx = self
                    .core
                    .app
                    .md_ctx()
                    .ok_or_else(|| internal("no Метаданные context"))?;
                Ok(value_of(
                    &cx,
                    ex,
                    MdKey::Path(MdPath::Object(self.core.obj)),
                ))
            }
            Some((true, "Выгрузить", _)) => store::unload(ex, self, args),
            Some((true, "ВыгрузитьКолонки", _)) => store::unload_columns(ex, self),
            Some((true, "Добавить", _)) => self.add(ex),
            Some((true, "Удалить", _)) => self.delete(ex, args),
            Some((true, "Получить", _)) => {
                let i = arg_value(ex, args, 0)?;
                self.record_at(&i)
            }
            Some((true, "Очистить", _)) => {
                self.state.borrow_mut().records.clear();
                self.core.modified.set(true);
                Ok(Value::Undefined)
            }
            Some((true, "Модифицированность", _)) => {
                Ok(Value::Bool(self.core.modified.get()))
            }
            Some((true, "Прочитать", _)) => store::read(ex, self),
            Some((true, "Записать", _)) => store::write(ex, &self.rc()?, args),
            Some((true, "Загрузить", _)) => store::load(ex, self, args),
            Some((_, ru, _)) => Err(self.refusal(ru).into()),
            None => Err(self.refusal(&m.0.to_string()).into()),
        }
    }

    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.state.borrow().records.len())
    }

    fn iter_get(&self, _: &mut Execution, i: usize) -> CallResult<Value> {
        let st = self.state.borrow();
        let r = st.records.get(i).ok_or_else(off_bound)?;
        let r: ObjRef = r.clone();
        Ok(Value::Object(r))
    }

    fn index_get(&self, _: &mut Execution, key: &Value) -> CallResult<Value> {
        self.record_at(key)
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let any = other.as_any();
        if let Some(o) = any.downcast_ref::<RecordSet>() {
            return Some(std::ptr::eq(self, o));
        }
        Some(
            any.downcast_ref::<SetCtx>()
                .is_some_and(|c| std::ptr::eq(self, c.set.as_ptr())),
        )
    }
}

struct SetCtx {
    set: Weak<RecordSet>,
    ty: TypeId,
}

impl SetCtx {
    fn target(&self) -> Result<Rc<RecordSet>, HostError> {
        self.set
            .upgrade()
            .ok_or_else(|| internal("the record set is gone"))
    }
}

impl HostObject for SetCtx {
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

    fn iter_len(&self, ex: &mut Execution) -> Option<usize> {
        self.set.upgrade()?.iter_len(ex)
    }

    fn iter_get(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        self.target()?.iter_get(ex, i)
    }

    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        self.target()?.index_get(ex, key)
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let set = self.set.upgrade()?;
        let any = other.as_any();
        Some(
            any.downcast_ref::<RecordSet>()
                .is_some_and(|o| std::ptr::eq(o, &*set))
                || any
                    .downcast_ref::<SetCtx>()
                    .is_some_and(|o| o.set.as_ptr() == self.set.as_ptr()),
        )
    }
}

struct KeyField {
    name: String,
    value: Option<Value>,
}

pub(crate) struct RecordKey {
    app: Arc<Application>,
    obj: ObjId,
    ty: TypeId,
    fields: Vec<KeyField>,
}

impl RecordKey {
    fn refusal(&self, member: &str) -> HostError {
        refusal(self.app.model().obj(self.obj), "КлючЗаписи", member)
    }

    fn field(&self, ex: &Execution, m: MemberId) -> Result<&KeyField, HostError> {
        if m.0 >= FIELD_BASE {
            return usize::try_from(m.0 - FIELD_BASE)
                .ok()
                .and_then(|i| self.fields.get(i))
                .ok_or_else(|| internal("unknown field of a record key"));
        }
        let Some((false, ru, en)) = member(ex, self.ty, m) else {
            return Err(internal("unknown member of a record key"));
        };
        let (ru_folded, en_folded) = (fold_name(ru), fold_name(en));
        self.fields
            .iter()
            .find(|f| {
                let name = fold_name(&f.name);
                name == ru_folded || name == en_folded
            })
            .ok_or_else(|| no_field(ru))
    }
}

impl HostObject for RecordKey {
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        visit.bytes(read_budget::slots::<KeyField>(self.fields.capacity()))?;
        for f in &self.fields {
            visit.bytes(f.name.capacity() as u64)?;
            if let Some(v) = &f.value {
                visit.value(v)?;
            }
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
        let f = self.field(ex, m)?;
        match &f.value {
            Some(v) => Ok(v.clone()),
            None => Err(self.refusal(&f.name).into()),
        }
    }

    fn set(&self, ex: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        let name = match self.field(ex, m) {
            Ok(f) => f.name.clone(),
            Err(_) => m.0.to_string(),
        };
        Err(HostError::key(bsl_stdlib::bind::msg::PROP_NOT_WRITABLE)
            .with_arg(name)
            .raw()
            .into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(self.refusal(&member_ru(ex, self.ty, m)).into())
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        let wanted = fold_name(name);
        let i = self
            .fields
            .iter()
            .position(|f| fold_name(&f.name) == wanted)?;
        FIELD_BASE.checked_add(u32::try_from(i).ok()?).map(MemberId)
    }

    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        let i = usize::try_from(m.0.checked_sub(FIELD_BASE)?).ok()?;
        (i < self.fields.len()).then_some(MemberKind::Property {
            readable: true,
            writable: false,
        })
    }
}
