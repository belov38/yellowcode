use memory_budget::{BudgetError, Category, Lock, MemoryBudget, Reservation};

const CATALOG_BYTES: u64 = 1 << 20;

struct Bootstrap {
    admission: Option<Reservation>,
    complete: bool,
}
impl Bootstrap {
    const fn new() -> Self {
        Self {
            admission: None,
            complete: false,
        }
    }
    fn initialize(
        &mut self,
        root: &MemoryBudget,
        initialize: impl FnOnce(),
    ) -> Result<(), BudgetError> {
        if self.complete {
            return Ok(());
        }
        if self.admission.is_none() {
            self.admission = Some(root.try_reserve(Category::Metadata, CATALOG_BYTES)?);
        }
        initialize();
        self.complete = true;
        Ok(())
    }
}
static CATALOGS: Lock<Bootstrap> = Lock::new(Bootstrap::new());

pub(super) fn ensure(root: &MemoryBudget) -> Result<(), BudgetError> {
    CATALOGS.lock().initialize(root, || {
        let _ = ui_style::Metrics::get(ui_style::Interface::Taxi);
        let _ = ui_style::captions::captions();
        let _ = ui_style::std_pictures().next();
        let _ = ui_style::Style::get();
    })
}
