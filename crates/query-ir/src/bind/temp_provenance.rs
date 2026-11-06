use super::{BindCtx, PolicyCompileStats};
use crate::ir::StmtS;
use crate::{ColumnInfo, CompiledStmt, ExecError, StmtL, TempOut, TempSchema};
use std::ops::{ControlFlow, Range};
use std::sync::Arc;

pub(crate) const SAFE: u8 = 1;
pub(crate) const BALANCE: u8 = 2;
pub(crate) const TURNOVERS: u8 = 4;
pub(crate) const UNKNOWN: u8 = 8;

struct GenerationIdentity {
    version: u64,
}
struct RunIdentity {
    _private: u8,
}

#[doc(hidden)]
#[derive(Clone)]
pub struct TempGeneration {
    identity: Arc<GenerationIdentity>,
}
impl TempGeneration {
    fn new(version: u64) -> Self {
        Self {
            identity: Arc::new(GenerationIdentity { version }),
        }
    }
    #[doc(hidden)]
    pub fn retained_bytes(&self) -> Result<u64, ExecError> {
        arc_bytes::<GenerationIdentity>()
    }
}

#[doc(hidden)]
pub struct MaterializedOrigins {
    schema: Arc<TempSchema>,
    generation: TempGeneration,
    _run: Arc<RunIdentity>,
    flags: Vec<u8>,
}
impl MaterializedOrigins {
    #[doc(hidden)]
    pub fn generation(&self) -> &TempGeneration {
        &self.generation
    }
    #[doc(hidden)]
    pub fn retained_bytes(&self) -> Result<u64, ExecError> {
        add(
            arc_bytes::<RunIdentity>()?,
            vector::<u8>(self.flags.capacity())?,
        )
    }
}

#[doc(hidden)]
pub struct BoundTempStatement {
    statement: StmtL,
    origins: Option<MaterializedOrigins>,
}
impl BoundTempStatement {
    #[doc(hidden)]
    pub fn statement(&self) -> &StmtL {
        &self.statement
    }
    #[doc(hidden)]
    pub fn into_parts(self) -> (StmtL, Option<MaterializedOrigins>) {
        (self.statement, self.origins)
    }
}

#[doc(hidden)]
pub enum TempOriginEvent<'a> {
    Step(usize),
    Run {
        rows: Range<usize>,
        proof: &'a MaterializedOrigins,
    },
}

#[doc(hidden)]
pub unsafe trait TempOriginProvider: Sync {
    #[doc(hidden)]
    fn destination_header<'a>(
        &'a self,
        _name: &str,
        _step: &mut dyn FnMut(usize) -> ControlFlow<()>,
    ) -> Option<(&'a Arc<TempSchema>, &'a TempGeneration)> {
        None
    }
    #[doc(hidden)]
    fn visit_committed<'a>(
        &'a self,
        _name: &str,
        _visit: &mut dyn FnMut(TempOriginEvent<'a>) -> ControlFlow<()>,
    ) -> Option<(&'a Arc<TempSchema>, &'a TempGeneration, usize)> {
        None
    }
}

#[doc(hidden)]
pub fn preflight_virtual_with_temp_origins(
    stmt: &CompiledStmt,
    ctx: &BindCtx<'_>,
    temps: &dyn TempOriginProvider,
) -> Result<(), ExecError> {
    super::security::typed::preflight_with_origins(stmt, ctx, Some(temps))
}

#[doc(hidden)]
pub fn bind_with_temp_origins(
    stmt: &StmtS,
    ctx: &BindCtx<'_>,
    temps: &dyn TempOriginProvider,
    output: Option<&TempOut>,
    charge_origins: &mut dyn FnMut(u64) -> Result<(), ExecError>,
    charge_policy: &mut dyn FnMut(&PolicyCompileStats) -> Result<(), ExecError>,
) -> Result<BoundTempStatement, ExecError> {
    let params = super::ParamValuesSnapshot::new(ctx.params);
    let ctx = BindCtx {
        params: &params,
        schema: ctx.schema,
        storage: ctx.storage,
        map: ctx.map,
        host: ctx.host,
        security: ctx.security,
    };
    let result = (|| {
        let (secured, security) =
            super::security::prepare::prepare_with_origins(stmt, &ctx, charge_policy, Some(temps))?;
        let origins = export_output(stmt, ctx.schema, Some(temps), output, charge_origins)?;
        let selected = secured.as_ref().unwrap_or(stmt);
        let mut bound = super::resolve(selected, &ctx)?;
        bound.security = security;
        let statement = crate::lower::lower(&bound, &ctx)?;
        Ok(BoundTempStatement { statement, origins })
    })();
    params.check()?;
    result
}

