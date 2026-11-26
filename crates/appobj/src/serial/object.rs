use crate::Application;
use crate::model::{CodeType, FieldRole, ObjDef, ObjKind, StdAttr, TypeAtom, TypeSet, Usage};
use crate::objects::ObjectInstance;
use crate::types::Category;
use crate::values::refs::{empty_ref, ref_value};
use bsl_vm::host::{AppXdtoPropDef, AppXdtoTypeDef};
use bsl_vm::{
    AppXdtoProp, AppXdtoValue, CallError, Exception, Execution, HostError, ObjRef, Value,
};
use platform_catalog::XdtoName;
use std::collections::HashSet;

const NS_CONFIG: &str = "http://v8.1c.ru/8.1/data/enterprise/current-config";
const NS_XS: &str = "http://www.w3.org/2001/XMLSchema";
const NS_CORE: &str = "http://v8.1c.ru/8.1/data/core";
const NS_ENTERPRISE: &str = "http://v8.1c.ru/8.1/data/enterprise";
const ROWS_UPPER: i64 = 999_999_999;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Writer {
    Xdto,
    Old,
}

fn refused(def: &ObjDef, what: &str) -> HostError {
    HostError::not_implemented(
        &format!("СправочникОбъект.{}", def.name),
        &format!("XML: {what}"),
    )
}

fn xs(name: &str) -> XdtoName {
    XdtoName {
        namespace: NS_XS.into(),
        name: name.into(),
    }
}

fn core(name: &str) -> XdtoName {
    XdtoName {
        namespace: NS_CORE.into(),
        name: name.into(),
    }
}

fn attr(ex: &mut Execution, o: &ObjectInstance, i: usize) -> Result<Value, Exception> {
    o.attr(ex, i).map_err(|e| match e {
        CallError::Host(h) => ex.error(h),
        CallError::Exc(x) => x,
    })
}

pub(super) fn catalog_object<'a>(app: &Application, v: &'a ObjRef) -> Option<&'a ObjectInstance> {
    let o = v.as_any().downcast_ref::<ObjectInstance>()?;
    (std::ptr::eq(&*o.app, app) && app.model().obj(o.obj).kind == ObjKind::Catalog).then_some(o)
}

fn own_objects(
    app: &Application,
    def: &ObjDef,
) -> Result<HashSet<crate::model::Uuid16>, HostError> {
    if let Some(why) = app.extension_model().refusal() {
        return Err(refused(def, &why));
    }
    let mut own = HashSet::new();
    if *app.extension_model() == crate::ExtensionModel::None {
        return Ok(own);
    }
    let facts = &app.options().extensions;
    if facts.is_empty() {
        return Err(refused(
            def,
            "в модели есть расширения, а их факты не переданы",
        ));
    }
    for e in facts {
        match &e.structure {
            Ok(s) => own.extend(s.own.iter().copied()),
            Err(why) => {
                return Err(refused(
                    def,
                    &format!("состав расширения «{}» не прочитан: {why}", e.name),
                ));
            }
        }
    }
    Ok(own)
}

