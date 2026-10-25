mod managed;
pub use managed::{
    measure_managed_repr, measure_native_repr, repr_budgeted, repr_lang_budgeted,
    repr_managed_budgeted, repr_managed_lang_budgeted, write_managed_repr, write_native_repr,
};

use crate::host::TestBase;
use query_ir::{QType, QValue, uuid_of};
use query_syntax::Lang;
use query_testkit::repr::platform;

fn platform_text(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied())
        .map(|r| r.unwrap_or('?'))
        .collect()
}

pub fn repr(v: &QValue, base: &TestBase) -> String {
    repr_lang(v, base, Lang::Ru)
}

pub fn repr_lang(v: &QValue, base: &TestBase, lang: Lang) -> String {
    match v {
        QValue::Undefined => "Неопределено".to_string(),
        QValue::Null => "NULL".to_string(),
        QValue::Bool(true) => "Истина".to_string(),
        QValue::Bool(false) => "Ложь".to_string(),
        QValue::Number(n) => n.to_xml_string(),
        QValue::String(s) => format!("\"{}\"", platform_text(&s.0).replace('"', "\"\"")),
        QValue::Date(d) => format!("'{}'", d.to_xml_string()),
        QValue::Type(t) => format!("Тип({})", type_name(t, base, lang)),
        QValue::Uuid(u) => format!("УникальныйИдентификатор({})", uuid_text(u)),
        QValue::Binary(b) => {
            let hex: String = b.iter().map(|x| format!("{x:02X}")).collect();
            format!("ДвоичныеДанные({hex})")
        }
        QValue::Storage(_) => "<ХранилищеЗначения>".to_string(),
        QValue::SysEnum(t, i) => {
            let ti = platform_catalog::catalog().type_info(*t);
            let value = usize::try_from(*i)
                .ok()
                .and_then(|i| ti.values.get(i))
                .map_or("?", |x| x.name_en.as_str());
            format!("{}({value})", xml_type_name(&ti.name_en))
        }
        QValue::Ref { table, id } => ref_text(*table, id, base),
        QValue::Nested(n) => {
            let cols: Vec<String> = n
                .columns
                .iter()
                .map(|c| {
                    let ty = query_testkit::repr::type_repr(&base.schema, &c.ty, lang);
                    format!("{}: {ty}", c.name)
                })
                .collect();
            let rows: Vec<String> = n
                .rows
                .iter()
                .map(|r| {
                    let vals: Vec<String> = r.iter().map(|v| repr_lang(v, base, lang)).collect();
                    format!("({})", vals.join(", "))
                })
                .collect();
            format!("РезультатЗапроса<{}>[{}]", cols.join("; "), rows.join(", "))
        }
        QValue::PointInTime(m) => {
            let date = repr_lang(&QValue::Date(m.date), base, lang);
            let r = match m.reference {
                Some((table, id)) => ref_text(table, &id, base),
                None => "Неопределено".to_string(),
            };
            format!("МоментВремени({date}, {r})")
        }
        QValue::Boundary(b) => {
            let kind = if b.include {
                "Включая"
            } else {
                "Исключая"
            };
            format!("Граница({}, {kind})", repr_lang(&b.point, base, lang))
        }
    }
}

fn uuid_text(bytes: &[u8; 16]) -> String {
    let mut s = String::with_capacity(36);
    for (i, b) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn ref_text(table: u32, id: &[u8; 16], base: &TestBase) -> String {
    let Some(atom) = base.by_number.get(&table) else {
        return format!("Ref#{table}({})", uuid_of(id));
    };
    let name = &base.schema.ref_info(*atom).xml_name;
    if let Some(p) = base.predefined_names.get(&(table, *id)) {
        return format!("{name}(@{p})");
    }
    if let Some(e) = base.enum_names.get(&(table, *id)) {
        return format!("{name}({e})");
    }
    if *id == [0; 16] && base.schema.ref_info(*atom).kind == md_model::MdKind::Enum {
        return format!("{name}()");
    }
    format!("{name}({})", uuid_of(id))
}

fn type_name(t: &QType, base: &TestBase, lang: Lang) -> String {
    if let Some(r) = t.refs.first() {
        return base.schema.ref_info(*r).xml_name.clone();
    }
    let en = if t.null {
        "Null"
    } else if t.boolean {
        "Boolean"
    } else if t.number.is_some() {
        "Number"
    } else if t.string.is_some() {
        "String"
    } else if t.date.is_some() {
        "Date"
    } else if t.others.is_empty() {
        return match lang {
            Lang::Ru => "Не определено",
            Lang::En => "Undefined",
        }
        .to_string();
    } else {
        return query_testkit::repr::type_repr(&base.schema, t, lang)
            .trim_start_matches("ОписаниеТипов(")
            .trim_end_matches(')')
            .to_string();
    };
    platform(en, lang)
}

fn xml_type_name(en: &str) -> &str {
    match en {
        "AccumulationRecordType" => "AccumulationMovementType",
        "AccountingRecordType" => "AccountingMovementType",
        other => other,
    }
}
