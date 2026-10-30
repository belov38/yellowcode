use std::fmt;

use super::{Field, Index, KeyKind, LType, LogicalSchema, Table, TableKind, TypeCode};
use crate::error::{Error, Result};

pub(crate) trait TreeNode: Sized {
    fn list(&self) -> Option<&[Self]>;
    fn int(&self) -> Option<i64>;
    fn string(&self) -> Option<String>;
}

pub(crate) trait TreeBuild: Sized {
    fn list(items: Vec<Self>) -> Self;
    fn int(v: i64) -> Self;
    fn string(s: &str) -> Self;
}

pub(crate) const MAX_TABLE_DEPTH: usize = 8;

pub(crate) fn decode<T: TreeNode>(root: &T, what: &'static str) -> Result<LogicalSchema> {
    decode_with(root, what, false)
}

pub(crate) fn decode_legacy_indexes<T: TreeNode>(
    root: &T,
    what: &'static str,
) -> Result<LogicalSchema> {
    decode_with(root, what, true)
}

fn decode_with<T: TreeNode>(
    root: &T,
    what: &'static str,
    legacy_indexes: bool,
) -> Result<LogicalSchema> {
    let d = Dec {
        what,
        legacy_indexes,
        saw_legacy: std::cell::Cell::new(false),
    };
    let header = At::root("header");
    let items = d.list(root, &header, "document")?;
    let [zero, tables] = items else {
        return Err(d.err(
            &header,
            format!(
                "expected {{0,{{N,table...}}}}, got {} elements",
                items.len()
            ),
        ));
    };
    let zero = d.int(zero, &header, "first element")?;
    if zero != 0 {
        return Err(d.err(&header, format!("first element is {zero}, expected 0")));
    }
    let rows = d.counted(tables, &At::root("tables"), "tables")?;
    let tables = rows
        .iter()
        .enumerate()
        .map(|(i, t)| d.table(t, None, "tables", i, 1))
        .collect::<Result<_>>()?;
    if legacy_indexes && !d.saw_legacy.get() {
        return Err(d.err(&header, "no short legacy indexes"));
    }
    Ok(LogicalSchema { tables })
}

pub(crate) fn encode<T: TreeBuild>(s: &LogicalSchema) -> T {
    T::list(vec![T::int(0), counted(s.tables.iter().map(table))])
}

#[derive(Clone, Copy)]
struct At<'a> {
    up: Option<&'a At<'a>>,
    key: &'static str,
    idx: Option<usize>,
    name: Option<&'a str>,
}

impl<'a> At<'a> {
    fn root(key: &'static str) -> At<'a> {
        At {
            up: None,
            key,
            idx: None,
            name: None,
        }
    }

    fn key(&'a self, key: &'static str) -> At<'a> {
        At {
            up: Some(self),
            key,
            idx: None,
            name: None,
        }
    }

    fn item(&'a self, key: &'static str, idx: usize) -> At<'a> {
        At {
            up: Some(self),
            key,
            idx: Some(idx),
            name: None,
        }
    }

    fn named(self, name: &'a str) -> At<'a> {
        At {
            name: Some(name),
            ..self
        }
    }
}

impl fmt::Display for At<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut chain = vec![self];
        let mut cur = self;
        while let Some(up) = cur.up {
            chain.push(up);
            cur = up;
        }
        for (i, a) in chain.iter().rev().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            f.write_str(a.key)?;
            if let Some(idx) = a.idx {
                write!(f, "[{idx}]")?;
            }
            if let Some(name) = a.name {
                write!(f, "({name})")?;
            }
        }
        Ok(())
    }
}

struct Dec {
    what: &'static str,
    legacy_indexes: bool,
    saw_legacy: std::cell::Cell<bool>,
}

impl Dec {
    fn err(&self, at: &At<'_>, why: impl Into<String>) -> Error {
        Error::Parse {
            what: self.what,
            at: at.to_string(),
            why: why.into(),
        }
    }

    fn list<'n, T: TreeNode>(&self, n: &'n T, at: &At<'_>, name: &str) -> Result<&'n [T]> {
        n.list()
            .ok_or_else(|| self.err(at, format!("{name} is not a list")))
    }

