use crate::schema::{QTableId, QuerySchema, StorageBinding};
use crate::{DateQ, ExecError, NumQ, QShared, QType, StrQ};
use ibschema::logical::{TypeCode, VAR};
use ibschema::storage::{FieldId, FieldInfo, StorageMap, TableId};
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, RequestMemory, Shared};

pub struct BalanceValueCheckL {
    pub nullable: bool,
    pub undefined: bool,
    pub boolean: bool,
    pub number: Option<NumQ>,
    pub string: Option<StrQ>,
    pub date: Option<DateQ>,
    pub uuid: bool,
    pub reference_tags: Charged<Vec<u32>>,
}
impl PartialEq for BalanceValueCheckL {
    fn eq(&self, other: &Self) -> bool {
        self.nullable == other.nullable
            && self.undefined == other.undefined
            && self.boolean == other.boolean
            && self.number == other.number
            && self.string == other.string
            && self.date == other.date
            && self.uuid == other.uuid
            && self.reference_tags.as_slice() == other.reference_tags.as_slice()
    }
}
#[derive(Debug, PartialEq)]
pub struct BalanceFieldCheckL {
    pub field: FieldId,
    pub value: BalanceValueCheckL,
}
pub struct BalanceExt1L {
    pub section: TableId,
    pub owner: FieldId,
    pub ordinal: FieldId,
    pub turnover_only: FieldId,
    pub values: [FieldId; 2],
    pub value_check: BalanceValueCheckL,
    pub shared_checks: Charged<Vec<BalanceFieldCheckL>>,
}
impl PartialEq for BalanceExt1L {
    fn eq(&self, other: &Self) -> bool {
        self.section == other.section
            && self.owner == other.owner
            && self.ordinal == other.ordinal
            && self.turnover_only == other.turnover_only
            && self.values == other.values
            && self.value_check == other.value_check
            && self.shared_checks.as_slice() == other.shared_checks.as_slice()
    }
}

impl std::fmt::Debug for BalanceValueCheckL {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BalanceValueCheckL")
            .field("nullable", &self.nullable)
            .field("undefined", &self.undefined)
            .field("boolean", &self.boolean)
            .field("number", &self.number)
            .field("string", &self.string)
            .field("date", &self.date)
            .field("uuid", &self.uuid)
            .field("reference_tags_len", &self.reference_tags.len())
            .finish()
    }
}
impl std::fmt::Debug for BalanceExt1L {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BalanceExt1L")
            .field("section", &self.section)
            .field("owner", &self.owner)
            .field("ordinal", &self.ordinal)
            .field("turnover_only", &self.turnover_only)
            .field("values", &self.values)
            .field("value_check", &self.value_check)
            .field("shared_checks_len", &self.shared_checks.len())
            .finish()
    }
}

pub(crate) fn memory_error(error: BudgetError) -> ExecError {
    match error {
        BudgetError::Limit(limit) => ExecError::Memory(limit),
        BudgetError::Overflow => ExecError::Memory(memory_budget::MemoryLimit {
            category: Category::Execution,
            requested: u64::MAX,
            reason: memory_budget::LimitReason::InvalidSize,
        }),
        BudgetError::AllocationFailed => {
            ExecError::Fatal("accounting payload allocation failed".into())
        }
    }
}
fn fatal(message: &'static str) -> ExecError {
    ExecError::Fatal(message.into())
}
fn later() -> ExecError {
    ExecError::Later {
        what: "Q6a-AccountingBalanceExtDimensionLayout",
        stage: "Q6a",
    }
}

