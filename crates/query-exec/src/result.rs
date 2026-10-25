use query_ir::ColumnInfo;

use crate::source::Row;
use crate::totals::TotalsTree;

#[derive(Clone, Debug, PartialEq)]
pub struct QueryResult {
    pub columns: Vec<ColumnInfo>,
    pub rows: Vec<Row>,
    pub totals: Option<TotalsTree>,
}

impl QueryResult {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn linear(&self) -> std::slice::Iter<'_, Row> {
        self.rows.iter()
    }
}