    fn counted<'n, T: TreeNode>(&self, n: &'n T, at: &At<'_>, name: &str) -> Result<&'n [T]> {
        let items = self.list(n, at, name)?;
        let Some((count, rest)) = items.split_first() else {
            return Err(self.err(at, format!("{name}: empty list, expected {{n,...}}")));
        };
        let count = count
            .int()
            .ok_or_else(|| self.err(at, format!("{name}: count is not an integer")))?;
        if usize::try_from(count).ok() != Some(rest.len()) {
            return Err(self.err(
                at,
                format!("{name}: count {count} but {} items", rest.len()),
            ));
        }
        Ok(rest)
    }

    fn int<T: TreeNode>(&self, n: &T, at: &At<'_>, name: &str) -> Result<i64> {
        n.int()
            .ok_or_else(|| self.err(at, format!("{name} is not an integer")))
    }

    fn u32<T: TreeNode>(&self, n: &T, at: &At<'_>, name: &str) -> Result<u32> {
        let v = self.int(n, at, name)?;
        u32::try_from(v).map_err(|_| self.err(at, format!("{name} {v} is out of 0..=4294967295")))
    }

    fn flag<T: TreeNode>(&self, n: &T, at: &At<'_>, name: &str) -> Result<bool> {
        match self.int(n, at, name)? {
            0 => Ok(false),
            1 => Ok(true),
            v => Err(self.err(at, format!("{name} {v} is not 0 or 1"))),
        }
    }

    fn string<T: TreeNode>(&self, n: &T, at: &At<'_>, name: &str) -> Result<String> {
        n.string()
            .ok_or_else(|| self.err(at, format!("{name} is not a string")))
    }

