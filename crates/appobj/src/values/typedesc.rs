use crate::Application;
use crate::model::{ObjKind, TypeAtom, TypeSet};
use crate::types::Category;
use bsl_stdlib::typedesc::{
    AllowedLength, AllowedSign, BinaryQualifiers, DateParts, DateQualifiers, NumberQualifiers,
    StringQualifiers, TypeDescription, new_type_description,
};
use bsl_vm::{CallError, CallResult, Execution, HostError, TypeId, Value};

fn unsupported() -> HostError {
    crate::meta::not_implemented("Тип")
}

pub(crate) fn from_metadata(
    app: &Application,
    ex: &mut Execution,
    td: &md_model::TypeDescription,
) -> CallResult<Value> {
    let model = app.model();
    let lookup = |id| {
        model.objects.iter().find_map(|o| {
            o.generated
                .iter()
                .any(|g| g.category == "Ref" && g.type_id == id)
                .then_some(o.id)
        })
    };
    let mut diag = Vec::new();
    let set = TypeSet::of_diag(&model.config, td, &lookup, &mut diag);
    if !diag.is_empty() || (!td.types.is_empty() && set.atoms.is_empty()) {
        return Err(unsupported().into());
    }
    from_set(app, ex, &set)
}

pub(crate) fn from_standard(
    app: &Application,
    ex: &mut Execution,
    set: &TypeSet,
) -> CallResult<Value> {
    if set.atoms.is_empty()
        || set.atoms.iter().any(|a| {
            matches!(a, TypeAtom::AnyRef(_))
                || matches!(a, TypeAtom::Platform(n) if n == "BusinessProcessRoutePointRef")
        })
    {
        return Err(unsupported().into());
    }
    from_set(app, ex, set)
}

pub(crate) fn all_refs(app: &Application, ex: &mut Execution, kind: ObjKind) -> CallResult<Value> {
    let set = TypeSet {
        atoms: app
            .model()
            .of_kind(kind)
            .map(|o| TypeAtom::Ref(o.id))
            .collect(),
        ..TypeSet::default()
    };
    from_set(app, ex, &set)
}

pub(crate) fn all_route_points(app: &Application, ex: &mut Execution) -> CallResult<Value> {
    let refs = app.types().ok_or_else(unsupported)?;
    let ids = app
        .model()
        .of_kind(ObjKind::BusinessProcess)
        .map(|o| {
            refs.type_of(o.id, Category::RoutePointRef)
                .ok_or_else(unsupported)
        })
        .collect::<Result<Vec<_>, _>>()?;
    from_ids(ex, &ids, &TypeSet::default())
}

fn platform_type(ex: &Execution, name: &str) -> Option<TypeId> {
    let name = name.strip_prefix("v8:").unwrap_or(name);
    ex.runtime().types().by_name(name).or_else(|| {
        platform_catalog::catalog()
            .types()
            .iter()
            .position(|t| t.id == name)
            .and_then(|i| u16::try_from(i).ok())
            .map(|i| TypeId::of_catalog(platform_catalog::TypeIdx(i)))
    })
}

fn from_set(app: &Application, ex: &mut Execution, set: &TypeSet) -> CallResult<Value> {
    Ok(new_type_description(description_of(app, ex, set)?))
}

pub(crate) fn cast_to_field(
    app: &Application,
    ex: &mut Execution,
    f: &crate::model::FieldDef,
    value: Value,
) -> CallResult<Value> {
    let refuse =
        |what: &str| HostError::not_implemented("Реквизит", &format!("{}: {what}", f.name)).into();
    if f.types
        .atoms
        .iter()
        .any(|a| matches!(a, TypeAtom::ValueStorage | TypeAtom::Binary))
    {
        return Err(refuse("значение типа ХранилищеЗначения или ДвоичныеДанные"));
    }
    let td = description_of(app, ex, &f.types).map_err(|e| match e {
        CallError::Host(_) => refuse("приведение к типам реквизита"),
        other => other,
    })?;
    bsl_stdlib::cast_value(ex, &td, value).map_err(CallError::Exc)
}

