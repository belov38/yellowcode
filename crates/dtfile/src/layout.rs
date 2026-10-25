use ibschema::columns::{ColRole, ColType, Column, Origin, Part};

fn columns(spec: &[(&str, ColType, ColRole, bool)]) -> Vec<Column> {
    spec.iter()
        .enumerate()
        .map(|(i, &(name, ty, role, nullable))| Column {
            name: name.to_string(),
            ty,
            role,
            nullable,
            origin: Origin::Field(i),
            part: Part::Single,
        })
        .collect()
}

pub fn folder() -> Vec<Column> {
    columns(&[
        ("FILENAME", ColType::NVC(128), ColRole::Str, false),
        ("CREATION", ColType::DT, ColRole::DateTime, false),
        ("MODIFIED", ColType::DT, ColRole::DateTime, false),
        ("ATTRIBUTES", ColType::N(5, 0), ColRole::Number, false),
        ("DATASIZE", ColType::N(20, 0), ColRole::Number, false),
        ("BINARYDATA", ColType::I, ColRole::Binary, false),
    ])
}

pub fn users() -> Vec<Column> {
    columns(&[
        ("ID", ColType::B(16), ColRole::Ref, false),
        ("NAME", ColType::NVC(64), ColRole::Str, false),
        ("DESCR", ColType::NVC(128), ColRole::Str, false),
        ("OSNAME", ColType::NVC(128), ColRole::Str, true),
        ("CHANGED", ColType::DT, ColRole::DateTime, false),
        ("ROLESID", ColType::N(10, 0), ColRole::Number, false),
        ("SHOW", ColType::L, ColRole::Bool, false),
        ("DATA", ColType::I, ColRole::Binary, false),
    ])
}

pub fn users_spr() -> Vec<Column> {
    let mut out = users();
    for mut column in columns(&[
        ("EAUTH", ColType::L, ColRole::Bool, true),
        ("ADMROLE", ColType::L, ColRole::Bool, true),
        ("USSPRH", ColType::N(10, 0), ColRole::Number, true),
    ]) {
        column.origin = Origin::Field(out.len());
        out.push(column);
    }
    out
}

pub fn password_policies() -> Vec<Column> {
    columns(&[
        ("NAME", ColType::NVC(64), ColRole::Str, false),
        ("DATA", ColType::I, ColRole::Binary, false),
    ])
}

pub fn compromised_passwords() -> Vec<Column> {
    columns(&[("PWDHASH", ColType::NVC(256), ColRole::Str, false)])
}
