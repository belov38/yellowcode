use super::{Backing, ManagedColumn, ManagedNumber, ManagedRow, ManagedValue, SharedManagedRow};
use memory_budget::{
    BudgetError, Category, LimitReason, MemoryBudget, MemoryLimit, Reservation, ScopeBorrow, Shared,
};

#[derive(Debug)]
pub struct ScopedRow {
    row: SharedManagedRow,
    imports: ScopeBorrow,
    _descriptor: Reservation,
}
impl ScopedRow {
    pub fn try_from_owned(row: ManagedRow) -> Result<Self, BudgetError> {
        let budget = row.reservation().budget().clone();
        let mut footprint = Footprint::new(&budget)?;
        footprint.owner(row.reservation())?;
        footprint.values(&row, 0)?;
        let imports = budget.try_borrow_scope(footprint.imported)?;
        let descriptor = descriptor(&budget)?;
        let row = row.try_into_shared()?;
        Ok(Self {
            row,
            imports,
            _descriptor: descriptor,
        })
    }

    pub fn try_from_shared(
        row: SharedManagedRow,
        budget: &MemoryBudget,
    ) -> Result<Self, BudgetError> {
        let mut footprint = Footprint::new(budget)?;
        footprint.owner(row.reservation())?;
        footprint.values(&row, 0)?;
        let imports = budget.try_borrow_scope(footprint.imported)?;
        let descriptor = descriptor(budget)?;
        Ok(Self {
            row,
            imports,
            _descriptor: descriptor,
        })
    }

    pub fn values(&self) -> &[ManagedValue] {
        &self.row
    }
    pub fn budget(&self) -> &MemoryBudget {
        self.imports.budget()
    }
    pub fn imported_bytes(&self) -> u64 {
        self.imports.bytes()
    }
    pub fn try_into_shared(self) -> Result<Shared<Self>, BudgetError> {
        let charge = self.budget().try_reserve(Category::Execution, 0)?;
        Shared::try_new(self, charge)
    }
}

#[derive(Debug)]
pub struct ScopedInput {
    source: Shared<ScopedRow>,
    imports: ScopeBorrow,
    _descriptor: Reservation,
}
impl ScopedInput {
    pub fn try_new(source: Shared<ScopedRow>, budget: &MemoryBudget) -> Result<Self, BudgetError> {
        let mut footprint = Footprint::new(budget)?;
        footprint.owner(source.reservation())?;
        footprint.owner(&source._descriptor)?;
        footprint.owner(source.row.reservation())?;
        footprint.values(source.values(), 0)?;
        let imports = budget.try_borrow_scope(footprint.imported)?;
        let descriptor = budget.try_reserve(
            Category::Execution,
            u64::try_from(std::mem::size_of::<Self>()).map_err(|_| BudgetError::Overflow)?,
        )?;
        Ok(Self {
            source,
            imports,
            _descriptor: descriptor,
        })
    }
    pub fn values(&self) -> &[ManagedValue] {
        self.source.values()
    }
    pub fn budget(&self) -> &MemoryBudget {
        self.imports.budget()
    }
    pub fn imported_bytes(&self) -> u64 {
        self.imports.bytes()
    }
}

fn descriptor(budget: &MemoryBudget) -> Result<Reservation, BudgetError> {
    Ok(budget.try_reserve(
        Category::Execution,
        u64::try_from(std::mem::size_of::<ScopedRow>()).map_err(|_| BudgetError::Overflow)?,
    )?)
}
fn invalid() -> BudgetError {
    BudgetError::Limit(MemoryLimit {
        category: Category::Row,
        requested: 0,
        reason: LimitReason::InvalidSize,
    })
}
struct Footprint<'a> {
    budget: &'a MemoryBudget,
    imported: u64,
    initially_live: u64,
    limit: u64,
}
impl<'a> Footprint<'a> {
    fn new(budget: &'a MemoryBudget) -> Result<Self, BudgetError> {
        let scope = budget.scope_snapshot().ok_or_else(invalid)?;
        if scope.category != Category::Row {
            return Err(invalid());
        }
        Ok(Self {
            budget,
            imported: 0,
            initially_live: scope.current,
            limit: scope.limit,
        })
    }
    fn owner(&mut self, owner: &Reservation) -> Result<(), BudgetError> {
        if !owner.budget().same_instance(self.budget) {
            return Err(invalid());
        }
        if !owner.budget().same_scope(self.budget) {
            self.imported = self
                .imported
                .checked_add(owner.bytes())
                .ok_or(BudgetError::Overflow)?;
            let requested = self
                .initially_live
                .checked_add(self.imported)
                .ok_or(BudgetError::Overflow)?;
            if requested > self.limit {
                return Err(BudgetError::Limit(MemoryLimit {
                    category: Category::Row,
                    requested,
                    reason: LimitReason::Category,
                }));
            }
        }
        Ok(())
    }
    fn values(&mut self, values: &[ManagedValue], depth: usize) -> Result<(), BudgetError> {
        if depth >= 128 {
            return Err(BudgetError::Overflow);
        }
        for value in values {
            match &value.0 {
                Backing::Number(ManagedNumber::Shared(number)) => {
                    self.owner(number.reservation())?
                }
                Backing::String(text) => {
                    self.owner(text.managed_reservation().ok_or_else(invalid)?)?
                }
                Backing::Type(ty) => self.owner(ty.reservation())?,
                Backing::Binary(bytes) | Backing::Storage(bytes) => {
                    self.owner(bytes.reservation())?
                }
                Backing::Nested(nested) => {
                    self.owner(nested.reservation())?;
                    self.owner(nested.columns.reservation())?;
                    self.columns(&nested.columns, depth)?;
                    self.owner(nested.rows.reservation())?;
                    for row in nested.rows.iter() {
                        self.owner(row.reservation())?;
                        self.values(row, depth + 1)?;
                    }
                }
                Backing::Null
                | Backing::Undefined
                | Backing::Bool(_)
                | Backing::Number(ManagedNumber::Inline(_))
                | Backing::Date(_)
                | Backing::Uuid(_)
                | Backing::Ref { .. }
                | Backing::SysEnum(..) => {}
            }
        }
        Ok(())
    }
    fn columns(&mut self, columns: &[ManagedColumn], depth: usize) -> Result<(), BudgetError> {
        if depth >= 128 {
            return Err(BudgetError::Overflow);
        }
        for column in columns {
            self.owner(column.name.reservation())?;
            self.owner(column.ty.reservation())?;
            if let Some(children) = &column.nested {
                self.owner(children.reservation())?;
                self.columns(children, depth + 1)?;
            }
        }
        Ok(())
    }
}
