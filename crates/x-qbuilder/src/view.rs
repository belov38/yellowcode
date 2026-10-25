use crate::session::{ErrorOut, Session};
use bsl_prim::fold_name;
use query_builder::available::{self, AvailableField, AvailableTable, TableGroup};
use query_builder::model::{Source, Stmt, Table};
use query_builder::ops::{OpError, QueryRef};
use query_ir::{DateQ, QType, QuerySchema};
use query_syntax::ScriptVariant;
use serde::Serialize;
use std::collections::HashMap;

pub use query_builder::view::{MAX_ROWS, MAX_TEXT, View};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AvailableOut {
    pub groups: Vec<GroupOut>,
    pub temp: Vec<TableOut>,
    pub fields: Vec<SourceFieldsOut>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GroupOut {
    pub presentation: String,
    pub tables: Vec<StubOut>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StubOut {
    pub name: String,
    pub fields: usize,
    pub children: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TableOut {
    pub name: String,
    pub id: Option<u32>,
    pub temp: bool,
    pub params: Vec<ParamOut>,
    pub fields: Vec<FieldOut>,
    pub children: Vec<StubOut>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ParamOut {
    pub name: String,
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FieldOut {
    pub name: String,
    pub ty: String,
    pub dimension: bool,
    pub resource: bool,
    pub nested: Option<u32>,
    pub refs: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SourceFieldsOut {
    pub alias: String,
    pub table: String,
    pub found: bool,
    pub fields: Vec<FieldOut>,
}

pub(crate) struct Catalog {
    groups: Vec<TableGroup>,
    index: HashMap<String, Vec<usize>>,
}

impl Catalog {
    pub(crate) fn build(qs: &QuerySchema) -> Catalog {
        let groups = available::database(qs);
        let mut index = HashMap::new();
        for (g, group) in groups.iter().enumerate() {
            for (t, tab) in group.tables.iter().enumerate() {
                walk(tab, vec![g, t], &mut index);
            }
        }
        Catalog { groups, index }
    }

    fn find(&self, name: &str) -> Option<&AvailableTable> {
        let way = self.index.get(&fold_name(name))?;
        let (g, rest) = way.split_first()?;
        let (t, kids) = rest.split_first()?;
        let mut cur = self.groups.get(*g)?.tables.get(*t)?;
        for k in kids {
            cur = cur.children.get(*k)?;
        }
        Some(cur)
    }
}

fn walk(t: &AvailableTable, way: Vec<usize>, index: &mut HashMap<String, Vec<usize>>) {
    index
        .entry(fold_name(&t.name))
        .or_insert_with(|| way.clone());
    for (k, c) in t.children.iter().enumerate() {
        let mut w = way.clone();
        w.push(k);
        walk(c, w, index);
    }
}

fn stub(t: &AvailableTable) -> StubOut {
    StubOut {
        name: t.name.clone(),
        fields: t.fields.len(),
        children: t.children.len(),
    }
}

fn table_out(qs: &QuerySchema, t: &AvailableTable) -> TableOut {
    TableOut {
        name: t.name.clone(),
        id: t.id.map(|i| i.0),
        temp: t.id.is_none(),
        params: t
            .params
            .iter()
            .map(|p| ParamOut {
                name: p.name.clone(),
                kind: format!("{:?}", p.kind),
            })
            .collect(),
        fields: t.fields.iter().map(|f| field_out(qs, f)).collect(),
        children: t.children.iter().map(stub).collect(),
    }
}

fn field_out(qs: &QuerySchema, f: &AvailableField) -> FieldOut {
    FieldOut {
        name: f.name.clone(),
        ty: f
            .ty
            .as_ref()
            .map(|t| query_builder::view::cut(type_text(qs, t)))
            .unwrap_or_default(),
        dimension: f.dimension,
        resource: f.resource,
        nested: f.nested.map(|n| n.0),
        refs: f.ty.as_ref().is_some_and(|t| !t.refs.is_empty()),
    }
}

fn type_text(qs: &QuerySchema, t: &QType) -> String {
    let ru = qs.script() == ScriptVariant::Russian;
    let w = |r: &'static str, e: &'static str| if ru { r } else { e };
    let mut out: Vec<String> = Vec::new();
    if t.null {
        out.push("Null".into());
    }
    if t.boolean {
        out.push(w("Булево", "Boolean").into());
    }
    if let Some(n) = t.number {
        out.push(format!(
            "{}({},{})",
            w("Число", "Number"),
            n.digits,
            n.scale
        ));
    }
    if let Some(s) = t.string {
        let name = w("Строка", "String");
        out.push(if s.len == 0 {
            name.to_string()
        } else {
            format!("{name}({})", s.len)
        });
    }
    if let Some(d) = t.date {
        out.push(
            match d {
                DateQ::Date => w("Дата", "Date"),
                DateQ::Time => w("Время", "Time"),
                DateQ::DateTime => w("ДатаВремя", "DateTime"),
            }
            .into(),
        );
    }
    for r in &t.refs {
        out.push(qs.ref_info(*r).xml_name.clone());
    }
    for o in &t.others {
        out.push(format!("{o:?}"));
    }
    out.join(", ")
}

fn op_err(e: OpError) -> ErrorOut {
    ErrorOut::from(&e)
}

impl Session {
    pub fn view(&self, stmt: usize, op_index: usize) -> Result<View, ErrorOut> {
        self.view_nested(
            &QueryRef {
                stmt,
                nested: Vec::new(),
            },
            op_index,
        )
    }

    pub fn view_nested(&self, q: &QueryRef, op_index: usize) -> Result<View, ErrorOut> {
        query_builder::view::view(self.schema(), q, op_index).map_err(op_err)
    }

    fn catalog(&self) -> &Catalog {
        self.catalog.get_or_init(|| Catalog::build(&self.qs))
    }

    fn temp_before(&self, stmt: usize) -> Vec<AvailableTable> {
        let ctx = query_builder::Ctx {
            schema: &self.qs,
            temp: &query_ir::NoTempTables,
        };
        match query_builder::open(&self.text(), self.mode(), &ctx) {
            Ok(o) => available::temp_tables(&o, stmt),
            Err(_) => Vec::new(),
        }
    }

    pub fn available(&self, stmt: usize, op_index: usize) -> Result<AvailableOut, ErrorOut> {
        let Some(Stmt::Select(sq)) = self.schema().batch.get(stmt) else {
            return Err(op_err(match self.schema().batch.get(stmt) {
                Some(_) => OpError::NotSelect(stmt),
                None => OpError::NoStmt(stmt),
            }));
        };
        let op = sq
            .operators
            .get(op_index)
            .ok_or_else(|| op_err(OpError::NoOperator(op_index)))?;
        let cat = self.catalog();
        let temp = self.temp_before(stmt);
        let mut fields = Vec::new();
        for s in &op.sources {
            self.source_fields(s, cat, &temp, &mut fields);
        }
        Ok(AvailableOut {
            groups: cat
                .groups
                .iter()
                .map(|g| GroupOut {
                    presentation: g.presentation.clone(),
                    tables: g.tables.iter().map(stub).collect(),
                })
                .collect(),
            temp: temp.iter().map(|t| table_out(&self.qs, t)).collect(),
            fields,
        })
    }

    fn source_fields(
        &self,
        s: &Source,
        cat: &Catalog,
        temp: &[AvailableTable],
        out: &mut Vec<SourceFieldsOut>,
    ) {
        let (table, found, fields) = match &s.table {
            Table::Named { path, .. } => {
                let name = path
                    .parts
                    .iter()
                    .map(|p| p.text.as_ref())
                    .collect::<Vec<_>>()
                    .join(".");
                let hit = if s.table.is_temp() {
                    let key = fold_name(&name);
                    temp.iter().rev().find(|t| fold_name(&t.name) == key)
                } else {
                    cat.find(&name)
                };
                let fields = hit
                    .map(|t| t.fields.iter().map(|f| field_out(&self.qs, f)).collect())
                    .unwrap_or_default();
                (name, hit.is_some(), fields)
            }
            Table::Subquery(_) => ("(…)".to_string(), false, Vec::new()),
            Table::Param(p) => (format!("&{}", p.name.text), false, Vec::new()),
        };
        let alias = s
            .alias
            .as_ref()
            .map(|a| a.name.text.to_string())
            .unwrap_or_else(|| table.rsplit('.').next().unwrap_or_default().to_string());
        out.push(SourceFieldsOut {
            alias,
            table,
            found,
            fields,
        });
        for j in &s.joins {
            self.source_fields(&j.right, cat, temp, out);
        }
    }

    pub fn table(&self, name: &str, stmt: usize) -> Option<TableOut> {
        if let Some(t) = self.catalog().find(name) {
            return Some(table_out(&self.qs, t));
        }
        let key = fold_name(name);
        self.temp_before(stmt)
            .iter()
            .rev()
            .find(|t| fold_name(&t.name) == key)
            .map(|t| table_out(&self.qs, t))
    }

    pub fn ref_fields(
        &self,
        table: &str,
        field_path: &[&str],
        stmt: usize,
    ) -> Option<Vec<FieldOut>> {
        if field_path.is_empty() {
            return None;
        }
        let cat = self.catalog();
        let temp;
        let t = match cat.find(table) {
            Some(t) => t,
            None => {
                temp = self.temp_before(stmt);
                let key = fold_name(table);
                temp.iter().rev().find(|t| fold_name(&t.name) == key)?
            }
        };
        let mut fields: Vec<AvailableField> = t.fields.clone();
        for name in field_path {
            let key = fold_name(name);
            let f = fields.iter().find(|f| fold_name(&f.name) == key)?;
            fields = available::ref_fields(&self.qs, f.ty.as_ref()?);
        }
        Some(fields.iter().map(|f| field_out(&self.qs, f)).collect())
    }
}