pub(super) fn validate_shape(
    semantic: &QType,
    physical: &FieldInfo,
    storage: &StorageBinding,
    map: &StorageMap,
) -> Result<(), ExecError> {
    if semantic
        .number
        .is_some_and(|q| q.digits > 38 || q.scale > q.digits)
        || semantic.string.is_some_and(|q| q.len >= VAR)
    {
        return Err(later());
    }
    if semantic
        .others
        .iter()
        .any(|other| *other != crate::Other::Uuid)
    {
        return Err(later());
    }
    let uuid = semantic.others.contains(&crate::Other::Uuid);
    for (index, atom) in semantic.refs.iter().enumerate() {
        let tag = storage
            .ref_number(*atom)
            .ok_or_else(|| fatal("accounting payload: unbound semantic reference"))?;
        if semantic
            .refs
            .iter()
            .take(index)
            .any(|other| storage.ref_number(*other) == Some(tag))
        {
            return Err(fatal("accounting payload: aliased semantic reference tags"));
        }
    }
    let tag_count = semantic.refs.len();
    let first_tag = semantic
        .refs
        .first()
        .and_then(|atom| storage.ref_number(*atom));
    let carrier = physical
        .types
        .first()
        .is_some_and(|ty| ty.code == TypeCode::E);
    let atoms = usize::from(semantic.boolean)
        + usize::from(semantic.number.is_some())
        + usize::from(semantic.string.is_some())
        + usize::from(semantic.date.is_some())
        + usize::from(uuid)
        + usize::from(tag_count != 0);
    if atoms == 0 || (carrier != (atoms > 1 || tag_count > 1)) {
        return Err(fatal("accounting payload: contradictory composite carrier"));
    }
    let mut codes = [false; 8];
    for ty in &physical.types {
        let seen = match ty.code {
            TypeCode::E => &mut codes[0],
            TypeCode::L => &mut codes[1],
            TypeCode::N => &mut codes[2],
            TypeCode::S => &mut codes[3],
            TypeCode::T => &mut codes[4],
            TypeCode::R => &mut codes[5],
            TypeCode::B => &mut codes[6],
            TypeCode::V => return Err(later()),
        };
        if *seen {
            return Err(fatal("accounting payload: duplicate physical atom"));
        }
        *seen = true;
        let valid = match ty.code {
            TypeCode::E => carrier && ty.length == 0 && ty.precision == 0,
            TypeCode::L => semantic.boolean && ty.length == 0 && ty.precision == 0,
            TypeCode::N => semantic.number.is_some_and(|q| {
                ty.length == u32::from(q.digits) && ty.precision == u32::from(q.scale)
            }),
            TypeCode::S => semantic.string.is_some_and(|q| {
                let length = if q.len == 0 {
                    VAR
                } else if q.fixed {
                    q.len
                } else {
                    VAR | q.len
                };
                ty.length == length && ty.precision == 0
            }),
            TypeCode::T => semantic.date.is_some() && ty.length == 0 && ty.precision == 0,
            TypeCode::B => uuid && ty.length == 16 && ty.precision == 0,
            TypeCode::R if tag_count == 1 => {
                ty.ref_kind == 3
                    && map
                        .table_by_name(&ty.ref_table)
                        .and_then(|id| map.ref_number(id))
                        == first_tag
            }
            TypeCode::R => tag_count > 1 && ty.ref_kind == 4 && ty.ref_table.is_empty(),
            TypeCode::V => false,
        };
        if !valid {
            return Err(fatal(
                "accounting payload: physical atom contradicts semantic type",
            ));
        }
    }
    if codes[1] != semantic.boolean
        || codes[2] != semantic.number.is_some()
        || codes[3] != semantic.string.is_some()
        || codes[4] != semantic.date.is_some()
        || codes[5] == (tag_count == 0)
        || codes[6] != uuid
    {
        return Err(fatal("accounting payload: incomplete physical union"));
    }
    Ok(())
}