pub(crate) fn overflow() -> ExecError {
    ExecError::Memory(memory_budget::MemoryLimit {
        category: memory_budget::Category::Execution,
        requested: u64::MAX,
        reason: memory_budget::LimitReason::InvalidSize,
    })
}
fn add(a: u64, b: u64) -> Result<u64, ExecError> {
    a.checked_add(b).ok_or_else(overflow)
}
fn block(bytes: u64) -> Result<u64, ExecError> {
    if bytes == 0 {
        return Ok(0);
    }
    bytes
        .checked_add(15)
        .and_then(|n| (n / 16).checked_mul(16))
        .and_then(|n| n.checked_add(16))
        .ok_or_else(overflow)
}
fn vector<T>(capacity: usize) -> Result<u64, ExecError> {
    std::alloc::Layout::array::<T>(capacity).map_err(|_| overflow())?;
    block(
        (capacity as u64)
            .checked_mul(std::mem::size_of::<T>() as u64)
            .ok_or_else(overflow)?,
    )
}
fn arc_bytes<T>() -> Result<u64, ExecError> {
    block(add(
        std::mem::size_of::<T>() as u64,
        std::mem::size_of::<(usize, usize)>() as u64,
    )?)
}
fn compatible_columns(columns: &[ColumnInfo], schema: &TempSchema) -> bool {
    !schema.open
        && columns.len() == schema.columns.len()
        && columns.iter().zip(&schema.columns).all(|(source, want)| {
            !want.unknown
                && want.ty.as_ref().is_some_and(|want| {
                    !want.is_undefined() && crate::sema::add_type_compatible(want, &source.ty)
                })
                && source.nested.is_none()
        })
}

struct ImportScratch<'a> {
    column: usize,
    first: Option<&'a MaterializedOrigins>,
    end: usize,
    flags: u8,
    valid: bool,
}
impl<'a> ImportScratch<'a> {
    fn new(column: usize) -> Self {
        Self {
            column,
            first: None,
            end: 0,
            flags: 0,
            valid: true,
        }
    }
    fn run(&mut self, rows: Range<usize>, proof: &'a MaterializedOrigins) -> bool {
        self.valid &= rows.start == self.end && rows.end >= rows.start;
        if rows.is_empty() && self.first.is_some() {
            self.valid = false;
        }
        if let Some(first) = self.first {
            self.valid &= Arc::ptr_eq(&first.generation.identity, &proof.generation.identity)
                && Arc::ptr_eq(&first.schema, &proof.schema)
                && first.flags.len() == proof.flags.len();
        } else {
            self.first = Some(proof);
        }
        self.flags |= if rows.is_empty() {
            SAFE
        } else {
            proof.flags.get(self.column).copied().unwrap_or(UNKNOWN)
        };
        self.end = rows.end;
        self.valid
    }
    fn valid_header(
        &self,
        schema: &Arc<TempSchema>,
        generation: &TempGeneration,
        rows: usize,
        version: u64,
    ) -> bool {
        self.first.is_some_and(|first| {
            self.valid
                && self.end == rows
                && !schema.open
                && generation.identity.version == version
                && Arc::ptr_eq(&generation.identity, &first.generation.identity)
                && Arc::ptr_eq(schema, &first.schema)
                && first.flags.len() == schema.columns.len()
        })
    }
}

#[derive(Clone, Copy)]
pub(in crate::bind) enum TempShape<'a> {
    Ir {
        columns: &'a [crate::QType],
        stored: &'a [Option<crate::NumQ>],
    },
    Typed(&'a TempSchema),
}
impl TempShape<'_> {
    pub(in crate::bind) fn width(&self) -> usize {
        match self {
            Self::Ir { columns, .. } => columns.len(),
            Self::Typed(schema) => schema.columns.len(),
        }
    }
    fn slot(&self, i: usize) -> Option<(&crate::QType, Option<crate::NumQ>)> {
        match self {
            Self::Ir { columns, stored } => Some((columns.get(i)?, *stored.get(i)?)),
            Self::Typed(schema) => {
                let column = schema.columns.get(i)?;
                if schema.open || column.unknown {
                    return None;
                }
                Some((column.ty.as_ref()?, column.stored))
            }
        }
    }
    fn matches(&self, schema: &TempSchema, spend: &mut dyn FnMut(usize) -> bool) -> bool {
        if schema.open || self.width() != schema.columns.len() || !spend(1) {
            return false;
        }
        match self {
            Self::Ir { columns, stored } if columns.len() != stored.len() => return false,
            Self::Typed(expected) if expected.open => return false,
            _ => {}
        }
        for (i, column) in schema.columns.iter().enumerate() {
            let Some((expected, stored)) = self.slot(i) else {
                return false;
            };
            let Some(actual) = &column.ty else {
                return false;
            };
            let work = expected
                .refs
                .len()
                .checked_add(expected.others.len())
                .and_then(|n| n.checked_add(actual.refs.len()))
                .and_then(|n| n.checked_add(actual.others.len()))
                .and_then(|n| n.checked_add(1));
            if work.is_none_or(|n| !spend(n))
                || column.unknown
                || expected.is_undefined()
                || actual.is_undefined()
                || expected != actual
                || stored != column.stored
            {
                return false;
            }
        }
        true
    }
}
pub(in crate::bind) enum ImportEvent {
    Step(usize),
    Column(u16, u8),
}

