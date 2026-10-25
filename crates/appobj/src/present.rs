use crate::Application;
use crate::model::{ObjDef, ObjId, ObjKind, StdAttr};
use crate::objects::ObjectInstance;
use crate::read::{ReadRow, field_index, read_object_row};
use crate::session::call_ctx;
use crate::values::refs::RefValue;
use crate::values::text::broken_ref_text;
use bsl_prim::Str;
use bsl_vm::{Arg, CallError, CallResult, Execution, ModuleInstanceRef, Place, Value};
use ibschema::values::RefId;
use std::collections::HashMap;
use std::rc::Rc;

const PRESENT_HANDLER: &str = "ОбработкаПолученияПредставления";
const FIELDS_HANDLER: &str = "ОбработкаПолученияПолейПредставления";
const PRESENT_HANDLER_EN: &str = "PresentationGetProcessing";
const FIELDS_HANDLER_EN: &str = "PresentationFieldsGetProcessing";

pub const HANDLER_WEIGHT: u32 = 208;

#[derive(Default)]
pub(crate) struct PresentationCache {
    done: HashMap<RefValue, Vec<u16>>,
    fields: HashMap<ObjId, FieldsInfo>,
}

#[derive(Clone)]
struct FieldsInfo {
    names: Vec<String>,
    standard: bool,
}

impl PresentationCache {
    pub(crate) fn forget(&mut self, r: RefValue) {
        self.done.remove(&r);
    }
}

fn cache(ex: &mut Execution) -> Option<&mut PresentationCache> {
    call_ctx(ex).map(|c| &mut c.sess.presentations)
}

#[doc(hidden)]
pub fn forget(ex: &mut Execution, r: RefValue) {
    if let Some(c) = cache(ex) {
        c.forget(r);
    }
}

pub fn ref_presentation(ex: &mut Execution, app: &Application, r: RefValue) -> CallResult<Str> {
    if r.is_empty() {
        return Ok(Str::new());
    }
    let def = app.model().obj(r.obj);
    if def.kind == ObjKind::Enum {
        let found = def
            .enum_values
            .iter()
            .find(|v| RefId::from_uuid(v.md) == r.id);
        return Ok(match found {
            Some(v) => {
                Str::from(strict_synonym(&v.synonym, &v.name, &app.model().lang_code).as_str())
            }
            None => broken(app, r),
        });
    }
    let row = read_object_row(ex, app, r)?;
    if !row.found {
        return Ok(broken(app, r));
    }
    present_row(ex, app, r.obj, Some(r), &row)
}

pub fn object_presentation(
    ex: &mut Execution,
    app: &Application,
    o: &ObjectInstance,
) -> CallResult<Str> {
    let (obj, _, row) = o.presentation_parts();
    present_row(ex, app, obj, None, &row)
}

fn broken(app: &Application, r: RefValue) -> Str {
    let no = app
        .model()
        .storage_of(r.obj)
        .and_then(|s| s.ref_number)
        .unwrap_or(0);
    Str::from(broken_ref_text(no, &r.id).as_str())
}

fn present_row(
    ex: &mut Execution,
    app: &Application,
    obj: ObjId,
    key: Option<RefValue>,
    row: &Rc<ReadRow>,
) -> CallResult<Str> {
    let Some(handler) = handler_of(ex, app, obj)? else {
        return standard(ex, app, obj, row);
    };
    if let Some(k) = key
        && let Some(c) = cache(ex)
        && let Some(s) = c.done.get(&k)
    {
        return Ok(Str::from_units(s));
    }
    let r = run_handler(ex, app, obj, row, &handler);
    if let Some(k) = key
        && let Some(c) = cache(ex)
        && let Ok(s) = &r
    {
        c.done.insert(k, s.units().to_vec());
    }
    r
}

struct Handler {
    module: bsl_vm::ModuleRef,
    present: Option<String>,
    fields: Option<String>,
}

fn handler_of(ex: &mut Execution, app: &Application, obj: ObjId) -> CallResult<Option<Handler>> {
    let Some(module) = app
        .modules()
        .of_object(obj, md_model::ModuleKind::ManagerModule)
    else {
        return Ok(None);
    };
    let compiled = app
        .modules()
        .compiled_or_fatal(module, ex.lang())
        .map_err(CallError::Exc)?;
    let find = |names: [&str; 2]| {
        names
            .iter()
            .find_map(|n| compiled.method(n).map(|(_, m)| m.name.name.to_string()))
    };
    let present = find([PRESENT_HANDLER, PRESENT_HANDLER_EN]);
    let fields = find([FIELDS_HANDLER, FIELDS_HANDLER_EN]);
    if present.is_none() && fields.is_none() {
        return Ok(None);
    }
    Ok(Some(Handler {
        module,
        present,
        fields,
    }))
}