pub(super) fn value_check(
    semantic: &QType,
    physical: &FieldInfo,
    storage: &StorageBinding,
    map: &StorageMap,
    memory: &RequestMemory,
) -> Result<BalanceValueCheckL, ExecError> {
    validate_shape(semantic, physical, storage, map)?;
    let uuid = semantic.others.contains(&crate::Other::Uuid);
    let mut tags =
        BudgetedVec::try_with_capacity(&memory.budget, Category::Execution, semantic.refs.len())
            .map_err(memory_error)?;
    for atom in &semantic.refs {
        tags.try_push(
            storage
                .ref_number(*atom)
                .ok_or_else(|| fatal("accounting payload: unbound semantic reference"))?,
        )
        .map_err(memory_error)?;
    }
    tags.as_mut_slice().sort_unstable();
    let slice = tags.as_mut_slice();
    let mut unique: usize = 0;
    for index in 0..slice.len() {
        if slice.get(index) != unique.checked_sub(1).and_then(|last| slice.get(last)) {
            slice.swap(unique, index);
            unique += 1;
        }
    }
    tags.truncate(unique);
    let tags = tags.into_charged_vec();
    assert!(tags.reservation().budget().same_scope(&memory.budget));
    let carrier = physical
        .types
        .first()
        .is_some_and(|ty| ty.code == TypeCode::E);
    Ok(BalanceValueCheckL {
        nullable: physical.nullable,
        undefined: carrier,
        boolean: semantic.boolean,
        number: semantic.number,
        string: semantic.string,
        date: semantic.date,
        uuid,
        reference_tags: tags,
    })
}

pub(super) fn policy_type<'a>(
    schema: &'a QuerySchema,
    storage: &StorageBinding,
    register: QTableId,
    field: FieldId,
) -> Result<&'a QType, ExecError> {
    let mut ty = None;
    for (index, candidate) in schema.table(register).fields.iter().enumerate() {
        let id = crate::schema::QFieldId(
            u16::try_from(index)
                .map_err(|_| fatal("accounting payload: logical field index overflow"))?,
        );
        if storage.field(register, id) == Some(field) {
            if ty.is_some_and(|old| old != &candidate.ty) {
                return Err(fatal("accounting payload: policy alias type mismatch"));
            }
            ty = Some(&candidate.ty);
        }
    }
    ty.ok_or_else(|| fatal("accounting payload: policy field has no semantic binding"))
}

pub(super) fn build<'a>(
    identity: (TableId, FieldId, FieldId, FieldId, [FieldId; 2]),
    semantic: &QType,
    main: TableId,
    shared: impl Iterator<Item = Result<(FieldId, &'a QType), ExecError>>,
    storage: &StorageBinding,
    map: &StorageMap,
    memory: &RequestMemory,
) -> Result<QShared<BalanceExt1L>, ExecError> {
    let (section, owner, ordinal, turnover_only, values) = identity;
    let main_info = map
        .try_table(main)
        .ok_or_else(|| fatal("accounting payload: missing main"))?;
    let dr = value_check(
        semantic,
        main_info
            .field(values[0])
            .ok_or_else(|| fatal("accounting payload: missing Dt1"))?,
        storage,
        map,
        memory,
    )?;
    let cr = value_check(
        semantic,
        main_info
            .field(values[1])
            .ok_or_else(|| fatal("accounting payload: missing Ct1"))?,
        storage,
        map,
        memory,
    )?;
    if dr != cr {
        return Err(fatal("accounting payload: incompatible side checks"));
    }
    drop(cr);
    let mut checks = BudgetedVec::new(&memory.budget, Category::Execution).map_err(memory_error)?;
    for shared in shared {
        let (field, semantic) = shared?;
        let value = value_check(
            semantic,
            main_info
                .field(field)
                .ok_or_else(|| fatal("accounting payload: missing shared field"))?,
            storage,
            map,
            memory,
        )?;
        checks
            .try_push(BalanceFieldCheckL { field, value })
            .map_err(memory_error)?;
    }
    let checks = checks.into_charged_vec();
    assert!(checks.reservation().budget().same_scope(&memory.budget));
    assert!(
        dr.reference_tags
            .reservation()
            .budget()
            .same_scope(&memory.budget)
    );
    assert!(checks.iter().all(|check| {
        check
            .value
            .reference_tags
            .reservation()
            .budget()
            .same_scope(&memory.budget)
    }));
    let payload = BalanceExt1L {
        section,
        owner,
        ordinal,
        turnover_only,
        values,
        value_check: dr,
        shared_checks: checks,
    };
    let node = memory
        .budget
        .try_reserve(Category::Execution, 0)
        .map_err(ExecError::Memory)?;
    assert!(node.budget().same_scope(&memory.budget));
    Ok(QShared::from_managed(
        Shared::try_new(payload, node).map_err(memory_error)?,
    ))
}