pub(in crate::bind) fn import_columns(
    provider: &dyn TempOriginProvider,
    name: &str,
    shape: TempShape<'_>,
    version: u64,
    apply: &mut dyn FnMut(ImportEvent) -> bool,
) -> bool {
    let mut scratch = ImportScratch::new(0);
    let mut stopped = false;
    let header = provider.visit_committed(name, &mut |event| {
        let accepted = match event {
            TempOriginEvent::Step(n) => apply(ImportEvent::Step(n)),
            TempOriginEvent::Run { rows, proof } => {
                if !apply(ImportEvent::Step(1)) || !scratch.run(rows.clone(), proof) {
                    false
                } else {
                    let mut accepted = true;
                    for i in 0..shape.width() {
                        let Ok(column) = u16::try_from(i) else {
                            accepted = false;
                            break;
                        };
                        if !apply(ImportEvent::Step(1))
                            || !apply(ImportEvent::Column(
                                column,
                                if rows.is_empty() {
                                    SAFE
                                } else {
                                    proof.flags.get(i).copied().unwrap_or(UNKNOWN)
                                },
                            ))
                        {
                            accepted = false;
                            break;
                        }
                    }
                    accepted
                }
            }
        };
        if accepted {
            ControlFlow::Continue(())
        } else {
            stopped = true;
            ControlFlow::Break(())
        }
    });
    let Some((schema, generation, rows)) = header else {
        return false;
    };
    !stopped
        && shape.matches(schema, &mut |n| apply(ImportEvent::Step(n)))
        && scratch.valid_header(schema, generation, rows, version)
}

fn output_flags(
    stmt: &StmtS,
    schema: &crate::QuerySchema,
    temps: Option<&dyn TempOriginProvider>,
    width: usize,
) -> Result<Vec<u8>, ExecError> {
    let mut workspace = super::security::provenance::Workspace::new();
    workspace.set_temp_origins(temps, schema.version());
    if stmt.totals.is_some() || !stmt.nested.is_empty() {
        workspace.disable();
    }
    workspace.record_ir(&stmt.root, stmt, schema, false);
    workspace.record_outputs(&stmt.root, stmt, schema, width);
    let mut flags = Vec::with_capacity(width);
    for i in 0..width {
        let value = u16::try_from(i)
            .ok()
            .map_or(UNKNOWN, |i| workspace.export_output(i));
        flags.push(value & (SAFE | BALANCE | TURNOVERS | UNKNOWN));
    }
    if workspace.failed() {
        flags.fill(UNKNOWN);
    }
    Ok(flags)
}
fn export_output(
    stmt: &StmtS,
    schema: &crate::QuerySchema,
    temps: Option<&dyn TempOriginProvider>,
    output: Option<&TempOut>,
    charge: &mut dyn FnMut(u64) -> Result<(), ExecError>,
) -> Result<Option<MaterializedOrigins>, ExecError> {
    let Some(output) = output else {
        return Ok(None);
    };
    let (destination, existing) = match output {
        TempOut::Put(out) => (out, None),
        TempOut::Add(name) => {
            let Some(provider) = temps else {
                return Ok(None);
            };
            let mut visits = 0usize;
            let Some((out, generation)) =
                provider.destination_header(name, &mut |n| match visits.checked_add(n) {
                    Some(next) if next < super::security::provenance::VISIT_LIMIT => {
                        visits = next;
                        ControlFlow::Continue(())
                    }
                    _ => ControlFlow::Break(()),
                })
            else {
                return Ok(None);
            };
            if generation.identity.version != schema.version() {
                return Ok(None);
            }
            (out, Some(generation))
        }
    };
    if !compatible_columns(&stmt.columns, destination) {
        return Ok(None);
    }
    let bytes = add(
        arc_bytes::<RunIdentity>()?,
        vector::<u8>(destination.columns.len())?,
    )?;
    let bytes = if existing.is_none() {
        add(bytes, arc_bytes::<GenerationIdentity>()?)?
    } else {
        bytes
    };
    charge(bytes)?;
    let flags = output_flags(stmt, schema, temps, destination.columns.len())?;
    let generation = existing
        .cloned()
        .unwrap_or_else(|| TempGeneration::new(schema.version()));
    Ok(Some(MaterializedOrigins {
        schema: destination.clone(),
        generation,
        _run: Arc::new(RunIdentity { _private: 0 }),
        flags,
    }))
}

const _: () = assert!(
    std::mem::size_of::<(
        super::security::provenance::Workspace<'_>,
        ImportScratch<'_>,
        TempShape<'_>,
        ImportEvent
    )>() <= 32 * 1024
);