fn run_handler(
    ex: &mut Execution,
    app: &Application,
    obj: ObjId,
    row: &Rc<ReadRow>,
    h: &Handler,
) -> CallResult<Str> {
    let inst = app.instance_in_call(ex, h.module).map_err(CallError::Exc)?;
    let info = data_fields(ex, app, obj, &inst, h)?;
    let Some(present) = &h.present else {
        return if info.standard {
            standard(ex, app, obj, row)
        } else {
            Ok(Str::from("<>"))
        };
    };
    let names = info.names;
    let def = app.model().obj(obj);
    let mut data: Vec<(&str, Value)> = Vec::with_capacity(names.len());
    for n in &names {
        if let Some(i) = field_index(def, n) {
            data.push((n.as_str(), row.value(app, obj, i)?));
        }
    }
    let presentation = Place::temp(Value::from(""));
    let standard = Place::temp(Value::Bool(true));
    let mut args = [
        Arg::Val(bsl_stdlib::new_structure(data)),
        Arg::Place(presentation.clone()),
        Arg::Place(standard.clone()),
    ];
    ex.call_weighted(&inst, present, &mut args, HANDLER_WEIGHT)
        .map_err(CallError::Exc)?;
    if matches!(standard.temp_value(), Some(Value::Bool(false))) {
        return match presentation.temp_value() {
            Some(Value::String(s)) => Ok(s),
            Some(v) => ex.present(&v).map_err(CallError::Exc),
            None => Ok(Str::new()),
        };
    }
    self::standard(ex, app, obj, row)
}

fn data_fields(
    ex: &mut Execution,
    app: &Application,
    obj: ObjId,
    inst: &ModuleInstanceRef,
    h: &Handler,
) -> CallResult<FieldsInfo> {
    let default = FieldsInfo {
        names: std::iter::once("Ссылка".to_string())
            .chain(standard_fields(app.model().obj(obj)))
            .collect(),
        standard: true,
    };
    let Some(name) = &h.fields else {
        return Ok(default);
    };
    if let Some(f) = cache(ex).and_then(|c| c.fields.get(&obj).cloned()) {
        return Ok(f);
    }
    if let Some(c) = cache(ex) {
        c.fields.insert(obj, default.clone());
    }
    let fields = Place::temp(bsl_stdlib::new_array(Vec::new()));
    let standard = Place::temp(Value::Bool(true));
    let mut args = [Arg::Place(fields.clone()), Arg::Place(standard.clone())];
    match ex.call_weighted(inst, name, &mut args, HANDLER_WEIGHT) {
        Ok(_) => {}
        Err(bsl_vm::Exception::Catchable(_)) => return Ok(default),
        Err(e) => {
            if let Some(c) = cache(ex) {
                c.fields.remove(&obj);
            }
            return Err(CallError::Exc(e));
        }
    }
    let named: Vec<String> = fields
        .temp_value()
        .and_then(|v| bsl_stdlib::array_items(&v))
        .unwrap_or_default()
        .iter()
        .filter_map(|v| match v {
            Value::String(s) => Some(s.to_string_lossy().trim().to_string()),
            _ => None,
        })
        .filter(|s| !s.is_empty())
        .collect();
    let keep_standard = matches!(standard.temp_value(), Some(Value::Bool(true)));
    let out = if keep_standard {
        default
    } else {
        FieldsInfo {
            names: named,
            standard: false,
        }
    };
    if let Some(c) = cache(ex) {
        c.fields.insert(obj, out.clone());
    }
    Ok(out)
}

fn standard_fields(def: &ObjDef) -> Vec<String> {
    let has = |a: StdAttr| def.fields.iter().any(|f| f.std == Some(a));
    let name = |a: StdAttr| a.names().0.to_string();
    if has(StdAttr::Description) || has(StdAttr::Code) {
        let attr =
            if def.behavior.present_by_code && has(StdAttr::Code) || !has(StdAttr::Description) {
                StdAttr::Code
            } else {
                StdAttr::Description
            };
        return vec![name(attr)];
    }
    if has(StdAttr::Number) && has(StdAttr::Date) {
        return vec![name(StdAttr::Number), name(StdAttr::Date)];
    }
    Vec::new()
}

fn standard(ex: &mut Execution, app: &Application, obj: ObjId, row: &ReadRow) -> CallResult<Str> {
    let def = app.model().obj(obj);
    let attr = |ex: &mut Execution, a: StdAttr| -> CallResult<Option<Str>> {
        match def.fields.iter().position(|f| f.std == Some(a)) {
            Some(i) => {
                let v = row.value(app, obj, i)?;
                Ok(Some(ex.present(&v).map_err(CallError::Exc)?))
            }
            None => Ok(None),
        }
    };
    let fields = standard_fields(def);
    let has_number = fields.len() == 2;
    if has_number {
        let number = attr(ex, StdAttr::Number)?.unwrap_or_default();
        let date = attr(ex, StdAttr::Date)?.unwrap_or_default();
        let title = strict_synonym(&def.synonym, &def.name, &app.model().lang_code);
        let text = format!(
            "{title} {} от {}",
            number.to_string_lossy().trim_end_matches(' '),
            date.to_string_lossy()
        );
        return Ok(Str::from(text.as_str()));
    }
    match fields.first().map(String::as_str) {
        Some(n) if n == StdAttr::Code.names().0 => {
            let code = attr(ex, StdAttr::Code)?.unwrap_or_default();
            Ok(code)
        }
        Some(_) => {
            let d = attr(ex, StdAttr::Description)?.unwrap_or_default();
            Ok(if d.is_empty() { Str::from("<>") } else { d })
        }
        None => Ok(Str::from("<>")),
    }
}

fn strict_synonym(s: &md_model::LocalString, name: &str, code: &str) -> String {
    crate::types::synonym(s, name, code)
}
