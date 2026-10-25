use crate::error::Result;
use crate::format::brace::{Node, bad_descr, parse_brace};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    N,
    C,
    NC,
    B,
    VC,
    NVC,
    VB,
    T,
    NT,
    I,
    L,
    D,
    DT,
    RV,
}

const TYPE_CODES: [(FieldType, &str); 14] = [
    (FieldType::N, "N"),
    (FieldType::C, "C"),
    (FieldType::NC, "NC"),
    (FieldType::B, "B"),
    (FieldType::VC, "VC"),
    (FieldType::NVC, "NVC"),
    (FieldType::VB, "VB"),
    (FieldType::T, "T"),
    (FieldType::NT, "NT"),
    (FieldType::I, "I"),
    (FieldType::L, "L"),
    (FieldType::D, "D"),
    (FieldType::DT, "DT"),
    (FieldType::RV, "RV"),
];

impl FieldType {
    pub fn code(&self) -> &'static str {
        match self {
            FieldType::N => "N",
            FieldType::C => "C",
            FieldType::NC => "NC",
            FieldType::B => "B",
            FieldType::VC => "VC",
            FieldType::NVC => "NVC",
            FieldType::VB => "VB",
            FieldType::T => "T",
            FieldType::NT => "NT",
            FieldType::I => "I",
            FieldType::L => "L",
            FieldType::D => "D",
            FieldType::DT => "DT",
            FieldType::RV => "RV",
        }
    }

    pub fn from_code(s: &str) -> Option<FieldType> {
        TYPE_CODES
            .iter()
            .find_map(|&(ty, code)| (code == s).then_some(ty))
    }

    pub fn is_string(&self) -> bool {
        matches!(
            self,
            FieldType::C | FieldType::NC | FieldType::VC | FieldType::NVC
        )
    }

    pub fn is_blob(&self) -> bool {
        matches!(self, FieldType::T | FieldType::NT | FieldType::I)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDescr {
    pub name: String,
    pub ty: FieldType,
    pub nullable: bool,
    pub length: u32,
    pub precision: u32,
    pub case_sensitive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexField {
    pub name: String,
    pub prefix_len: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexDescr {
    pub name: String,
    pub unique: bool,
    pub fields: Vec<IndexField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDescr {
    pub name: String,
    pub fields: Vec<FieldDescr>,
    pub indexes: Vec<IndexDescr>,
    pub recordlock: bool,
    pub files: [u32; 3],
}

pub(crate) fn names_eq(a: &str, b: &str) -> bool {
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(b);
    }
    a.chars()
        .flat_map(char::to_lowercase)
        .eq(b.chars().flat_map(char::to_lowercase))
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn section(title: &str, items: &[String]) -> String {
    if items.is_empty() {
        format!("{{{}}}", quote(title))
    } else {
        format!("{{{},\n{}\n}}", quote(title), items.join(",\n"))
    }
}

fn expect_str<'a>(n: &'a Node, what: &str) -> Result<&'a str> {
    match n {
        Node::Str(s) => Ok(s),
        _ => Err(bad_descr(format!("{what}: expected a quoted string"))),
    }
}

fn expect_u32(n: &Node, what: &str) -> Result<u32> {
    match n {
        Node::Atom(a) if !a.is_empty() && a.bytes().all(|b| b.is_ascii_digit()) => a
            .parse()
            .map_err(|_| bad_descr(format!("{what}: number {a} is out of range"))),
        _ => Err(bad_descr(format!("{what}: expected an unsigned number"))),
    }
}

fn expect_flag(n: &Node, what: &str) -> Result<bool> {
    match expect_u32(n, what)? {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(bad_descr(format!(
            "{what}: flag must be 0 or 1, got {other}"
        ))),
    }
}

fn expect_section<'a>(n: &'a Node, title: &str) -> Result<&'a [Node]> {
    let Node::List(items) = n else {
        return Err(bad_descr(format!("{title}: expected a list")));
    };
    match items.split_first() {
        Some((Node::Str(t), rest)) if t == title => Ok(rest),
        _ => Err(bad_descr(format!("expected section {title}"))),
    }
}

fn parse_field(n: &Node) -> Result<FieldDescr> {
    let Node::List(items) = n else {
        return Err(bad_descr("field: expected a list"));
    };
    let [name, ty, nullable, length, precision, case] = items.as_slice() else {
        return Err(bad_descr(format!(
            "field: expected 6 items, found {}",
            items.len()
        )));
    };
    let name = expect_str(name, "field name")?;
    let what = |part: &str| format!("field {name} {part}");
    let code = expect_str(ty, &what("type"))?;
    let ty = FieldType::from_code(code)
        .ok_or_else(|| bad_descr(format!("field {name}: unknown type {code:?}")))?;
    let case_sensitive = match expect_str(case, &what("case"))? {
        "CS" => true,
        "CI" => false,
        other => {
            return Err(bad_descr(format!(
                "field {name}: case must be CS or CI, got {other:?}"
            )));
        }
    };
    Ok(FieldDescr {
        name: name.to_string(),
        ty,
        nullable: expect_flag(nullable, &what("null flag"))?,
        length: expect_u32(length, &what("length"))?,
        precision: expect_u32(precision, &what("precision"))?,
        case_sensitive,
    })
}

