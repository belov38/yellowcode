use std::collections::HashSet;

use crate::value::{TypeName, Value, ValueError, fold_key, is_structure_key};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AllowedSign {
    Any,
    Nonnegative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AllowedLength {
    Variable,
    Fixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DateFractions {
    Date,
    Time,
    DateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NumberQualifiers {
    pub digits: u32,
    pub fraction_digits: u32,
    pub sign: AllowedSign,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StringQualifiers {
    pub length: u32,
    pub allowed: AllowedLength,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DateQualifiers {
    pub fractions: DateFractions,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct TypeDescription {
    pub types: Vec<TypeName>,
    pub number: Option<NumberQualifiers>,
    pub string: Option<StringQualifiers>,
    pub date: Option<DateQualifiers>,
}

impl TypeDescription {
    pub fn any() -> TypeDescription {
        TypeDescription::default()
    }

    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
            && self.number.is_none()
            && self.string.is_none()
            && self.date.is_none()
    }

    pub fn single(&self) -> Option<&TypeName> {
        match self.types.as_slice() {
            [t] => Some(t),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Column {
    pub name: String,
    pub value_type: TypeDescription,
    pub title: String,
    pub width: u64,
}

impl Column {
    pub fn new(name: &str) -> Column {
        Column {
            name: name.to_string(),
            value_type: TypeDescription::any(),
            title: String::new(),
            width: 0,
        }
    }

    pub fn typed(name: &str, value_type: TypeDescription) -> Column {
        Column {
            value_type,
            ..Column::new(name)
        }
    }
}

fn check_columns(columns: &[Column]) -> Result<(), ValueError> {
    let mut keys = HashSet::with_capacity(columns.len());
    for c in columns {
        if !is_structure_key(&c.name) {
            return Err(ValueError::ColumnName(c.name.clone()));
        }
        if !keys.insert(fold_key(&c.name)) {
            return Err(ValueError::RepeatedColumn(c.name.clone()));
        }
    }
    Ok(())
}

fn position(columns: &[Column], name: &str) -> Option<usize> {
    let key = fold_key(name);
    columns.iter().position(|c| fold_key(&c.name) == key)
}

fn check_width(columns: &[Column], values: &[Value]) -> Result<(), ValueError> {
    if values.len() != columns.len() {
        return Err(ValueError::RowWidth {
            columns: columns.len(),
            values: values.len(),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ValueTable {
    columns: Vec<Column>,
    indexes: Vec<Vec<String>>,
    rows: Vec<Vec<Value>>,
}

impl ValueTable {
    pub fn new(columns: Vec<Column>) -> Result<ValueTable, ValueError> {
        check_columns(&columns)?;
        Ok(ValueTable {
            columns,
            indexes: Vec::new(),
            rows: Vec::new(),
        })
    }

    pub fn add_index(&mut self, names: Vec<String>) -> Result<(), ValueError> {
        if names.is_empty() {
            return Err(ValueError::EmptyIndex);
        }
        let mut seen = Vec::with_capacity(names.len());
        for name in &names {
            match position(&self.columns, name) {
                Some(i) if !seen.contains(&i) => seen.push(i),
                _ => return Err(ValueError::IndexColumn(name.clone())),
            }
        }
        self.indexes.push(names);
        Ok(())
    }

    pub fn push_row(&mut self, values: Vec<Value>) -> Result<(), ValueError> {
        check_width(&self.columns, &values)?;
        self.rows.push(values);
        Ok(())
    }

    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    pub fn indexes(&self) -> &[Vec<String>] {
        &self.indexes
    }

    pub fn rows(&self) -> &[Vec<Value>] {
        &self.rows
    }

    pub fn column(&self, name: &str) -> Option<usize> {
        position(&self.columns, name)
    }

    pub fn is_empty(&self) -> bool {
        self.columns.is_empty() && self.indexes.is_empty() && self.rows.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct TreeRow {
    values: Vec<Value>,
    rows: Vec<TreeRow>,
}

impl TreeRow {
    pub fn new(values: Vec<Value>) -> TreeRow {
        TreeRow {
            values,
            rows: Vec::new(),
        }
    }

    pub fn push_child(&mut self, row: TreeRow) {
        self.rows.push(row);
    }

    pub fn values(&self) -> &[Value] {
        &self.values
    }

    pub fn rows(&self) -> &[TreeRow] {
        &self.rows
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ValueTree {
    columns: Vec<Column>,
    rows: Vec<TreeRow>,
}

impl ValueTree {
    pub fn new(columns: Vec<Column>) -> Result<ValueTree, ValueError> {
        check_columns(&columns)?;
        Ok(ValueTree {
            columns,
            rows: Vec::new(),
        })
    }

    pub fn push_row(&mut self, row: TreeRow) -> Result<(), ValueError> {
        let mut stack = vec![&row];
        while let Some(r) = stack.pop() {
            check_width(&self.columns, &r.values)?;
            stack.extend(&r.rows);
        }
        self.rows.push(row);
        Ok(())
    }

    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    pub fn rows(&self) -> &[TreeRow] {
        &self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.columns.is_empty() && self.rows.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Picture {
    pub t: Option<String>,
    pub reference: Option<TypeName>,
}

pub const NS_UI: &str = "http://v8.1c.ru/8.1/data/ui";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ListItem {
    pub value: Value,
    pub presentation: String,
    pub check: bool,
    pub picture: Option<Picture>,
    id: u64,
}

impl ListItem {
    pub fn new(value: Value) -> ListItem {
        ListItem {
            value,
            presentation: String::new(),
            check: false,
            picture: None,
            id: 0,
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }
}

#[derive(Debug, Clone)]
pub struct ValueList {
    pub value_type: TypeDescription,
    items: Vec<ListItem>,
    ids: HashSet<u64>,
    last_id: i64,
}

impl PartialEq for ValueList {
    fn eq(&self, other: &ValueList) -> bool {
        self.value_type == other.value_type
            && self.items == other.items
            && self.last_id == other.last_id
    }
}

impl Eq for ValueList {}

impl std::hash::Hash for ValueList {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.value_type.hash(h);
        self.items.hash(h);
        self.last_id.hash(h);
    }
}

impl Default for ValueList {
    fn default() -> ValueList {
        ValueList::new(TypeDescription::any())
    }
}

impl ValueList {
    pub fn new(value_type: TypeDescription) -> ValueList {
        ValueList {
            value_type,
            items: Vec::new(),
            ids: HashSet::new(),
            last_id: -1,
        }
    }

    pub fn push(&mut self, item: ListItem) -> Result<(), ValueError> {
        let next = self
            .last_id
            .checked_add(1)
            .filter(|n| *n < i64::MAX)
            .ok_or(ValueError::ListIdsExhausted(self.last_id))?;
        self.push_with_id(item, next as u64)
    }

    pub fn push_with_id(&mut self, mut item: ListItem, id: u64) -> Result<(), ValueError> {
        let as_last = i64::try_from(id)
            .ok()
            .filter(|n| *n < i64::MAX)
            .ok_or(ValueError::LastId(i64::MAX))?;
        if !self.ids.insert(id) {
            return Err(ValueError::RepeatedListId(id));
        }
        item.id = id;
        self.last_id = self.last_id.max(as_last);
        self.items.push(item);
        Ok(())
    }

    pub fn set_last_id(&mut self, last_id: i64) -> Result<(), ValueError> {
        let max = self.ids.iter().map(|&i| i as i64).max().unwrap_or(-1);
        if last_id < max || last_id == i64::MAX {
            return Err(ValueError::LastId(last_id));
        }
        self.last_id = last_id;
        Ok(())
    }

    pub fn items(&self) -> &[ListItem] {
        &self.items
    }

    pub fn last_id(&self) -> i64 {
        self.last_id
    }
}
