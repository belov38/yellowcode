use super::{Field, Index, KeyKind, LType, LogicalSchema, Table, TableKind, TypeCode};
use crate::{Result, managed::Build};
use brace::Node;
use memory_budget::{BudgetedVec, Category, Charged, MemoryBudget};

impl LogicalSchema {
    pub fn from_text_budgeted(bytes: &[u8], budget: &MemoryBudget) -> Result<Charged<Self>> {
        let root = brace::parse_bytes_budgeted(bytes, budget, Category::Metadata)?;
        let decoder = Decode {
            build: Build {
                budget,
                what: "DBSCHEMA",
            },
        };
        let build = &decoder.build;
        let [zero, tables] = build.list(&root)? else {
            return Err(build.bad("expected {0,{n,tables...}}"));
        };
        if build.integer(zero)? != 0 {
            return Err(build.bad("schema header is not zero"));
        }
        let tables = build.vector(build.counted(tables)?, |node| decoder.table(node, 1))?;
        Ok(tables.map(|tables| Self { tables }))
    }
}
struct Decode<'a> {
    build: Build<'a>,
}
impl Decode<'_> {
    fn table(&self, node: &Node, depth: usize) -> Result<Charged<Table>> {
        let b = &self.build;
        if depth > super::tree::MAX_TABLE_DEPTH {
            return Err(b.bad("tables nested deeper than eight levels"));
        }
        let [
            name,
            kind,
            number,
            parent,
            fields,
            nested,
            indexes,
            one,
            key,
            spl0,
            spl1,
            tail_s,
            tail_n1,
            tail_n2,
        ] = b.list(node)?
        else {
            return Err(b.bad("table must contain fourteen elements"));
        };
        let kind = TableKind::from_letter(b.str(kind)?)
            .ok_or_else(|| b.bad("table kind is not N or I"))?;
        let number = b.uint(number)?;
        let one = b.integer(one)?;
        let key =
            KeyKind::from_letter(b.str(key)?).ok_or_else(|| b.bad("key kind is not R or S"))?;
        let tail_n1 = b.integer(tail_n1)?;
        let tail_n2 = b.integer(tail_n2)?;
        let mut output = b.string(name)?.map(|name| Table {
            name,
            kind,
            number,
            parent: String::new(),
            fields: Vec::new(),
            nested: Vec::new(),
            indexes: Vec::new(),
            one,
            key,
            splitters: [Vec::new(), Vec::new()],
            tail_s: String::new(),
            tail_n1,
            tail_n2,
        });
        output = output
            .try_zip(b.string(parent)?)?
            .map(|(mut table, parent)| {
                table.parent = parent;
                table
            });
        let fields = b.vector(b.counted(fields)?, |node| self.field(node))?;
        output = output.try_zip(fields)?.map(|(mut table, fields)| {
            table.fields = fields;
            table
        });
        let nested = b.vector(b.counted(nested)?, |node| self.table(node, depth + 1))?;
        output = output.try_zip(nested)?.map(|(mut table, nested)| {
            table.nested = nested;
            table
        });
        let indexes = b.vector(b.counted(indexes)?, |node| self.index(node))?;
        output = output.try_zip(indexes)?.map(|(mut table, indexes)| {
            table.indexes = indexes;
            table
        });
        let splitters = self.splitter(spl0)?.try_zip(self.splitter(spl1)?)?;
        output = output
            .try_zip(splitters)?
            .map(|(mut table, (first, second))| {
                table.splitters = [first, second];
                table
            });
        Ok(output
            .try_zip(b.string(tail_s)?)?
            .map(|(mut table, tail_s)| {
                table.tail_s = tail_s;
                table
            }))
    }
    fn field(&self, node: &Node) -> Result<Charged<Field>> {
        let b = &self.build;
        let [name, nullable, types, tail_s, tail_n] = b.list(node)? else {
            return Err(b.bad("field must contain five elements"));
        };
        let nullable = b.flag(nullable)?;
        let tail_n = b.integer(tail_n)?;
        let mut output = b.string(name)?.map(|name| Field {
            name,
            nullable,
            types: Vec::new(),
            tail_s: String::new(),
            tail_n,
        });
        let types = b.vector(b.counted(types)?, |node| self.ltype(node))?;
        output = output.try_zip(types)?.map(|(mut field, types)| {
            field.types = types;
            field
        });
        Ok(output
            .try_zip(b.string(tail_s)?)?
            .map(|(mut field, tail_s)| {
                field.tail_s = tail_s;
                field
            }))
    }
    fn ltype(&self, node: &Node) -> Result<Charged<LType>> {
        let b = &self.build;
        let (code, length, precision, ref_table, ref_kind, extra) = match b.list(node)? {
            [code, length, precision, ref_table, ref_kind] => {
                (code, length, precision, ref_table, ref_kind, None)
            }
            [code, length, precision, ref_table, ref_kind, extra] => {
                (code, length, precision, ref_table, ref_kind, Some(extra))
            }
            _ => return Err(b.bad("type must contain five or six elements")),
        };
        let code = TypeCode::from_letter(b.str(code)?)
            .ok_or_else(|| b.bad("unknown logical type code"))?;
        let length = b.uint(length)?;
        let precision = b.uint(precision)?;
        let ref_kind = b.uint(ref_kind)?;
        let extra = extra.map(|node| b.integer(node)).transpose()?;
        Ok(b.string(ref_table)?.map(|ref_table| LType {
            code,
            length,
            precision,
            ref_table,
            ref_kind,
            extra,
        }))
    }
    fn index(&self, node: &Node) -> Result<Charged<Index>> {
        let b = &self.build;
        let [name, unique, fields, clustered, flag2, n3, list, n4, n5] = b.list(node)? else {
            return Err(b.bad("index must contain nine elements"));
        };
        let unique = b.flag(unique)?;
        let clustered = b.flag(clustered)?;
        let flag2 = b.flag(flag2)?;
        let n3 = b.integer(n3)?;
        let n4 = b.integer(n4)?;
        let n5 = b.integer(n5)?;
        let mut output = b.string(name)?.map(|name| Index {
            name,
            unique,
            fields: Vec::new(),
            clustered,
            flag2,
            n3,
            list: Vec::new(),
            n4,
            n5,
        });
        let fields = b.vector(b.counted(fields)?, |node| b.string(node))?;
        output = output.try_zip(fields)?.map(|(mut index, fields)| {
            index.fields = fields;
            index
        });
        Ok(output
            .try_zip(b.integers(b.list(list)?)?)?
            .map(|(mut index, list)| {
                index.list = list;
                index
            }))
    }
    fn splitter(&self, node: &Node) -> Result<Charged<Vec<Vec<String>>>> {
        let b = &self.build;
        match b.list(node)? {
            [count] if b.integer(count)? == 0 => {
                Ok(BudgetedVec::new(b.budget, Category::Metadata)?.into_charged_vec())
            }
            [count, groups] => {
                let count = b.integer(count)?;
                let groups = b.list(groups)?;
                if count == 0 || usize::try_from(count).ok() != Some(groups.len()) {
                    return Err(b.bad("splitter count differs from group count"));
                }
                b.vector(groups, |group| {
                    b.vector(b.counted(group)?, |node| b.string(node))
                })
            }
            _ => Err(b.bad("invalid splitter list")),
        }
    }
}