fn parse_index(n: &Node) -> Result<IndexDescr> {
    let Node::List(items) = n else {
        return Err(bad_descr("index: expected a list"));
    };
    let (Some(name), Some(unique), Some(fields)) = (items.first(), items.get(1), items.get(2..))
    else {
        return Err(bad_descr("index: expected a name, a flag and fields"));
    };
    let name = expect_str(name, "index name")?;
    if fields.is_empty() {
        return Err(bad_descr(format!("index {name}: no fields")));
    }
    let fields = fields
        .iter()
        .map(|f| match f {
            Node::List(parts) => match parts.as_slice() {
                [field, prefix] => Ok(IndexField {
                    name: expect_str(field, "index field name")?.to_string(),
                    prefix_len: expect_u32(prefix, &format!("index {name} prefix length"))?,
                }),
                _ => Err(bad_descr(format!(
                    "index {name}: field item needs a name and a prefix length"
                ))),
            },
            _ => Err(bad_descr(format!("index {name}: field item is not a list"))),
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(IndexDescr {
        name: name.to_string(),
        unique: expect_flag(unique, &format!("index {name} unique flag"))?,
        fields,
    })
}

impl TableDescr {
    pub fn parse(text: &str) -> Result<TableDescr> {
        let Node::List(top) = parse_brace(text)? else {
            return Err(bad_descr("the description is not a list"));
        };
        let [name, zero, fields, indexes, recordlock, files] = top.as_slice() else {
            return Err(bad_descr(format!(
                "expected 6 top-level items, found {}",
                top.len()
            )));
        };
        let name = expect_str(name, "table name")?.to_string();
        if *zero != Node::Atom("0".into()) {
            return Err(bad_descr(format!("table {name}: second item must be 0")));
        }
        let fields = expect_section(fields, "Fields")?
            .iter()
            .map(parse_field)
            .collect::<Result<Vec<_>>>()?;
        let indexes = expect_section(indexes, "Indexes")?
            .iter()
            .map(parse_index)
            .collect::<Result<Vec<_>>>()?;
        let recordlock = match expect_section(recordlock, "Recordlock")? {
            [Node::Str(v)] if v == "0" => false,
            [Node::Str(v)] if v == "1" => true,
            _ => {
                return Err(bad_descr(format!(
                    "table {name}: Recordlock must be \"0\" or \"1\""
                )));
            }
        };
        let files = match expect_section(files, "Files")? {
            [a, b, c] => [
                expect_u32(a, "Files")?,
                expect_u32(b, "Files")?,
                expect_u32(c, "Files")?,
            ],
            other => {
                return Err(bad_descr(format!(
                    "table {name}: Files needs 3 numbers, found {}",
                    other.len()
                )));
            }
        };
        let d = TableDescr {
            name,
            fields,
            indexes,
            recordlock,
            files,
        };
        d.check_indexes()?;
        Ok(d)
    }

    fn check_indexes(&self) -> Result<()> {
        for ix in &self.indexes {
            for f in &ix.fields {
                let Some((_, field)) = self.field(&f.name) else {
                    return Err(bad_descr(format!(
                        "table {}: index {} refers to the unknown field {}",
                        self.name, ix.name, f.name
                    )));
                };
                if field.ty.is_blob() || field.ty == FieldType::RV {
                    return Err(bad_descr(format!(
                        "table {}: index {} includes the {} field {}",
                        self.name,
                        ix.name,
                        field.ty.code(),
                        f.name
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn to_text(&self) -> String {
        let fields: Vec<String> = self
            .fields
            .iter()
            .map(|f| {
                format!(
                    "{{{},{},{},{},{},{}}}",
                    quote(&f.name),
                    quote(f.ty.code()),
                    u8::from(f.nullable),
                    f.length,
                    f.precision,
                    quote(if f.case_sensitive { "CS" } else { "CI" })
                )
            })
            .collect();
        let indexes: Vec<String> = self
            .indexes
            .iter()
            .map(|ix| {
                let fields: Vec<String> = ix
                    .fields
                    .iter()
                    .map(|f| format!("{{{},{}}}", quote(&f.name), f.prefix_len))
                    .collect();
                format!(
                    "{{{},{},\n{}\n}}",
                    quote(&ix.name),
                    u8::from(ix.unique),
                    fields.join(",\n")
                )
            })
            .collect();
        let [data, blob, index] = self.files;
        format!(
            "{{{},0,\n{},\n{},\n{{\"Recordlock\",\"{}\"}},\n{{\"Files\",{data},{blob},{index}}}\n}}",
            quote(&self.name),
            section("Fields", &fields),
            section("Indexes", &indexes),
            u8::from(self.recordlock),
        )
    }

    pub fn field(&self, name: &str) -> Option<(usize, &FieldDescr)> {
        self.fields
            .iter()
            .enumerate()
            .find(|(_, f)| names_eq(&f.name, name))
    }

    pub fn index(&self, name: &str) -> Option<(usize, &IndexDescr)> {
        self.indexes
            .iter()
            .enumerate()
            .find(|(_, i)| names_eq(&i.name, name))
    }
}
