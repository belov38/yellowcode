use std::io::Write;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub total: usize,
    pub differ: usize,
    pub not_in_dbschema: usize,
    pub not_in_schema: usize,
    pub no_number: usize,
}

impl Summary {
    pub fn of(notes: &[String]) -> Summary {
        let mut s = Summary {
            total: notes.len(),
            ..Summary::default()
        };
        for note in notes {
            if note.starts_with("DBNames has no ") {
                s.no_number += 1;
            } else if note.ends_with(" is not in the schema") {
                s.not_in_schema += 1;
            } else if note.ends_with(": not in DBSCHEMA") {
                s.not_in_dbschema += 1;
            } else if note.contains(" differs: ") {
                s.differ += 1;
            }
        }
        s
    }
}

pub fn write(err: &mut dyn Write, notes: &[String], list: bool) {
    if notes.is_empty() {
        return;
    }
    if list {
        for note in notes {
            let _ = writeln!(err, "yellowcode: schema note: {note}");
        }
        return;
    }
    let s = Summary::of(notes);
    let mut kinds = Vec::new();
    for (n, what) in [
        (s.differ, "differ"),
        (s.not_in_dbschema, "not in DBSCHEMA"),
        (s.not_in_schema, "not in the schema"),
        (s.no_number, "DBNames has no"),
    ] {
        if n > 0 {
            kinds.push(format!("{n} {what}"));
        }
    }
    let other = s.total - s.differ - s.not_in_dbschema - s.not_in_schema - s.no_number;
    if other > 0 {
        kinds.push(format!("{other} other"));
    }
    let _ = writeln!(
        err,
        "yellowcode: schema: {} notes ({}); --schema-notes to list",
        s.total,
        kinds.join(", ")
    );
}