fn check_kind(app: &Application, def: &ObjDef, writer: Writer) -> Result<(), HostError> {
    let own = own_objects(app, def)?;
    let b = &def.behavior;
    if !b.owners.is_empty() {
        return Err(refused(def, "подчинённый справочник"));
    }
    if b.hierarchical && !b.hierarchy_folders {
        return Err(refused(def, "иерархия элементов"));
    }
    if b.code_length == 0 || b.description_length == 0 {
        return Err(refused(def, "справочник без кода или наименования"));
    }
    for f in &def.fields {
        match f.role {
            FieldRole::Standard => {}
            FieldRole::Attribute if f.usage == Usage::ForItem => {}
            FieldRole::Attribute => {
                return Err(refused(
                    def,
                    &format!("реквизит «{}» не для элемента", f.name),
                ));
            }
            _ => return Err(refused(def, &format!("поле «{}»", f.name))),
        }
    }
    for t in &def.tabular {
        if own.contains(&t.md) {
            return Err(refused(
                def,
                &format!("табличная часть расширения «{}»", t.name),
            ));
        }
        if let Some(f) = t
            .fields
            .iter()
            .find(|f| f.md.is_some_and(|m| own.contains(&m)))
        {
            return Err(refused(
                def,
                &format!("реквизит расширения «{}» в табличной части", f.name),
            ));
        }
    }
    if let Some(f) = def
        .fields
        .iter()
        .find(|f| f.md.is_some_and(|m| own.contains(&m)))
    {
        if !b.hierarchical {
            return Err(refused(
                def,
                &format!("реквизит расширения «{}» у плоского справочника", f.name),
            ));
        }
        if !def.tabular.is_empty() {
            return Err(refused(
                def,
                &format!(
                    "реквизит расширения «{}» у справочника с табличной частью",
                    f.name
                ),
            ));
        }
    }
    for f in def.fields.iter().filter(|f| f.std.is_none()) {
        property_type(app, def, &f.name, &f.types)?;
    }
    if writer == Writer::Old && b.code_type == CodeType::Number {
        return Err(refused(def, "числовой код"));
    }
    if writer == Writer::Old && b.hierarchical && !def.tabular.is_empty() {
        return Err(refused(def, "табличная часть иерархического справочника"));
    }
    Ok(())
}

fn std_order(def: &ObjDef) -> Vec<StdAttr> {
    let mut v = Vec::with_capacity(7);
    if def.behavior.hierarchical {
        v.push(StdAttr::IsFolder);
    }
    v.push(StdAttr::Ref);
    v.push(StdAttr::DeletionMark);
    if def.behavior.hierarchical {
        v.push(StdAttr::Parent);
    }
    v.extend([
        StdAttr::Code,
        StdAttr::Description,
        StdAttr::PredefinedDataName,
    ]);
    v
}

fn std_name(a: StdAttr) -> &'static str {
    a.names().1
}

fn field_of(def: &ObjDef, a: StdAttr) -> Option<usize> {
    def.fields.iter().position(|f| f.std == Some(a))
}

fn composite(t: &TypeSet) -> bool {
    !matches!(t.atoms.as_slice(), [a] if !matches!(a, TypeAtom::AnyRef(_)))
}

fn prop(name: &str, value: AppXdtoValue) -> AppXdtoProp {
    AppXdtoProp {
        name: name.into(),
        ns: None,
        value,
    }
}

fn plain(value: Value) -> AppXdtoValue {
    AppXdtoValue::Value {
        value,
        typed: false,
    }
}

fn attribute_value(
    def: &ObjDef,
    name: &str,
    t: &TypeSet,
    value: Value,
    writer: Writer,
) -> Result<AppXdtoValue, HostError> {
    if !composite(t) {
        return Ok(plain(value));
    }
    match (value, writer) {
        (Value::Undefined, _) => Ok(AppXdtoValue::Nil),
        (
            value @ (Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Date(_)),
            Writer::Xdto,
        ) => Ok(AppXdtoValue::Value { value, typed: true }),
        (_, Writer::Xdto) => Err(refused(
            def,
            &format!("составной реквизит «{name}» со значением не простого типа"),
        )),
        (_, Writer::Old) => Err(refused(
            def,
            &format!("непустой составной реквизит «{name}»"),
        )),
    }
}

