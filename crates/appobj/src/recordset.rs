use crate::meta::{MdKey, MdPath, arg_value, member_name, no_field, value_of};
use crate::model::{FieldDef, FieldRole, ObjDef, ObjId, StdAttr, TypeAtom, TypeSet};
use crate::session::internal;
use crate::types::Category;
use crate::values::refs::as_ref;
use crate::{Application, read_budget};
use bsl_prim::fold_name;
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, MemberKind,
    RetainedMemory, TypeId, Value,
};
use md_model::{MdKind, MdObject};
use platform_catalog::messages::Lang;
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

const FIELD_BASE: u32 = 1 << 20;

const STANDARD: [(StdAttr, bool); 4] = [
    (StdAttr::Period, true),
    (StdAttr::Recorder, true),
    (StdAttr::LineNumber, false),
    (StdAttr::Active, false),
];

#[derive(Clone, Copy)]
enum Source {
    Field(usize),
    Separator(usize),
}

struct Column {
    name: String,
    source: Source,
    std: Option<StdAttr>,
    types: TypeSet,
    key: bool,
}

fn refusal(def: &ObjDef, suffix: &str, member: &str) -> HostError {
    HostError::not_implemented(&format!("{}{suffix}", def.kind.name(Lang::Ru)), member)
}

fn applies(ca: &MdObject, full: &str) -> bool {
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

fn text<'a>(o: &'a MdObject, name: &str) -> Option<&'a str> {
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

fn accepts_key_value(app: &Application, types: &TypeSet, v: &Value) -> bool {
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

pub(crate) fn create_set(
    ex: &mut Execution,
    app: &Arc<Application>,
    obj: ObjId,
) -> CallResult<Value> {
    let ty = type_of(app, obj, Category::RecordSet)?;
    read_budget::reserve(ex, read_budget::rc_size::<RecordSet>())?;
    Ok(Value::Object(Rc::new(RecordSet {
        app: app.clone(),
        obj,
        ty,
    })))
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

pub(crate) struct RecordSet {
    app: Arc<Application>,
    obj: ObjId,
    ty: TypeId,
}

impl RecordSet {
    fn refusal(&self, member: &str) -> HostError {
        refusal(self.app.model().obj(self.obj), "НаборЗаписей", member)
    }

    fn unload(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        if args.iter().any(|a| !matches!(a, Arg::Omitted)) {
            return Err(self.refusal("Выгрузить: параметры").into());
        }
        let cols = columns(&self.app, self.obj)?;
        let table = ex.construct_by_name("ТаблицаЗначений", &mut [])?;
        let list = ex.get_member(&table, "Колонки")?;
        for col in &cols {
            ex.charge(1)?;
            let ty = column_type(&self.app, ex, self.obj, col)?;
            let mut add = [Arg::Val(Value::from(col.name.as_str())), Arg::Val(ty)];
            ex.call_method(&list, "Добавить", &mut add, true)?;
        }
        Ok(table)
    }
}

impl HostObject for RecordSet {
    fn retained_memory(&self, _: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        Ok(true)
    }

    fn type_id(&self) -> TypeId {
        self.ty
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(self.refusal(&member_ru(ex, self.ty, m)).into())
    }

    fn set(&self, ex: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(self.refusal(&member_ru(ex, self.ty, m)).into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match member(ex, self.ty, m) {
            Some((true, "Количество", _)) => Ok(Value::from(0i64)),
            Some((true, "Метаданные", _)) => {
                let cx = self
                    .app
                    .md_ctx()
                    .ok_or_else(|| internal("no Метаданные context"))?;
                Ok(value_of(&cx, ex, MdKey::Path(MdPath::Object(self.obj))))
            }
            Some((true, "Выгрузить", _)) => self.unload(ex, args),
            Some((_, ru, _)) => Err(self.refusal(ru).into()),
            None => Err(self.refusal(&m.0.to_string()).into()),
        }
    }

    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(0)
    }

    fn index_get(&self, _: &mut Execution, _: &Value) -> CallResult<Value> {
        Err(self.refusal("[индекс]").into())
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