    fn named_items<'n, T: TreeNode>(
        &self,
        n: &'n T,
        at: &At<'_>,
        item_kind: &str,
        len: usize,
    ) -> Result<(&'n [T], String)> {
        let items = self.list(n, at, item_kind)?;
        let name = match items.first() {
            Some(x) => self.string(x, at, "name")?,
            None => return Err(self.err(at, format!("expected {len} elements, got 0"))),
        };
        Ok((items, name))
    }

    fn len_err(&self, at: &At<'_>, len: usize, got: usize) -> Error {
        self.err(at, format!("expected {len} elements, got {got}"))
    }

    fn table<T: TreeNode>(
        &self,
        n: &T,
        up: Option<&At<'_>>,
        key: &'static str,
        idx: usize,
        depth: usize,
    ) -> Result<Table> {
        let bare = At {
            up,
            key,
            idx: Some(idx),
            name: None,
        };
        let (items, name) = self.named_items(n, &bare, "table", 14)?;
        let at = bare.named(&name);
        if depth > MAX_TABLE_DEPTH {
            return Err(self.err(
                &at,
                format!("tables nested deeper than {MAX_TABLE_DEPTH} levels"),
            ));
        }
        let [
            _,
            kind,
            number,
            parent,
            fields,
            nested,
            indexes,
            one,
            key_kind,
            spl0,
            spl1,
            tail_s,
            tail_n1,
            tail_n2,
        ] = items
        else {
            return Err(self.len_err(&at, 14, items.len()));
        };
        let kind_s = self.string(kind, &at, "kind")?;
        let kind = TableKind::from_letter(&kind_s)
            .ok_or_else(|| self.err(&at, format!("kind {kind_s:?} is not \"N\" or \"I\"")))?;
        let number = self.u32(number, &at, "number")?;
        let parent = self.string(parent, &at, "parent")?;
        let fields = self
            .counted(fields, &at.key("fields"), "fields")?
            .iter()
            .enumerate()
            .map(|(i, f)| self.field(f, &at, i))
            .collect::<Result<_>>()?;
        let nested = self
            .counted(nested, &at.key("nested"), "nested tables")?
            .iter()
            .enumerate()
            .map(|(i, t)| self.table(t, Some(&at), "nested", i, depth.saturating_add(1)))
            .collect::<Result<_>>()?;
        let indexes = self
            .counted(indexes, &at.key("indexes"), "indexes")?
            .iter()
            .enumerate()
            .map(|(i, x)| self.index(x, &at, i))
            .collect::<Result<_>>()?;
        let one = self.int(one, &at, "one")?;
        let key_s = self.string(key_kind, &at, "key")?;
        let key = KeyKind::from_letter(&key_s)
            .ok_or_else(|| self.err(&at, format!("key {key_s:?} is not \"R\" or \"S\"")))?;
        let splitters = [
            self.splitter(spl0, &at.item("splitters", 0))?,
            self.splitter(spl1, &at.item("splitters", 1))?,
        ];
        let tail_s = self.string(tail_s, &at, "tail string")?;
        let tail_n1 = self.int(tail_n1, &at, "tail number 1")?;
        let tail_n2 = self.int(tail_n2, &at, "tail number 2")?;
        Ok(Table {
            name,
            kind,
            number,
            parent,
            fields,
            nested,
            indexes,
            one,
            key,
            splitters,
            tail_s,
            tail_n1,
            tail_n2,
        })
    }

    fn field<T: TreeNode>(&self, n: &T, up: &At<'_>, idx: usize) -> Result<Field> {
        let bare = up.item("fields", idx);
        let (items, name) = self.named_items(n, &bare, "field", 5)?;
        let at = bare.named(&name);
        let [_, nullable, types, tail_s, tail_n] = items else {
            return Err(self.len_err(&at, 5, items.len()));
        };
        let nullable = self.flag(nullable, &at, "nullable")?;
        let types = self
            .counted(types, &at.key("types"), "types")?
            .iter()
            .enumerate()
            .map(|(i, t)| self.ltype(t, &at.item("types", i)))
            .collect::<Result<_>>()?;
        let tail_s = self.string(tail_s, &at, "tail string")?;
        let tail_n = self.int(tail_n, &at, "tail number")?;
        Ok(Field {
            name,
            nullable,
            types,
            tail_s,
            tail_n,
        })
    }

    fn ltype<T: TreeNode>(&self, n: &T, at: &At<'_>) -> Result<LType> {
        let items = self.list(n, at, "type")?;
        let (code, length, precision, ref_table, ref_kind, extra) = match items {
            [c, l, p, t, k] => (c, l, p, t, k, None),
            [c, l, p, t, k, x] => (c, l, p, t, k, Some(x)),
            _ => {
                return Err(self.err(at, format!("expected 5 or 6 elements, got {}", items.len())));
            }
        };
        let code_s = self.string(code, at, "code")?;
        let code = TypeCode::from_letter(&code_s)
            .ok_or_else(|| self.err(at, format!("type code {code_s:?} is not one of BLNSTREV")))?;
        Ok(LType {
            code,
            length: self.u32(length, at, "length")?,
            precision: self.u32(precision, at, "precision")?,
            ref_table: self.string(ref_table, at, "reference table")?,
            ref_kind: self.u32(ref_kind, at, "reference kind")?,
            extra: extra
                .map(|x| self.int(x, at, "sixth element"))
                .transpose()?,
        })
    }

    fn index<T: TreeNode>(&self, n: &T, up: &At<'_>, idx: usize) -> Result<Index> {
        let bare = up.item("indexes", idx);
        let (items, name) = self.named_items(n, &bare, "index", 9)?;
        let at = bare.named(&name);
        let (unique, fields, clustered, flag2, n3, tail) = match items {
            [_, unique, fields, clustered, flag2, n3] if self.legacy_indexes => {
                self.saw_legacy.set(true);
                (unique, fields, clustered, flag2, n3, None)
            }
            [_, unique, fields, clustered, flag2, n3, list, n4, n5] if !self.legacy_indexes => {
                (unique, fields, clustered, flag2, n3, Some((list, n4, n5)))
            }
            _ => {
                return Err(self.len_err(
                    &at,
                    if self.legacy_indexes { 6 } else { 9 },
                    items.len(),
                ));
            }
        };
        let unique = self.flag(unique, &at, "unique")?;
        let fields = self
            .counted(fields, &at.key("fields"), "index fields")?
            .iter()
            .enumerate()
            .map(|(i, f)| self.string(f, &at.item("fields", i), "field name"))
            .collect::<Result<_>>()?;
        let clustered = self.flag(clustered, &at, "clustered")?;
        let flag2 = self.flag(flag2, &at, "flag2")?;
        let n3 = self.int(n3, &at, "n3")?;
        let (list, n4, n5) = if let Some((list, n4, n5)) = tail {
            let list = self
                .list(list, &at.key("list"), "list")?
                .iter()
                .enumerate()
                .map(|(i, x)| self.int(x, &at.item("list", i), "list item"))
                .collect::<Result<_>>()?;
            (list, self.int(n4, &at, "n4")?, self.int(n5, &at, "n5")?)
        } else {
            if n3 != 0 {
                return Err(self.err(&at, "unverified legacy index flag"));
            }
            (vec![0], 0, 0)
        };
        Ok(Index {
            name,
            unique,
            fields,
            clustered,
            flag2,
            n3,
            list,
            n4,
            n5,
        })
    }

    fn splitter<T: TreeNode>(&self, n: &T, at: &At<'_>) -> Result<Vec<Vec<String>>> {
        let items = self.list(n, at, "splitter")?;
        match items {
            [c] => match self.int(c, at, "group count")? {
                0 => Ok(Vec::new()),
                c => Err(self.err(at, format!("group count {c} without a group list"))),
            },
            [c, groups] => {
                let c = self.int(c, at, "group count")?;
                let groups = self.list(groups, at, "group list")?;
                if c == 0 || usize::try_from(c).ok() != Some(groups.len()) {
                    return Err(
                        self.err(at, format!("group count {c} but {} groups", groups.len()))
                    );
                }
                groups
                    .iter()
                    .enumerate()
                    .map(|(i, g)| {
                        let gat = at.item("groups", i);
                        self.counted(g, &gat, "group")?
                            .iter()
                            .map(|f| self.string(f, &gat, "field name"))
                            .collect()
                    })
                    .collect()
            }
            _ => Err(self.err(
                at,
                format!(
                    "expected {{0}} or {{c,{{group...}}}}, got {} elements",
                    items.len()
                ),
            )),
        }
    }
}