pub(super) fn object_props(
    ex: &mut Execution,
    app: &Application,
    o: &ObjectInstance,
    writer: Writer,
) -> Result<Result<Vec<AppXdtoProp>, HostError>, Exception> {
    let def = app.model().obj(o.obj);
    macro_rules! refuse {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(e) => return Ok(Err(e)),
            }
        };
    }
    refuse!(check_kind(app, def, writer));
    refuse!(
        o.live()
            .map_err(|_| refused(def, "удалённый объект или отменённая запись нового"))
    );
    let (r, new_ref) = {
        let st = o.state.borrow();
        (st.r, st.new_ref)
    };
    let folder = o.folder_flag().unwrap_or(false);
    if let Some(i) = field_of(def, StdAttr::PredefinedDataName) {
        match attr(ex, o, i)? {
            Value::String(s) if s.is_empty() => {}
            _ => return Ok(Err(refused(def, "предопределённый элемент"))),
        }
    }
    let mut out = Vec::with_capacity(def.fields.len() + def.tabular.len());
    for a in std_order(def) {
        let value = match a {
            StdAttr::PredefinedDataName => continue,
            StdAttr::Ref => {
                let value = match (r, new_ref) {
                    (Some(r), _) => ref_value(app, r),
                    (None, Some(_)) if writer == Writer::Old => {
                        return Ok(Err(refused(def, "ссылка нового объекта")));
                    }
                    (None, Some(n)) => ref_value(app, n),
                    (None, None) => empty_ref(app, o.obj),
                };
                value.map_err(|e| ex.error(e))?
            }
            _ => {
                let i = refuse!(
                    field_of(def, a)
                        .ok_or_else(|| refused(def, &format!("нет поля {}", std_name(a))))
                );
                attr(ex, o, i)?
            }
        };
        let p = prop(std_name(a), plain(value));
        if writer == Writer::Old && a == StdAttr::Ref {
            out.insert(0, p);
        } else {
            out.push(p);
        }
    }
    for (i, f) in def.fields.iter().enumerate() {
        if f.std.is_some() || o.hidden(i) {
            continue;
        }
        let value = attr(ex, o, i)?;
        let value = refuse!(attribute_value(def, &f.name, &f.types, value, writer));
        out.push(prop(&f.name, value));
    }
    let tabs = o.state.borrow().tabular.clone();
    for (t, tab) in def.tabular.iter().zip(tabs) {
        match writer {
            Writer::Old if tab.is_empty() => {
                out.push(prop(&t.name, AppXdtoValue::Object(vec![])));
            }
            Writer::Old => {
                return Ok(Err(refused(
                    def,
                    &format!("непустая табличная часть «{}»", t.name),
                )));
            }
            Writer::Xdto if folder && !tab.is_empty() => {
                return Ok(Err(refused(
                    def,
                    &format!("табличная часть группы «{}»", t.name),
                )));
            }
            Writer::Xdto => {
                let lines = tab.lines();
                ex.charge((lines.len() as u64).saturating_mul(t.fields.len() as u64 + 1))?;
                for line in lines {
                    let mut row = Vec::with_capacity(t.fields.len());
                    for (f, value) in t.fields.iter().zip(line) {
                        if f.std == Some(StdAttr::LineNumber) {
                            continue;
                        }
                        let value = refuse!(attribute_value(def, &f.name, &f.types, value, writer));
                        row.push(prop(&f.name, value));
                    }
                    out.push(prop(&t.name, AppXdtoValue::Object(row)));
                }
            }
        }
    }
    Ok(Ok(out))
}

fn property_type(
    app: &Application,
    def: &ObjDef,
    name: &str,
    t: &TypeSet,
) -> Result<(XdtoName, bool, Option<AppXdtoTypeDef>), HostError> {
    if composite(t) {
        return Ok((xs("anyType"), true, None));
    }
    let Some(atom) = t.atoms.first() else {
        return Ok((xs("anyType"), true, None));
    };
    let ty = match atom {
        TypeAtom::Bool => xs("boolean"),
        TypeAtom::Number => xs("decimal"),
        TypeAtom::String => xs("string"),
        TypeAtom::Date => xs("dateTime"),
        TypeAtom::Binary => xs("base64Binary"),
        TypeAtom::Uuid => core("UUID"),
        TypeAtom::ValueStorage => core("ValueStorage"),
        TypeAtom::Ref(obj) => {
            let name = reference_type(app, *obj)
                .ok_or_else(|| refused(def, &format!("тип реквизита «{name}»")))?;
            let base = if app.model().obj(*obj).kind == ObjKind::Enum {
                "AnyMDEnum"
            } else {
                "AnyDBRef"
            };
            let value = AppXdtoTypeDef::Value {
                name: name.clone(),
                base: XdtoName {
                    namespace: NS_ENTERPRISE.into(),
                    name: base.into(),
                },
            };
            return Ok((name, false, Some(value)));
        }
        _ => return Err(refused(def, &format!("тип реквизита «{name}»"))),
    };
    Ok((ty, false, None))
}

