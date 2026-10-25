mod tree;

use bsl_vm::{Arg, Exception, Execution, HostObject, Lang, ObjRef, Value};

const MAX_DEPTH: u32 = 6;

pub fn repr(ex: &mut Execution, v: &Value) -> Result<String, Exception> {
    match repr_in(ex, v, 0) {
        Ok(s) => Ok(s),
        Err(Exception::Catchable(_)) => Ok(format!("<{}>", type_text(ex, v))),
        Err(e) => Err(e),
    }
}

pub(crate) fn repr_in(ex: &mut Execution, v: &Value, depth: u32) -> Result<String, Exception> {
    repr_in_live(ex, v, depth, 0)
}

fn repr_in_live(ex: &mut Execution, v: &Value, depth: u32, live: u64) -> Result<String, Exception> {
    if depth > MAX_DEPTH {
        return Ok("…".into());
    }
    Ok(match v {
        Value::Undefined => "Неопределено".into(),
        Value::Null => "NULL".into(),
        Value::Bool(b) => if *b { "Истина" } else { "Ложь" }.into(),
        Value::Number(n) if live != 0 => return tree::number(ex, n, live),
        Value::Number(n) => {
            ex.reserve(n.size_hint_limbs().saturating_mul(18))?;
            n.to_xml_string()
        }
        Value::String(s) if live != 0 => return tree::string(ex, s, live),
        Value::String(s) => format!("\"{}\"", s.to_string_lossy().replace('"', "\"\"")),
        Value::Date(d) => format!("'{}'", d.to_xml_string()),
        Value::Type(_) => format!("Тип({})", ex.present(v)?.to_string_lossy()),
        Value::Object(o) => return object_repr(ex, v, o, depth, live),
    })
}

fn type_text(ex: &mut Execution, v: &Value) -> String {
    let t = Value::Type(v.type_id());
    ex.present(&t)
        .map(|s| s.to_string_lossy())
        .unwrap_or_default()
}

fn items(ex: &mut Execution, o: &ObjRef) -> Result<Vec<Value>, Exception> {
    let Some(n) = o.iter_len(ex) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for i in 0..n {
        match o.iter_get(ex, i) {
            Ok(v) => out.push(v),
            Err(e) => return Err(ex.call_error(e)),
        }
    }
    Ok(out)
}

fn global_text(ex: &mut Execution, name: &str, v: &Value) -> Result<String, Exception> {
    let r = ex.call_global(name, &mut [Arg::Val(v.clone())])?;
    Ok(ex.present(&r)?.to_string_lossy())
}