pub(crate) fn description_of(
    app: &Application,
    ex: &mut Execution,
    set: &TypeSet,
) -> CallResult<TypeDescription> {
    let refs = app.types().ok_or_else(unsupported)?;
    let mut ids = Vec::new();
    for atom in &set.atoms {
        let id = match atom {
            TypeAtom::Bool => TypeId::BOOL,
            TypeAtom::Number => TypeId::NUMBER,
            TypeAtom::String => TypeId::STRING,
            TypeAtom::Date => TypeId::DATE,
            TypeAtom::Null => TypeId::NULL,
            TypeAtom::Undefined => TypeId::UNDEFINED,
            TypeAtom::Uuid => platform_type(ex, "UUID").ok_or_else(unsupported)?,
            TypeAtom::ValueStorage => platform_type(ex, "ValueStorage").ok_or_else(unsupported)?,
            TypeAtom::Binary => platform_type(ex, "BinaryData").ok_or_else(unsupported)?,
            TypeAtom::Platform(n) => platform_type(ex, n).ok_or_else(unsupported)?,
            TypeAtom::Ref(obj) => refs.type_of(*obj, Category::Ref).ok_or_else(unsupported)?,
            TypeAtom::AnyRef(kind) => {
                if kind.is_some_and(|k| !k.is_reference()) {
                    return Err(unsupported().into());
                }
                for obj in &app.model().objects {
                    if obj.kind.is_reference() && kind.is_none_or(|k| k == obj.kind) {
                        ids.push(
                            refs.type_of(obj.id, Category::Ref)
                                .ok_or_else(unsupported)?,
                        );
                    }
                    if kind.is_none()
                        && obj.kind == ObjKind::BusinessProcess
                        && let Some(t) = refs.type_of(obj.id, Category::RoutePointRef)
                    {
                        ids.push(t);
                    }
                }
                continue;
            }
        };
        ids.push(id);
    }
    if ids.is_empty() && !set.atoms.is_empty() {
        return Err(unsupported().into());
    }
    build_ids(ex, &ids, set)
}

fn from_ids(ex: &mut Execution, ids: &[TypeId], set: &TypeSet) -> CallResult<Value> {
    Ok(new_type_description(build_ids(ex, ids, set)?))
}

fn build_ids(ex: &mut Execution, ids: &[TypeId], set: &TypeSet) -> CallResult<TypeDescription> {
    ex.reserve((ids.len() as u64).saturating_mul(32).saturating_add(256))?;
    let number = set.number.map(|q| NumberQualifiers {
        digits: q.digits,
        fraction: q.fraction,
        sign: if q.nonnegative {
            AllowedSign::Nonnegative
        } else {
            AllowedSign::Any
        },
    });
    let length = |fixed| {
        if fixed {
            AllowedLength::Fixed
        } else {
            AllowedLength::Variable
        }
    };
    let string = set.string.map(|q| StringQualifiers {
        length: q.length,
        allowed: length(q.fixed),
    });
    let date = set.date.map(|q| DateQualifiers {
        parts: match q {
            md_model::DateFractions::Date => DateParts::Date,
            md_model::DateFractions::Time => DateParts::Time,
            md_model::DateFractions::DateTime => DateParts::DateTime,
        },
    });
    let binary = set.binary.map(|q| BinaryQualifiers {
        length: q.length,
        allowed: length(q.fixed),
    });
    let td = TypeDescription::new(ids, number, string, date, binary, ex.runtime());
    let td = if td.types().len() > 1 || ids.contains(&TypeId::UNDEFINED) {
        TypeDescription::from_type_array(ids, number, string, date, binary, ex.runtime())
    } else {
        td
    };
    Ok(td)
}
