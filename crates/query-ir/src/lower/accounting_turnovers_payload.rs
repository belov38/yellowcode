use crate::schema::{QFieldId, StorageBinding};
use crate::{DateQ, ExecError, NumQ, QType, StrQ};
use ibschema::logical::{TypeCode, VAR};
use ibschema::storage::{FieldId, FieldInfo, IndexId, StorageMap, TableId};

#[derive(Clone, Debug, PartialEq)]
pub struct TurnoversValueCheckL {
    pub nullable: bool,
    pub undefined: bool,
    pub boolean: bool,
    pub number: Option<NumQ>,
    pub string: Option<StrQ>,
    pub date: Option<DateQ>,
    pub uuid: bool,
    pub reference_tags: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TurnoversExtDimSlotL {
    pub ordinal: u8,
    pub output: QFieldId,
    pub values: Option<[FieldId; 2]>,
    pub value_check: TurnoversValueCheckL,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TurnoversAccRgEdL {
    pub table: TableId,
    pub main_period: FieldId,
    pub main_recorder: FieldId,
    pub main_line: FieldId,
    pub recorder_tags: Vec<u32>,
    pub section_dim_kind: FieldId,
    pub kind_ref_table: u32,
    pub ed_period: FieldId,
    pub ed_recorder: FieldId,
    pub ed_line: FieldId,
    pub ed_correspond: FieldId,
    pub ed_kind: FieldId,
    pub ed_value: FieldId,
    pub main_by_period: Option<IndexId>,
    pub ed_by_period: Option<IndexId>,
    pub ed_by_recorder: Option<IndexId>,
    pub ed_by_recorder_has_correspond: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TurnoversExtDimsL {
    pub section: TableId,
    pub owner: FieldId,
    pub ordinal: FieldId,
    pub acc_rg_ed: Option<Box<TurnoversAccRgEdL>>,
    pub ext_flag_fields: Vec<FieldId>,
    pub slots: Vec<TurnoversExtDimSlotL>,
}

fn fatal(message: &'static str) -> ExecError {
    ExecError::Fatal(format!("accounting Turnovers: {message}"))
}

fn later(what: &'static str) -> ExecError {
    ExecError::Later { what, stage: "Q6b" }
}

pub(super) fn validate_shape_with_later(
    semantic: &QType,
    physical: &FieldInfo,
    storage: &StorageBinding,
    map: &StorageMap,
    later_what: &'static str,
) -> Result<(), ExecError> {
    if semantic
        .number
        .is_some_and(|q| q.digits > 38 || q.scale > q.digits)
        || semantic.string.is_some_and(|q| q.len >= VAR)
    {
        return Err(later(later_what));
    }
    if semantic
        .others
        .iter()
        .any(|other| *other != crate::Other::Uuid)
    {
        return Err(later(later_what));
    }
    let uuid = semantic.others.contains(&crate::Other::Uuid);
    for (index, atom) in semantic.refs.iter().enumerate() {
        let tag = storage
            .ref_number(*atom)
            .ok_or_else(|| fatal("unbound semantic ExtDimension reference"))?;
        if semantic
            .refs
            .iter()
            .take(index)
            .any(|other| storage.ref_number(*other) == Some(tag))
        {
            return Err(fatal("aliased semantic ExtDimension reference tags"));
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
        return Err(fatal("contradictory ExtDimension composite carrier"));
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
            TypeCode::V => return Err(later(later_what)),
        };
        if *seen {
            return Err(fatal("duplicate physical ExtDimension atom"));
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
                "physical ExtDimension atom contradicts semantic type",
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
        return Err(fatal("incomplete physical ExtDimension union"));
    }
    Ok(())
}

pub(super) fn value_check(
    semantic: &QType,
    physical: &FieldInfo,
    storage: &StorageBinding,
    map: &StorageMap,
) -> Result<TurnoversValueCheckL, ExecError> {
    value_check_with_later(
        semantic,
        physical,
        storage,
        map,
        "Q6b-TurnoversExtDimensionLayout",
    )
}

pub(super) fn value_check_with_later(
    semantic: &QType,
    physical: &FieldInfo,
    storage: &StorageBinding,
    map: &StorageMap,
    later_what: &'static str,
) -> Result<TurnoversValueCheckL, ExecError> {
    validate_shape_with_later(semantic, physical, storage, map, later_what)?;
    let uuid = semantic.others.contains(&crate::Other::Uuid);
    let mut reference_tags = Vec::with_capacity(semantic.refs.len());
    for atom in &semantic.refs {
        reference_tags.push(
            storage
                .ref_number(*atom)
                .ok_or_else(|| fatal("unbound semantic ExtDimension reference"))?,
        );
    }
    reference_tags.sort_unstable();
    reference_tags.dedup();
    let carrier = physical
        .types
        .first()
        .is_some_and(|ty| ty.code == TypeCode::E);
    Ok(TurnoversValueCheckL {
        nullable: physical.nullable,
        undefined: carrier,
        boolean: semantic.boolean,
        number: semantic.number,
        string: semantic.string,
        date: semantic.date,
        uuid,
        reference_tags,
    })
}
