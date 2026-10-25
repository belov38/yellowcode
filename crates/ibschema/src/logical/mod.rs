mod binary;
mod managed;
mod text;
mod tree;

pub const VAR: u32 = 0x8000_0000;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LogicalSchema {
    pub tables: Vec<Table>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    Main,
    Nested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    Ref,
    Plain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub name: String,
    pub kind: TableKind,
    pub number: u32,
    pub parent: String,
    pub fields: Vec<Field>,
    pub nested: Vec<Table>,
    pub indexes: Vec<Index>,
    pub one: i64,
    pub key: KeyKind,
    pub splitters: [Vec<Vec<String>>; 2],
    pub tail_s: String,
    pub tail_n1: i64,
    pub tail_n2: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub nullable: bool,
    pub types: Vec<LType>,
    pub tail_s: String,
    pub tail_n: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeCode {
    B,
    L,
    N,
    S,
    T,
    R,
    E,
    V,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LType {
    pub code: TypeCode,
    pub length: u32,
    pub precision: u32,
    pub ref_table: String,
    pub ref_kind: u32,
    pub extra: Option<i64>,
}

impl LType {
    pub fn new(code: TypeCode, length: u32, precision: u32) -> LType {
        LType {
            code,
            length,
            precision,
            ref_table: String::new(),
            ref_kind: 0,
            extra: None,
        }
    }

    pub fn reference(table: impl Into<String>, kind: u32) -> LType {
        LType {
            ref_table: table.into(),
            ref_kind: kind,
            ..LType::new(TypeCode::R, 0, 0)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    pub name: String,
    pub unique: bool,
    pub fields: Vec<String>,
    pub clustered: bool,
    pub flag2: bool,
    pub n3: i64,
    pub list: Vec<i64>,
    pub n4: i64,
    pub n5: i64,
}

impl TableKind {
    pub(crate) fn letter(self) -> &'static str {
        match self {
            TableKind::Main => "N",
            TableKind::Nested => "I",
        }
    }

    pub(crate) fn from_letter(s: &str) -> Option<TableKind> {
        match s {
            "N" => Some(TableKind::Main),
            "I" => Some(TableKind::Nested),
            _ => None,
        }
    }
}

impl KeyKind {
    pub(crate) fn letter(self) -> &'static str {
        match self {
            KeyKind::Ref => "R",
            KeyKind::Plain => "S",
        }
    }

    pub(crate) fn from_letter(s: &str) -> Option<KeyKind> {
        match s {
            "R" => Some(KeyKind::Ref),
            "S" => Some(KeyKind::Plain),
            _ => None,
        }
    }
}

impl TypeCode {
    pub(crate) fn letter(self) -> &'static str {
        match self {
            TypeCode::B => "B",
            TypeCode::L => "L",
            TypeCode::N => "N",
            TypeCode::S => "S",
            TypeCode::T => "T",
            TypeCode::R => "R",
            TypeCode::E => "E",
            TypeCode::V => "V",
        }
    }

    pub(crate) fn from_letter(s: &str) -> Option<TypeCode> {
        Some(match s {
            "B" => TypeCode::B,
            "L" => TypeCode::L,
            "N" => TypeCode::N,
            "S" => TypeCode::S,
            "T" => TypeCode::T,
            "R" => TypeCode::R,
            "E" => TypeCode::E,
            "V" => TypeCode::V,
            _ => return None,
        })
    }
}

impl LogicalSchema {
    pub fn walk(&self) -> Vec<(&Table, Option<&Table>)> {
        let mut out = Vec::new();
        let mut stack: Vec<(&Table, Option<&Table>)> =
            self.tables.iter().rev().map(|t| (t, None)).collect();
        while let Some((t, owner)) = stack.pop() {
            out.push((t, owner));
            stack.extend(t.nested.iter().rev().map(|n| (n, Some(t))));
        }
        out
    }

    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables.iter().find(|t| t.name == name)
    }
}

impl Table {
    pub fn new(
        name: impl Into<String>,
        kind: TableKind,
        number: u32,
        parent: impl Into<String>,
    ) -> Table {
        Table {
            name: name.into(),
            kind,
            number,
            parent: parent.into(),
            fields: Vec::new(),
            nested: Vec::new(),
            indexes: Vec::new(),
            one: 1,
            key: KeyKind::Plain,
            splitters: [Vec::new(), Vec::new()],
            tail_s: String::new(),
            tail_n1: 0,
            tail_n2: 0,
        }
    }

    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }
}

impl Field {
    pub fn new(name: impl Into<String>, nullable: bool, types: Vec<LType>) -> Field {
        Field {
            name: name.into(),
            nullable,
            types,
            tail_s: String::new(),
            tail_n: 0,
        }
    }
}

impl Index {
    pub fn new(name: impl Into<String>, unique: bool, fields: Vec<String>) -> Index {
        Index {
            name: name.into(),
            unique,
            fields,
            clustered: false,
            flag2: false,
            n3: 0,
            list: vec![0],
            n4: 0,
            n5: 0,
        }
    }
}
