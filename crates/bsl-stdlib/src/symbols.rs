use crate::enums::{manager, type_by_id};
use bsl_vm::{Registry, Value};
use platform_catalog::{TypeIdx, catalog};

const CHARS: [(&str, &str); 6] = [
    ("LF", "\n"),
    ("CR", "\r"),
    ("Tab", "\t"),
    ("NBSp", "\u{a0}"),
    ("FF", "\u{c}"),
    ("VTab", "\u{b}"),
];

pub(crate) fn register(reg: &mut Registry) {
    if let Some(t) = type_by_id("Chars") {
        reg.system_enum(t, manager(t, char_member));
    }
}

pub(crate) fn char_member(t: TypeIdx, i: usize) -> Option<Value> {
    let name = &catalog()
        .types()
        .get(usize::from(t.0))?
        .values
        .get(i)?
        .name_en;
    let (_, c) = CHARS.iter().find(|(en, _)| en == name)?;
    Some(Value::from(*c))
}