fn reference_type(app: &Application, obj: crate::model::ObjId) -> Option<XdtoName> {
    let types = app.types()?;
    types.xdto_name(types.type_of(obj, Category::Ref)?)
}

fn object_type(app: &Application, obj: crate::model::ObjId) -> Option<XdtoName> {
    let types = app.types()?;
    types.xdto_name(types.type_of(obj, Category::Object)?)
}

pub(super) fn object_types(
    ex: &mut Execution,
    app: &Application,
    o: &ObjectInstance,
    root: &XdtoName,
) -> Result<Option<Vec<AppXdtoTypeDef>>, Exception> {
    if object_type(app, o.obj).as_ref() != Some(root) || root.namespace != NS_CONFIG {
        return Ok(None);
    }
    if let Err(e) = object_props(ex, app, o, Writer::Xdto)? {
        return Err(ex.error(e));
    }
    let def = app.model().obj(o.obj);
    let b = &def.behavior;
    let lower = u32::from(!b.hierarchical);
    let mut values: Vec<AppXdtoTypeDef> = Vec::new();
    let mut add_value = |v: Option<AppXdtoTypeDef>| {
        if let Some(AppXdtoTypeDef::Value { name, base }) = v
            && !values
                .iter()
                .any(|t| matches!(t, AppXdtoTypeDef::Value { name: n, .. } if *n == name))
        {
            values.push(AppXdtoTypeDef::Value { name, base });
        }
    };
    let own_ref =
        reference_type(app, o.obj).ok_or_else(|| ex.error(refused(def, "нет типа ссылки")))?;
    let mut props = Vec::with_capacity(def.fields.len() + def.tabular.len());
    for a in std_order(def) {
        let (ty, lower) = match a {
            StdAttr::IsFolder | StdAttr::DeletionMark => (xs("boolean"), 1),
            StdAttr::Ref | StdAttr::Parent => (own_ref.clone(), 1),
            StdAttr::Code if b.code_type == CodeType::Number => (xs("decimal"), 1),
            StdAttr::Code | StdAttr::Description => (xs("string"), 1),
            _ => (xs("string"), 0),
        };
        props.push(AppXdtoPropDef {
            name: std_name(a).into(),
            ty,
            lower,
            upper: 1,
            nillable: false,
        });
    }
    add_value(Some(AppXdtoTypeDef::Value {
        name: own_ref.clone(),
        base: XdtoName {
            namespace: NS_ENTERPRISE.into(),
            name: "AnyDBRef".into(),
        },
    }));
    for f in def.fields.iter().filter(|f| f.std.is_none()) {
        let (ty, nillable, value) =
            property_type(app, def, &f.name, &f.types).map_err(|e| ex.error(e))?;
        add_value(value);
        props.push(AppXdtoPropDef {
            name: f.name.clone(),
            ty,
            lower,
            upper: 1,
            nillable,
        });
    }
    let mut rows = Vec::with_capacity(def.tabular.len());
    for t in &def.tabular {
        let row = XdtoName {
            namespace: NS_CONFIG.into(),
            name: format!("CatalogTabularSectionRow.{}.{}", def.name, t.name),
        };
        let mut row_props = Vec::with_capacity(t.fields.len());
        for f in t
            .fields
            .iter()
            .filter(|f| f.std != Some(StdAttr::LineNumber))
        {
            let (ty, nillable, value) =
                property_type(app, def, &f.name, &f.types).map_err(|e| ex.error(e))?;
            add_value(value);
            row_props.push(AppXdtoPropDef {
                name: f.name.clone(),
                ty,
                lower: 1,
                upper: 1,
                nillable,
            });
        }
        props.push(AppXdtoPropDef {
            name: t.name.clone(),
            ty: row.clone(),
            lower: 0,
            upper: ROWS_UPPER,
            nillable: false,
        });
        rows.push(AppXdtoTypeDef::Object {
            name: row,
            properties: row_props,
        });
    }
    let mut out = Vec::with_capacity(1 + rows.len() + values.len());
    out.push(AppXdtoTypeDef::Object {
        name: root.clone(),
        properties: props,
    });
    out.extend(rows);
    out.extend(values);
    Ok(Some(out))
}