fn object_repr(
    ex: &mut Execution,
    v: &Value,
    o: &ObjRef,
    depth: u32,
    live: u64,
) -> Result<String, Exception> {
    let tid = HostObject::type_id(&**o);
    let name = ex.runtime().types().name(tid, Lang::Ru).to_owned();
    let d = depth + 1;
    if live != 0
        && let Some(result) = tree::container(ex, v, o, &name, d, live)
    {
        return result;
    }
    match name.as_str() {
        "Массив" | "ФиксированныйМассив" => {
            let mut parts = Vec::new();
            for e in items(ex, o)? {
                parts.push(repr_in(ex, &e, d)?);
            }
            Ok(format!("{name}[{}]", parts.join(", ")))
        }
        "Структура" | "ФиксированнаяСтруктура" => {
            let mut parts = Vec::new();
            for kv in items(ex, o)? {
                let k = ex.get_member(&kv, "Ключ")?;
                let val = ex.get_member(&kv, "Значение")?;
                let key = ex.present(&k)?.to_string_lossy();
                parts.push(format!("{key}: {}", repr_in(ex, &val, d)?));
            }
            Ok(format!("{name}{{{}}}", parts.join(", ")))
        }
        "Соответствие" | "ФиксированноеСоответствие" => {
            let mut parts = Vec::new();
            for kv in items(ex, o)? {
                let k = ex.get_member(&kv, "Ключ")?;
                let val = ex.get_member(&kv, "Значение")?;
                parts.push(format!(
                    "{} => {}",
                    repr_in(ex, &k, d)?,
                    repr_in(ex, &val, d)?
                ));
            }
            Ok(format!("{name}{{{}}}", parts.join(", ")))
        }
        "ОписаниеТипов" => Ok(format!(
            "ОписаниеТипов({})",
            ex.present(v)?.to_string_lossy()
        )),
        "ДвоичныеДанные" => Ok(format!(
            "ДвоичныеДанные({})",
            global_text(ex, "ПолучитьHexСтрокуИзДвоичныхДанных", v)?
        )),
        "БуферДвоичныхДанных" => Ok(format!(
            "БуферДвоичныхДанных({})",
            global_text(ex, "ПолучитьHexСтрокуИзБуфераДвоичныхДанных", v)?
        )),
        "УникальныйИдентификатор" => Ok(format!(
            "УникальныйИдентификатор({})",
            ex.present(v)?.to_string_lossy()
        )),
        "СписокЗначений" => {
            let mut parts = Vec::new();
            for item in items(ex, o)? {
                let value = ex.get_member(&item, "Значение")?;
                let presentation = ex.get_member(&item, "Представление")?;
                let check = ex.get_member(&item, "Пометка")?;
                let mut part = repr_in(ex, &value, d)?;
                let text = ex.present(&presentation)?.to_string_lossy();
                if !text.is_empty() {
                    part.push_str(&format!(" '{text}'"));
                }
                if matches!(check, Value::Bool(true)) {
                    part.push_str(" +");
                }
                parts.push(part);
            }
            Ok(format!("СписокЗначений[{}]", parts.join(", ")))
        }
        "ТаблицаЗначений" => {
            let names = table_columns(ex, v)?;
            let mut text = String::from("ТаблицаЗначений<");
            for (i, name) in names.iter().enumerate() {
                if i != 0 {
                    append(ex, &mut text, ", ")?;
                }
                append(ex, &mut text, &ex_string(name))?;
            }
            append(ex, &mut text, ">[")?;
            let rows = o.iter_len(ex).unwrap_or(0);
            for i in 0..rows {
                let row = o.iter_get(ex, i).map_err(|e| ex.call_error(e))?;
                if i != 0 {
                    append(ex, &mut text, ", ")?;
                }
                append(ex, &mut text, "(")?;
                for (j, name) in names.iter().enumerate() {
                    if j != 0 {
                        append(ex, &mut text, ", ")?;
                    }
                    let value = ex.index_get(&row, name)?;
                    let part = repr_in(ex, &value, d)?;
                    append(ex, &mut text, &part)?;
                }
                append(ex, &mut text, ")")?;
            }
            append(ex, &mut text, "]")?;
            Ok(text)
        }
        "СтрокаТаблицыЗначений" => {
            let owner = ex.call_method(v, "Владелец", &mut [], true)?;
            let names = table_columns(ex, &owner)?;
            let mut text = String::from("СтрокаТаблицыЗначений(");
            for (i, name) in names.iter().enumerate() {
                if i != 0 {
                    append(ex, &mut text, ", ")?;
                }
                append(ex, &mut text, &ex_string(name))?;
                append(ex, &mut text, ": ")?;
                let value = ex.index_get(v, name)?;
                let part = repr_in(ex, &value, d)?;
                append(ex, &mut text, &part)?;
            }
            append(ex, &mut text, ")")?;
            Ok(text)
        }
        "ДеревоЗначений" => tree::repr(ex, v, depth, live),
        _ => {
            let t = type_text(ex, v);
            match ex.present(v) {
                Ok(s) => Ok(format!("<{t}> {}", s.to_string_lossy())),
                Err(Exception::Catchable(_)) => Ok(format!("<{t}>")),
                Err(e) => Err(e),
            }
        }
    }
}

fn ex_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.to_string_lossy(),
        _ => String::new(),
    }
}
fn append(ex: &mut Execution, text: &mut String, part: &str) -> Result<(), Exception> {
    ex.charge(part.len() as u64 / 64 + 1)?;
    ex.reserve(
        (text.capacity() as u64)
            .saturating_add(part.len() as u64)
            .saturating_mul(2),
    )?;
    text.try_reserve_exact(part.len())
        .map_err(|_| ex.out_of_memory())?;
    text.push_str(part);
    Ok(())
}
fn table_columns(ex: &mut Execution, table: &Value) -> Result<Vec<Value>, Exception> {
    let columns = ex.get_member(table, "Колонки")?;
    let Some(object) = columns.as_object() else {
        return Ok(Vec::new());
    };
    let len = object.iter_len(ex).unwrap_or(0);
    ex.reserve((len as u64).saturating_mul(std::mem::size_of::<Value>() as u64))?;
    ex.charge(len as u64)?;
    let mut names = Vec::new();
    names
        .try_reserve_exact(len)
        .map_err(|_| ex.out_of_memory())?;
    for i in 0..len {
        let column = object.iter_get(ex, i).map_err(|e| ex.call_error(e))?;
        let name = ex.get_member(&column, "Имя")?;
        if let Value::String(s) = &name {
            ex.reserve((s.len() as u64).saturating_mul(4))?;
        }
        names.push(name);
    }
    Ok(names)
}
