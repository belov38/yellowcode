use query_ir::QValue;

pub fn not3(a: Option<bool>) -> Option<bool> {
    a.map(|b| !b)
}

pub fn and3(items: impl IntoIterator<Item = Option<bool>>) -> Option<bool> {
    let mut unknown = false;
    for i in items {
        match i {
            Some(false) => return Some(false),
            None => unknown = true,
            Some(true) => {}
        }
    }
    if unknown { None } else { Some(true) }
}

pub fn or3(items: impl IntoIterator<Item = Option<bool>>) -> Option<bool> {
    let mut unknown = false;
    for i in items {
        match i {
            Some(true) => return Some(true),
            None => unknown = true,
            Some(false) => {}
        }
    }
    if unknown { None } else { Some(false) }
}

pub fn truth(v: &QValue) -> Option<bool> {
    match v {
        QValue::Bool(b) => Some(*b),
        _ => None,
    }
}

pub fn case_truth(v: &QValue) -> bool {
    match v {
        QValue::Bool(b) => *b,
        QValue::Number(n) => !n.is_zero(),
        _ => false,
    }
}

pub fn to_value(t: Option<bool>) -> QValue {
    t.map_or(QValue::Null, QValue::Bool)
}
