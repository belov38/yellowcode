use memory_budget::{BudgetError, BudgetedVec, Category, MemoryBudget, TextOutput};
use query_ir::{QType, QValue, uuid_of};
use query_syntax::Lang;
use query_testhost::{TestBase, measure_native_repr, repr_lang, write_native_repr};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CellKind {
    Number,
    String,
    Date,
    Bool,
    Ref,
    Null,
    Undefined,
    Type,
    Binary,
    Nested,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RefOut {
    pub kind: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Cell {
    pub k: CellKind,
    pub t: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r: Option<RefOut>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Column {
    pub name: String,
    pub ty: String,
    pub width: u32,
    pub kind: CellKind,
}

pub(crate) fn column_kind(ty: &QType) -> CellKind {
    let mut kinds = [
        ty.boolean.then_some(CellKind::Bool),
        ty.number.is_some().then_some(CellKind::Number),
        ty.string.is_some().then_some(CellKind::String),
        ty.date.is_some().then_some(CellKind::Date),
        (!ty.refs.is_empty()).then_some(CellKind::Ref),
    ]
    .into_iter()
    .flatten()
    .chain(ty.others.iter().map(|atom| match atom {
        query_ir::Other::Type => CellKind::Type,
        query_ir::Other::Binary => CellKind::Binary,
        query_ir::Other::QueryResult => CellKind::Nested,
        _ => CellKind::Other,
    }));
    match (kinds.next(), kinds.next()) {
        (None, _) if ty.null => CellKind::Null,
        (None, _) => CellKind::Undefined,
        (Some(kind), None) => kind,
        _ => CellKind::Other,
    }
}

fn kind_of(v: &QValue) -> CellKind {
    match v {
        QValue::Null => CellKind::Null,
        QValue::Undefined => CellKind::Undefined,
        QValue::Bool(_) => CellKind::Bool,
        QValue::Number(_) => CellKind::Number,
        QValue::String(_) => CellKind::String,
        QValue::Date(_) => CellKind::Date,
        QValue::Type(_) => CellKind::Type,
        QValue::Binary(_) => CellKind::Binary,
        QValue::Nested(_) => CellKind::Nested,
        QValue::Uuid(_)
        | QValue::Storage(_)
        | QValue::SysEnum(..)
        | QValue::PointInTime(_)
        | QValue::Boundary(_) => CellKind::Other,
        QValue::Ref { .. } => CellKind::Ref,
    }
}

fn ref_out(v: &QValue, base: &TestBase) -> Option<RefOut> {
    let QValue::Ref { table, id } = v else {
        return None;
    };
    Some(RefOut {
        kind: base
            .ref_xml_name(*table)
            .map_or_else(|| format!("Ref#{table}"), str::to_string),
        id: uuid_of(id).to_string(),
    })
}

pub(crate) fn cell(v: &QValue, base: &TestBase, lang: Lang) -> Cell {
    Cell {
        k: kind_of(v),
        t: repr_lang(v, base, lang),
        r: ref_out(v, base),
    }
}

const fn block(n: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    n.div_ceil(16).saturating_mul(16).saturating_add(16)
}

const UUID_TEXT: u64 = 36;

fn ref_out_bytes(v: &QValue, base: &TestBase) -> u64 {
    let QValue::Ref { table, .. } = v else {
        return 0;
    };
    let kind = match base.ref_xml_name(*table) {
        Some(name) => name.len() as u64,
        None => 4 + u64::from(table.checked_ilog10().unwrap_or(0)) + 1,
    };
    block(kind) + block(UUID_TEXT)
}

pub(crate) fn cell_measure(
    v: &QValue,
    base: &TestBase,
    lang: Lang,
    budget: &MemoryBudget,
) -> Result<u64, BudgetError> {
    measure_native_repr(v, base, lang, budget, u64::MAX)
}

pub(crate) fn cell_charge(v: &QValue, base: &TestBase, n: u64) -> u64 {
    block(std::mem::size_of::<Cell>() as u64)
        .saturating_add(block(n))
        .saturating_add(ref_out_bytes(v, base))
}

pub(crate) fn cell_write(
    v: &QValue,
    base: &TestBase,
    lang: Lang,
    budget: &MemoryBudget,
    n: u64,
) -> Result<Cell, BudgetError> {
    let mut text = ExactText::try_new(n)?;
    write_native_repr(v, base, lang, budget, &mut text)?;
    Ok(Cell {
        k: kind_of(v),
        t: text.into_exact(n)?,
        r: ref_out(v, base),
    })
}

pub(crate) struct RowSizes {
    budget: MemoryBudget,
    sizes: Option<BudgetedVec<u64>>,
}

impl RowSizes {
    pub(crate) fn new(budget: &MemoryBudget) -> Self {
        Self {
            budget: budget.clone(),
            sizes: None,
        }
    }

    pub(crate) fn measure(
        &mut self,
        values: &[QValue],
        base: &TestBase,
        lang: Lang,
    ) -> Result<u64, BudgetError> {
        let sizes = match &mut self.sizes {
            Some(sizes) => sizes,
            empty => empty.insert(BudgetedVec::try_with_capacity(
                &self.budget,
                Category::Execution,
                values.len(),
            )?),
        };
        sizes.clear();
        sizes.try_reserve(values.len())?;
        let mut total = 0u64;
        for v in values {
            let n = cell_measure(v, base, lang, &self.budget)?;
            sizes.try_push(n)?;
            total = total.saturating_add(cell_charge(v, base, n));
        }
        Ok(total)
    }

    pub(crate) fn get(&self) -> &[u64] {
        self.sizes.as_ref().map_or(&[], BudgetedVec::as_slice)
    }
}

pub(crate) fn row_write(
    values: &[QValue],
    sizes: &[u64],
    base: &TestBase,
    lang: Lang,
    budget: &MemoryBudget,
    row: &mut Vec<Cell>,
) -> Result<(), BudgetError> {
    for (v, &n) in values.iter().zip(sizes) {
        row.push(cell_write(v, base, lang, budget, n)?);
    }
    Ok(())
}

struct ExactText {
    text: String,
    error: Option<BudgetError>,
}

impl ExactText {
    fn try_new(n: u64) -> Result<Self, BudgetError> {
        let n = usize::try_from(n).map_err(|_| BudgetError::Overflow)?;
        let mut text = String::new();
        text.try_reserve_exact(n)
            .map_err(|_| BudgetError::AllocationFailed)?;
        Ok(Self { text, error: None })
    }

    fn into_exact(self, n: u64) -> Result<String, BudgetError> {
        if self.text.len() as u64 == n {
            Ok(self.text)
        } else {
            Err(BudgetError::Overflow)
        }
    }
}

impl TextOutput for ExactText {
    fn try_push_str(&mut self, text: &str) -> Result<(), BudgetError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if self.text.capacity().saturating_sub(self.text.len()) < text.len() {
            self.error = Some(BudgetError::Overflow);
            return Err(BudgetError::Overflow);
        }
        self.text.push_str(text);
        Ok(())
    }
    fn error(&self) -> Option<&BudgetError> {
        self.error.as_ref()
    }
}

impl std::fmt::Write for ExactText {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.try_push_str(text).map_err(|_| std::fmt::Error)
    }
}

pub(crate) fn column_model(cs: &[Column]) -> u64 {
    block((cs.len() as u64).saturating_mul(std::mem::size_of::<Column>() as u64))
        + cs.iter()
            .map(|c| block(c.name.len() as u64) + block(c.ty.len() as u64))
            .sum::<u64>()
}

pub(crate) fn cell_bytes(c: &Cell) -> usize {
    std::mem::size_of::<Cell>() + c.t.len() + c.r.as_ref().map_or(0, |r| r.kind.len() + r.id.len())
}

pub(crate) fn row_bytes(row: &[Cell]) -> usize {
    std::mem::size_of::<Vec<Cell>>() + row.iter().map(cell_bytes).sum::<usize>()
}
