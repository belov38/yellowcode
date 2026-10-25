use super::index::{GuidText, doc_tree, text};
use super::{GuidMap, ManagedIndex};
use crate::predefined::{iter_children, ref_name_columns, ref_uuid, table_suffix};
use crate::{ManagedEntries, ManagedEntriesError, ManagedEntrySource};
use brace::{Guid, Node};
use mdschema::MdKind;
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, MemoryBudget};

const CATEGORY: Category = Category::Metadata;
macro_rules! present {
    ($value:expr) => {
        match $value {
            Some(value) => value,
            None => return Ok(None),
        }
    };
}
fn num(node: &Node) -> Option<usize> {
    node.text()?.trim().parse().ok()
}
#[derive(Debug)]
pub struct ManagedPredefColumn<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub pattern: &'a Node,
}
#[derive(Debug, Clone, Copy)]
pub enum ManagedPredefBody<'a> {
    Tree(&'a Node),
    Flat(&'a [Node]),
}
#[derive(Debug)]
pub struct ManagedPredefTable<'a> {
    pub kind_no: u32,
    pub columns: BudgetedVec<ManagedPredefColumn<'a>>,
    pub positions: BudgetedVec<Option<&'a str>>,
    pub body: ManagedPredefBody<'a>,
}
impl<'a> ManagedPredefTable<'a> {
    pub fn read(tree: &'a Node, budget: &MemoryBudget) -> Result<Option<Self>, BudgetError> {
        let kind_no = present!(tree.at(0).and_then(num).and_then(|n| u32::try_from(n).ok()));
        let flat = kind_no == 9;
        let body = if flat { tree } else { present!(tree.at(1)) };
        let cols = present!(body.at(1).and_then(Node::as_list));
        let count = present!(cols.first().and_then(num));
        let cols = present!(cols.get(1..));
        let mut columns = BudgetedVec::try_with_capacity(budget, CATEGORY, count.min(cols.len()))?;
        for col in cols.iter().take(count) {
            columns.try_push(ManagedPredefColumn {
                id: present!(col.at(0).and_then(Node::text)),
                name: present!(col.at(1).and_then(Node::text)),
                pattern: present!(col.at(2)),
            })?;
        }
        let tb = present!(body.at(2).and_then(Node::as_list));
        let n = present!(tb.get(1).and_then(num));
        if n > tb.len().saturating_sub(2) / 2 {
            return Ok(None);
        }
        let mut positions = BudgetedVec::try_with_capacity(budget, CATEGORY, n)?;
        positions.try_resize(n, None)?;
        for i in 0..n {
            let p = present!(tb.get(2 + 2 * i).and_then(num));
            let id = present!(tb.get(3 + 2 * i).and_then(Node::text));
            *present!(positions.get_mut(p)) = Some(id);
        }
        if positions.iter().any(Option::is_none) {
            return Ok(None);
        }
        let rows = present!(tb.get(2 + 2 * n));
        let body = if flat {
            let rows = present!(rows.as_list());
            let count = present!(rows.get(1).and_then(num));
            let rows = present!(rows.get(2..));
            ManagedPredefBody::Flat(present!(rows.get(..count.min(rows.len()))))
        } else {
            ManagedPredefBody::Tree(present!(rows.at(2)))
        };
        Ok(Some(Self {
            kind_no,
            columns,
            positions,
            body,
        }))
    }
}
#[derive(Debug)]
pub struct ManagedPredefined {
    budget: MemoryBudget,
    names: GuidMap<BudgetedText>,
}
impl ManagedPredefined {
    pub fn new(budget: &MemoryBudget) -> Result<Self, BudgetError> {
        Ok(Self {
            budget: budget.clone(),
            names: GuidMap::new(budget)?,
        })
    }
    pub fn memory_budget(&self) -> &MemoryBudget {
        &self.budget
    }
    pub fn len(&self) -> usize {
        self.names.len()
    }
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
    pub fn get(&self, id: &Guid) -> Option<&str> {
        self.names.get(id).map(BudgetedText::as_str)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&Guid, &str)> {
        self.names.iter().map(|(id, name)| (id, name.as_str()))
    }
    pub fn build<S: ManagedEntrySource + ?Sized>(
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
    ) -> Result<Self, ManagedEntriesError<S::Error>> {
        if !index.owns_scope(entries.memory_budget()) {
            return Err(ManagedEntriesError::Inconsistent(
                "predefined index has unrelated memory ownership",
            ));
        }
        let mut out = Self::new(entries.memory_budget())?;
        for id in index.order.iter() {
            let Some((kind, _)) = index.objs.get(id) else {
                continue;
            };
            let Some(suffix) = table_suffix(*kind) else {
                continue;
            };
            let id = GuidText::new(*id);
            let entry = text(entries.memory_budget(), &[id.as_str()?, ".", suffix])?;
            let data = match entries.doc(entry.as_str()) {
                Ok(Some(data)) => data,
                Ok(None) => continue,
                Err(error) if entries.recoverable_content_error(&error) => continue,
                Err(error) => return Err(error),
            };
            if let Some(tree) = doc_tree(&data) {
                out.index_table(*kind, tree)?;
            }
        }
        Ok(out)
    }
    pub fn index_table(&mut self, kind: MdKind, tree: &Node) -> Result<(), BudgetError> {
        if table_suffix(kind).is_none() {
            return Ok(());
        }
        let Some(table) = ManagedPredefTable::read(tree, &self.budget)? else {
            return Ok(());
        };
        let Some(cols) = ref_name_columns(table.kind_no) else {
            return Ok(());
        };
        match table.body {
            ManagedPredefBody::Flat(rows) => {
                for row in rows {
                    if !self.add_item(row, &table.positions, cols)? {
                        break;
                    }
                }
            }
            ManagedPredefBody::Tree(root) => {
                let Some(children) = iter_children(root) else {
                    return Ok(());
                };
                let mut stack = BudgetedVec::new(&self.budget, CATEGORY)?;
                stack.try_push(children.iter())?;
                while let Some(top) = stack.last_mut() {
                    let Some(row) = top.next() else {
                        stack.pop();
                        continue;
                    };
                    if !self.add_item(row, &table.positions, cols)? {
                        break;
                    }
                    let Some(children) = iter_children(row) else {
                        break;
                    };
                    if !children.is_empty() {
                        stack.try_push(children.iter())?;
                    }
                }
            }
        }
        Ok(())
    }
    fn add_item(
        &mut self,
        row: &Node,
        positions: &[Option<&str>],
        cols: (&str, &str),
    ) -> Result<bool, BudgetError> {
        let Some((id, name)) = item(row, positions, cols) else {
            return Ok(false);
        };
        self.names.insert(id, text(&self.budget, &[name])?)?;
        Ok(true)
    }
}
fn item<'a>(
    row: &'a Node,
    positions: &[Option<&str>],
    (rid, nid): (&str, &str),
) -> Option<(Guid, &'a str)> {
    let row = row.as_list()?;
    let n = num(row.get(2)?)?;
    let values = row.get(3..)?.get(..n)?;
    let positions = positions.get(..n)?;
    let cell = |id: &str| {
        positions
            .iter()
            .rposition(|p| *p == Some(id))
            .and_then(|i| values.get(i))
    };
    let id = ref_uuid(cell(rid)?)?;
    let value = cell(nid)?;
    let name = match value.at(0)?.text()? {
        "S" => value.at(1).and_then(Node::text).unwrap_or(""),
        "N" => value.at(1)?.text()?,
        "B" => {
            if value.at(1)?.text()? == "1" {
                "true"
            } else {
                "false"
            }
        }
        _ => return None,
    };
    Some((id, name.rsplit('.').next().unwrap_or("")))
}