fn int_of_len(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn flag<T: TreeBuild>(b: bool) -> T {
    T::int(i64::from(b))
}

fn counted<T: TreeBuild>(items: impl ExactSizeIterator<Item = T>) -> T {
    let mut out = Vec::with_capacity(items.len().saturating_add(1));
    out.push(T::int(int_of_len(items.len())));
    out.extend(items);
    T::list(out)
}

fn table<T: TreeBuild>(t: &Table) -> T {
    let [s0, s1] = &t.splitters;
    T::list(vec![
        T::string(&t.name),
        T::string(t.kind.letter()),
        T::int(i64::from(t.number)),
        T::string(&t.parent),
        counted(t.fields.iter().map(field)),
        counted(t.nested.iter().map(table)),
        counted(t.indexes.iter().map(index)),
        T::int(t.one),
        T::string(t.key.letter()),
        splitter(s0),
        splitter(s1),
        T::string(&t.tail_s),
        T::int(t.tail_n1),
        T::int(t.tail_n2),
    ])
}

fn field<T: TreeBuild>(f: &Field) -> T {
    T::list(vec![
        T::string(&f.name),
        flag(f.nullable),
        counted(f.types.iter().map(ltype)),
        T::string(&f.tail_s),
        T::int(f.tail_n),
    ])
}

fn ltype<T: TreeBuild>(t: &LType) -> T {
    let mut items = vec![
        T::string(t.code.letter()),
        T::int(i64::from(t.length)),
        T::int(i64::from(t.precision)),
        T::string(&t.ref_table),
        T::int(i64::from(t.ref_kind)),
    ];
    items.extend(t.extra.map(T::int));
    T::list(items)
}

fn index<T: TreeBuild>(i: &Index) -> T {
    T::list(vec![
        T::string(&i.name),
        flag(i.unique),
        counted(i.fields.iter().map(|f| T::string(f))),
        flag(i.clustered),
        flag(i.flag2),
        T::int(i.n3),
        T::list(i.list.iter().map(|&v| T::int(v)).collect()),
        T::int(i.n4),
        T::int(i.n5),
    ])
}

fn splitter<T: TreeBuild>(groups: &[Vec<String>]) -> T {
    if groups.is_empty() {
        return T::list(vec![T::int(0)]);
    }
    let list = groups
        .iter()
        .map(|g| counted(g.iter().map(|f| T::string(f))))
        .collect();
    T::list(vec![T::int(int_of_len(groups.len())), T::list(list)])
}
