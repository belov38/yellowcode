use super::content::{Field, Tab, Val, Wire, described, fields_of, tags};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Form {
    Whole,
    Difference,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stored {
    pub form: Form,
    pub change: u64,
    pub fields: Vec<Field>,
    pub tabs: Vec<Tab>,
    pub values: Vec<Val>,
    pub lines: Vec<Vec<Vec<Val>>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct State {
    pub fields: Vec<Field>,
    pub tabs: Vec<Tab>,
    pub values: Vec<Val>,
    pub lines: Vec<Vec<Vec<Val>>>,
}

pub(crate) fn decode(content: &[u8]) -> Result<Stored, String> {
    let bad = || "содержимое версии не разбирается".to_string();
    let mut described_once = None;
    let mut change = None;
    let mut form = None;
    let mut values = Vec::new();
    let mut lines = Vec::new();
    for (n, w) in fields_of(content).ok_or_else(bad)? {
        match (n, w) {
            (2, Wire::Bytes(d)) if described_once.is_none() => {
                described_once = Some(described(d).ok_or("описание полей версии")?);
            }
            (3, Wire::Varint(c)) if change.is_none() && c <= 2 => change = Some(c),
            (4, Wire::Varint(1)) if form.is_none() => form = Some(Form::Whole),
            (4, Wire::Varint(2)) if form.is_none() => form = Some(Form::Difference),
            (5, Wire::Bytes(v)) => values.push(value(v)?),
            (6, Wire::Bytes(t)) => lines.push(section_lines(t)?),
            (n, _) => return Err(format!("поле {n} содержимого версии")),
        }
    }
    let form = form.ok_or("форма содержимого версии")?;
    let (fields, tabs) = described_once.unwrap_or_default();
    if values.len() != fields.len() {
        return Err("число значений версии".into());
    }
    match form {
        Form::Whole => {
            if lines.len() != tabs.len() {
                return Err("число табличных частей версии".into());
            }
            for (t, ls) in tabs.iter().zip(&lines) {
                if ls.iter().any(|l| l.len() != t.fields.len()) {
                    return Err("число колонок строки табличной части версии".into());
                }
            }
            if change == Some(2) && !fields.is_empty() {
                return Err("поля в версии удаления".into());
            }
        }
        Form::Difference => {
            if change.is_some() {
                return Err("вид изменения в разности версий".into());
            }
            if !tabs.is_empty() || !lines.is_empty() {
                return Err("разность версий с табличными частями".into());
            }
        }
    }
    Ok(Stored {
        form,
        change: change.unwrap_or(0),
        fields,
        tabs,
        values,
        lines,
    })
}

fn value(v: &[u8]) -> Result<Val, String> {
    let bad = || "значение версии не разбирается".to_string();
    let mut fs = fields_of(v).ok_or_else(bad)?;
    if fs.len() != 1 {
        return Err("значение версии не из одного поля".into());
    }
    let (tag, w) = fs.pop().ok_or_else(bad)?;
    let text = |b: &[u8]| String::from_utf8(b.to_vec()).map_err(|_| bad());
    Ok(match (tag, w) {
        (tags::NULL, Wire::Varint(1)) => Val::Null,
        (tags::BOOL, Wire::Varint(b @ (0 | 1))) => Val::Bool(b == 1),
        (tags::INT, Wire::Varint(n)) => Val::Int(n),
        (tags::STR, Wire::Bytes(b)) => Val::Str(text(b)?),
        (tags::DATE, Wire::Varint(t)) => Val::Date(t),
        (tags::UUID, Wire::Bytes(b)) => Val::Uuid(b.try_into().map_err(|_| bad())?),
        (tags::DECIMAL, Wire::Bytes(b)) => Val::Decimal(text(b)?),
        (tags::REF, Wire::Bytes(r)) => reference(r)?,
        (tags::STORAGE, Wire::Bytes(b)) => Val::Storage(b.to_vec()),
        (tag, _) => return Err(format!("значение версии с полем {tag}")),
    })
}

fn reference(r: &[u8]) -> Result<Val, String> {
    let bad = || "ссылка в версии не разбирается".to_string();
    let fs = fields_of(r).ok_or_else(bad)?;
    let table = |n: u64| u32::try_from(n).map_err(|_| bad());
    match fs.as_slice() {
        [(1, Wire::Bytes(id)), (2, Wire::Varint(t))] => Ok(Val::Ref {
            id: (*id).try_into().map_err(|_| bad())?,
            table: table(*t)?,
        }),
        [(2, Wire::Varint(t))] => Ok(Val::EmptyRef { table: table(*t)? }),
        _ => Err(bad()),
    }
}

fn section_lines(t: &[u8]) -> Result<Vec<Vec<Val>>, String> {
    let bad = || "строки табличной части версии не разбираются".to_string();
    let mut out = Vec::new();
    for (k, w) in fields_of(t).ok_or_else(bad)? {
        let (1, Wire::Bytes(row)) = (k, w) else {
            return Err(bad());
        };
        let mut line = Vec::new();
        for (c, cw) in fields_of(row).ok_or_else(bad)? {
            let (2, Wire::Bytes(v)) = (c, cw) else {
                return Err(bad());
            };
            line.push(value(v)?);
        }
        out.push(line);
    }
    Ok(out)
}

pub(crate) fn assemble(chain: &[(i64, &Stored)]) -> Result<Option<State>, String> {
    let start = chain
        .iter()
        .rposition(|(_, s)| s.form == Form::Whole)
        .ok_or("нет целой версии до запрошенной")?;
    let tail = chain.get(start..).unwrap_or_default();
    if tail
        .windows(2)
        .any(|w| matches!(w, [a, b] if a.0.checked_add(1) != Some(b.0)))
    {
        return Err("пропуск в номерах версий".into());
    }
    let mut state = None;
    for (i, (_, s)) in tail.iter().enumerate() {
        if i == 0 {
            if s.change != 2 {
                state = Some(State {
                    fields: s.fields.clone(),
                    tabs: s.tabs.clone(),
                    values: s.values.clone(),
                    lines: s.lines.clone(),
                });
            }
            continue;
        }
        let st = state.as_mut().ok_or("разность после версии удаления")?;
        for (f, v) in s.fields.iter().zip(&s.values) {
            let at = st
                .fields
                .iter()
                .position(|x| x == f)
                .ok_or("поле разности вне предыдущей версии")?;
            if let Some(slot) = st.values.get_mut(at) {
                *slot = v.clone();
            }
        }
    }
    Ok(state)
}
