// Derived from bcdb 0.2.1, src/BcDb.Core/Catalog.cs. Copyright (c) 2026 Stefan Maron, MIT License; see crates/mssql-backup/LICENSE-BCDB-MIT.
use crate::{
    bak_bytes::{i16_at, i32_at, i64_at, u8_at, u16_at, u32_at},
    bak_checksum::PAGE_SIZE,
    bak_read::{Error, PageReader, PageSource, TYPE_BOOT},
    bak_record::{PagePtr, PhysSlot, Storage},
    bak_table::{
        AllocUnit, ScanSource, TableDef, UnitKind, for_each_record, for_each_record_in_chain,
    },
    bak_values::{ColumnDef, ColumnType, SqlType, column_type},
    records::FixedVarRecord,
};

pub const BOOT_PAGE: u32 = 9;
pub const BOOT_ADR_AT: usize = 1848;
const BOOT_SYSALLOCUNITS_AT: usize = 612;
const SYSROWSETS_ROWSET: i64 = 5 << 16;
const SYSALLOCUNITS_UNIT: i64 = 7 << 16;
const SYSSCHOBJS: i32 = 34;
const SYSCOLPARS: i32 = 41;
const SYSRSCOLS: i32 = 3;
const SYSROWSETS: i32 = 5;
const SYSALLOCUNITS: i32 = 7;
pub const DBO_SCHEMA_ID: i32 = 1;
pub const MAX_COLUMNS: usize = 4096;
pub const MAX_TABLES: usize = 1 << 17;
const RSCOL_DROPPED_STATUS: u32 = 0x02;
const RSCOL_DROPPED_ID: u32 = 0x0400_0000;
const RSCOL_INTERNAL_ID: u32 = 0x0800_0000;
const RSCOL_ID_MASK: u32 = 0x00FF_FFFF;
const COLUMN_NOT_NULL: u32 = 0x01;
const COLUMN_ANSI_PADDED: u32 = 0x02;
const COLUMN_COMPUTED: u32 = 0x10;
const COLUMN_FILESTREAM: u32 = 0x20;
const COLUMN_SPARSE: u32 = 0x0100_0000;
const COLUMN_SET: u32 = 0x0200_0000;
const RSCOL_DEFAULT_FOR_ABSENT: u32 = 0x200;
const RSCOL_SPARSE: u32 = 0x100;
const COLUMNSTORE_LEVEL: u8 = 3;
const CLR_TYPE: u8 = 240;
const UNUSED: Storage = Storage::Fixed {
    offset: 0,
    width: 0,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableName {
    pub object_id: i32,
    pub schema_id: i32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectName {
    pub object_id: i32,
    pub schema_id: i32,
    pub kind: [u8; 2],
    pub name: String,
}

impl ObjectName {
    pub fn is_user_table(&self) -> bool {
        self.kind == *b"U "
    }
}

#[derive(Debug, Clone)]
pub struct Catalog {
    first_allocation_page: PagePtr,
    rowsets: AllocUnit,
    objects: AllocUnit,
    columns: AllocUnit,
    layouts: AllocUnit,
}

#[derive(Debug, Clone, Copy)]
struct Rowset {
    id: i64,
    object: i32,
    index: i32,
    compression: u8,
    rows: i64,
}

impl Rowset {
    fn from_fixed(fixed: &[u8]) -> Result<Rowset, Error> {
        let cut = || Error::Inconsistent("a sysrowsets record is cut off");
        Ok(Rowset {
            id: i64_at(fixed, 0).ok_or_else(cut)?,
            object: i32_at(fixed, 9).ok_or_else(cut)?,
            index: i32_at(fixed, 13).ok_or_else(cut)?,
            compression: u8_at(fixed, 35).ok_or_else(cut)?,
            rows: i64_at(fixed, 27).ok_or_else(cut)?,
        })
    }
}

#[derive(Debug, Clone)]
struct SysColumn {
    column_id: u32,
    name: String,
    xtype: u8,
    status: u32,
}

#[derive(Debug, Clone, Copy)]
struct PhysColumn {
    column_id: u32,
    dropped: bool,
    internal: bool,
    sparse: bool,
    default_for_absent: bool,
    ti: u32,
    leaf_offset: i16,
    null_bit: u16,
    bit_position: u16,
}

fn utf16_name(bytes: &[u8]) -> Result<String, Error> {
    let (pairs, rest) = bytes.as_chunks::<2>();
    if !rest.is_empty() {
        return Err(Error::Inconsistent(
            "a catalog name has an odd number of bytes",
        ));
    }
    char::decode_utf16(pairs.iter().map(|pair| u16::from_le_bytes(*pair)))
        .collect::<Result<String, _>>()
        .map_err(|_| Error::Inconsistent("a catalog name is not UTF-16"))
}

fn name_of(record: &FixedVarRecord<'_>) -> Result<String, Error> {
    match record.variables.first() {
        Some(variable) if variable.complex => {
            Err(Error::Inconsistent("a catalog name is stored off the row"))
        }
        Some(variable) => utf16_name(variable.data),
        None => Ok(String::new()),
    }
}

fn differ_only_in_case(stored: &str, wanted: &str) -> bool {
    stored != wanted && stored.to_lowercase() == wanted.to_lowercase()
}

fn for_each_allocation_unit<S: PageSource>(
    reader: &mut PageReader<S>,
    first: PagePtr,
    visit: impl FnMut(&FixedVarRecord<'_>) -> Result<(), Error>,
) -> Result<(), Error> {
    reader.note_allocation_unit(SYSALLOCUNITS_UNIT)?;
    reader.note_object(SYSALLOCUNITS)?;
    for_each_record_in_chain(reader, first, SYSALLOCUNITS_UNIT, visit)
}

fn for_each_catalog_record<S: PageSource>(
    reader: &mut PageReader<S>,
    object: i32,
    unit: &AllocUnit,
    visit: impl FnMut(&FixedVarRecord<'_>) -> Result<(), Error>,
) -> Result<(), Error> {
    reader.note_object(object)?;
    for_each_record(reader, ScanSource::Clustered(unit), visit)
}

fn units_of<S: PageSource>(
    reader: &mut PageReader<S>,
    first: PagePtr,
    owners: &[i64],
) -> Result<Vec<AllocUnit>, Error> {
    let mut found: Vec<AllocUnit> = Vec::new();
    for_each_allocation_unit(reader, first, |record| {
        let owner = i64_at(record.fixed, 9)
            .ok_or(Error::Inconsistent("an allocation unit record is cut off"))?;
        if !owners.contains(&owner) {
            return Ok(());
        }
        let unit = AllocUnit::from_fixed(record.fixed)?;
        if unit.kind != UnitKind::InRow {
            return Ok(());
        }
        if found.iter().any(|known| known.owner == owner) {
            return Err(Error::Inconsistent(
                "sysallocunits has two in-row units for one rowset",
            ));
        }
        found.push(unit);
        Ok(())
    })?;
    Ok(found)
}

impl Catalog {
    pub fn open<S: PageSource>(reader: &mut PageReader<S>) -> Result<Catalog, Error> {
        let first = boot_pointer(reader)?;
        let rowsets =
            units_of(reader, first, &[SYSROWSETS_ROWSET])?
                .pop()
                .ok_or(Error::Inconsistent(
                    "sysallocunits has no unit for sysrowsets",
                ))?;
        let mut wanted: [(i32, Option<Rowset>); 3] =
            [(SYSSCHOBJS, None), (SYSCOLPARS, None), (SYSRSCOLS, None)];
        for_each_catalog_record(reader, SYSROWSETS, &rowsets, |record| {
            let rowset = Rowset::from_fixed(record.fixed)?;
            if rowset.index > 1 {
                return Ok(());
            }
            for (object, slot) in &mut wanted {
                if *object != rowset.object {
                    continue;
                }
                match slot {
                    Some(known) if known.index == rowset.index => {
                        return Err(Error::Inconsistent(
                            "sysrowsets has two rowsets for one system table",
                        ));
                    }
                    Some(known) if known.index > rowset.index => {}
                    _ => *slot = Some(rowset),
                }
            }
            Ok(())
        })?;
        let ids: Vec<i64> = wanted
            .iter()
            .map(|(_, slot)| slot.map(|rowset| rowset.id))
            .collect::<Option<_>>()
            .ok_or(Error::Inconsistent(
                "sysrowsets has no rowset for a system table that is needed",
            ))?;
        let units = units_of(reader, first, &ids)?;
        let unit_for = |rowset: i64| {
            units
                .iter()
                .find(|unit| unit.owner == rowset)
                .cloned()
                .ok_or(Error::Inconsistent(
                    "sysallocunits has no unit for a system table that is needed",
                ))
        };
        let [objects, columns, layouts] = ids.as_slice() else {
            return Err(Error::Inconsistent("the catalog tables were not all found"));
        };
        Ok(Catalog {
            first_allocation_page: first,
            rowsets,
            objects: unit_for(*objects)?,
            columns: unit_for(*columns)?,
            layouts: unit_for(*layouts)?,
        })
    }

    pub fn sysallocunits_first_page(&self) -> PagePtr {
        self.first_allocation_page
    }

    pub fn table<S: PageSource>(
        &self,
        reader: &mut PageReader<S>,
        schema: &str,
        name: &str,
    ) -> Result<TableDef, Error> {
        if !schema.eq_ignore_ascii_case("dbo") {
            return Err(Error::Unsupported("a schema other than dbo"));
        }
        let (object_id, stored_name) = self.find_object(reader, schema, name)?;
        let rowset = self.find_rowset(reader, object_id)?;
        let mut units = self.find_units(reader, rowset.id)?;
        units.sort_by_key(|unit| match unit.kind {
            UnitKind::InRow => 0,
            UnitKind::Lob => 1,
            UnitKind::RowOverflow => 2,
        });
        let sys_columns = self.read_columns(reader, object_id)?;
        let physical = self.read_layout(reader, rowset.id)?;
        let compressed = rowset.compression != 0;
        let (columns, layout) = assemble(&sys_columns, &physical, compressed)?;
        let mut table = TableDef::new(object_id, stored_name, columns, layout, compressed, units)
            .with_catalog_row_count(u64::try_from(rowset.rows).ok());
        table.clustered = rowset.index == 1;
        Ok(table)
    }

    pub fn tables<S: PageSource>(
        &self,
        reader: &mut PageReader<S>,
    ) -> Result<Vec<TableName>, Error> {
        let mut tables: Vec<TableName> = Vec::new();
        for_each_catalog_record(reader, SYSSCHOBJS, &self.objects, |record| {
            let fixed = record.fixed;
            if fixed.get(13..15) != Some(b"U ") {
                return Ok(());
            }
            if tables.len() >= MAX_TABLES {
                return Err(Error::Limit("user tables of a database"));
            }
            let cut = || Error::Inconsistent("a sysschobjs record is cut off");
            tables.push(TableName {
                object_id: i32_at(fixed, 0).ok_or_else(cut)?,
                schema_id: i32_at(fixed, 4).ok_or_else(cut)?,
                name: name_of(record)?,
            });
            Ok(())
        })?;
        tables.sort_by_key(|table| table.object_id);
        Ok(tables)
    }

    pub fn objects_where<S: PageSource>(
        &self,
        reader: &mut PageReader<S>,
        mut keep: impl FnMut(&ObjectName) -> bool,
    ) -> Result<Vec<ObjectName>, Error> {
        let mut objects: Vec<ObjectName> = Vec::new();
        for_each_catalog_record(reader, SYSSCHOBJS, &self.objects, |record| {
            let fixed = record.fixed;
            let cut = || Error::Inconsistent("a sysschobjs record is cut off");
            let Some(kind) = fixed
                .get(13..15)
                .and_then(|kind| <[u8; 2]>::try_from(kind).ok())
            else {
                return Ok(());
            };
            let name = match name_of(record) {
                Ok(name) => name,
                Err(error) if kind == *b"U " => return Err(error),
                Err(_) => return Ok(()),
            };
            let object = ObjectName {
                object_id: i32_at(fixed, 0).ok_or_else(cut)?,
                schema_id: i32_at(fixed, 4).ok_or_else(cut)?,
                kind,
                name,
            };
            if keep(&object) {
                if objects.len() >= MAX_TABLES {
                    return Err(Error::Limit("objects of a database that were asked for"));
                }
                objects.push(object);
            }
            Ok(())
        })?;
        objects.sort_by_key(|object| object.object_id);
        Ok(objects)
    }

    fn find_object<S: PageSource>(
        &self,
        reader: &mut PageReader<S>,
        schema: &str,
        name: &str,
    ) -> Result<(i32, String), Error> {
        let mut exact: Vec<(i32, String)> = Vec::new();
        let mut other_case = false;
        for_each_catalog_record(reader, SYSSCHOBJS, &self.objects, |record| {
            let fixed = record.fixed;
            if fixed.get(13..15) != Some(b"U ") || i32_at(fixed, 4) != Some(DBO_SCHEMA_ID) {
                return Ok(());
            }
            let id =
                i32_at(fixed, 0).ok_or(Error::Inconsistent("a sysschobjs record is cut off"))?;
            let stored = name_of(record)?;
            if stored == name {
                exact.push((id, stored));
            } else if differ_only_in_case(&stored, name) {
                other_case = true;
            }
            Ok(())
        })?;
        match exact.len() {
            0 if other_case => Err(Error::Unsupported(
                "a table whose name differs from the one asked for only in case (this reader does not know the collation of the database, so it does not take them for one name)",
            )),
            0 => Err(Error::NotFound {
                schema: schema.to_owned(),
                name: name.to_owned(),
            }),
            1 => exact.pop().ok_or(Error::Inconsistent("a table was lost")),
            _ => Err(Error::Inconsistent(
                "two user tables of schema dbo have the same name",
            )),
        }
    }

    fn find_rowset<S: PageSource>(
        &self,
        reader: &mut PageReader<S>,
        object_id: i32,
    ) -> Result<Rowset, Error> {
        let mut rowsets: Vec<Rowset> = Vec::new();
        for_each_catalog_record(reader, SYSROWSETS, &self.rowsets, |record| {
            let rowset = Rowset::from_fixed(record.fixed)?;
            if rowset.object == object_id && rowset.index <= 1 {
                rowsets.push(rowset);
            }
            Ok(())
        })?;
        if rowsets
            .iter()
            .any(|rowset| rowset.compression >= COLUMNSTORE_LEVEL)
        {
            return Err(Error::Unsupported("a columnstore table"));
        }
        match rowsets.as_slice() {
            [] => Err(Error::Inconsistent("a table has no rowset")),
            [only] => Ok(*only),
            _ => Err(Error::Unsupported("a partitioned table")),
        }
    }

    fn find_units<S: PageSource>(
        &self,
        reader: &mut PageReader<S>,
        rowset: i64,
    ) -> Result<Vec<AllocUnit>, Error> {
        let mut units: Vec<AllocUnit> = Vec::new();
        for_each_allocation_unit(reader, self.first_allocation_page, |record| {
            if i64_at(record.fixed, 9) != Some(rowset) {
                return Ok(());
            }
            let unit = AllocUnit::from_fixed(record.fixed)?;
            if units.iter().any(|known| known.kind == unit.kind) {
                return Err(Error::Inconsistent(
                    "sysallocunits has two units of one kind for a rowset",
                ));
            }
            units.push(unit);
            Ok(())
        })?;
        Ok(units)
    }

    fn read_columns<S: PageSource>(
        &self,
        reader: &mut PageReader<S>,
        object_id: i32,
    ) -> Result<Vec<SysColumn>, Error> {
        let mut columns: Vec<SysColumn> = Vec::new();
        for_each_catalog_record(reader, SYSCOLPARS, &self.columns, |record| {
            let fixed = record.fixed;
            let cut = || Error::Inconsistent("a syscolpars record is cut off");
            if i32_at(fixed, 0).ok_or_else(cut)? != object_id
                || i16_at(fixed, 4).ok_or_else(cut)? != 0
            {
                return Ok(());
            }
            if columns.len() >= MAX_COLUMNS {
                return Err(Error::Limit("columns of a table"));
            }
            let column_id = u32::try_from(i32_at(fixed, 6).ok_or_else(cut)?)
                .map_err(|_| Error::Inconsistent("a column has a negative id"))?;
            if columns.iter().any(|known| known.column_id == column_id) {
                return Err(Error::Inconsistent("syscolpars has a column twice"));
            }
            columns.push(SysColumn {
                column_id,
                name: name_of(record)?,
                xtype: u8_at(fixed, 10).ok_or_else(cut)?,
                status: u32_at(fixed, 23).ok_or_else(cut)?,
            });
            Ok(())
        })?;
        columns.sort_by_key(|column| column.column_id);
        Ok(columns)
    }

    fn read_layout<S: PageSource>(
        &self,
        reader: &mut PageReader<S>,
        rowset: i64,
    ) -> Result<Vec<PhysColumn>, Error> {
        let mut physical: Vec<PhysColumn> = Vec::new();
        for_each_catalog_record(reader, SYSRSCOLS, &self.layouts, |record| {
            let fixed = record.fixed;
            let cut = || Error::Inconsistent("a sysrscols record is cut off");
            if i64_at(fixed, 0).ok_or_else(cut)? != rowset {
                return Ok(());
            }
            if physical.len() >= MAX_COLUMNS {
                return Err(Error::Limit("physical columns of a rowset"));
            }
            let column_id = u32_at(fixed, 8).ok_or_else(cut)?;
            let status = u32_at(fixed, 36).ok_or_else(cut)?;
            physical.push(PhysColumn {
                column_id: column_id & RSCOL_ID_MASK,
                dropped: status & RSCOL_DROPPED_STATUS != 0 || column_id & RSCOL_DROPPED_ID != 0,
                internal: column_id & RSCOL_INTERNAL_ID != 0,
                sparse: status & RSCOL_SPARSE != 0,
                default_for_absent: status & RSCOL_DEFAULT_FOR_ABSENT != 0,
                ti: u32_at(fixed, 24).ok_or_else(cut)?,
                leaf_offset: i16_at(fixed, 40).ok_or_else(cut)?,
                null_bit: u16_at(fixed, 44).ok_or_else(cut)?,
                bit_position: u16_at(fixed, 48).ok_or_else(cut)?,
            });
            Ok(())
        })?;
        if physical.is_empty() {
            return Err(Error::Inconsistent("a rowset has no physical layout"));
        }
        physical.sort_by_key(|column| column.null_bit);
        Ok(physical)
    }
}

pub fn accelerated_recovery<S: PageSource>(reader: &mut PageReader<S>) -> Result<bool, Error> {
    let mut page = [0_u8; PAGE_SIZE];
    if reader.read(BOOT_PAGE, &[TYPE_BOOT], &mut page)?.is_none() {
        return Err(Error::Inconsistent("the backup has no boot page"));
    }
    let flag = page
        .get(BOOT_ADR_AT)
        .copied()
        .ok_or(Error::Inconsistent("the boot page is cut off"))?;
    Ok(flag != 0)
}

fn boot_pointer<S: PageSource>(reader: &mut PageReader<S>) -> Result<PagePtr, Error> {
    let mut page = [0_u8; PAGE_SIZE];
    if reader.read(BOOT_PAGE, &[TYPE_BOOT], &mut page)?.is_none() {
        return Err(Error::Inconsistent("the backup has no boot page"));
    }
    let pointer = PagePtr::from_bytes(&page, BOOT_SYSALLOCUNITS_AT)
        .ok_or(Error::Inconsistent("the boot page is cut off"))?;
    if pointer.is_null() || pointer.file != 1 {
        return Err(Error::Inconsistent(
            "the boot page does not point at the first page of sysallocunits",
        ));
    }
    Ok(pointer)
}

fn refusal_of_kind(status: u32) -> Option<&'static str> {
    if status & COLUMN_COMPUTED != 0 {
        Some("a computed column")
    } else if status & COLUMN_SPARSE != 0 {
        Some("a sparse column")
    } else if status & COLUMN_SET != 0 {
        Some("a column set")
    } else if status & COLUMN_FILESTREAM != 0 {
        Some("a FILESTREAM column")
    } else {
        None
    }
}

fn refusal_of(status: u32, ty: SqlType) -> Option<&'static str> {
    refusal_of_kind(status).or_else(|| {
        (matches!(ty, SqlType::Char | SqlType::Binary) && status & COLUMN_ANSI_PADDED == 0).then_some(
            "a char or binary column created with ANSI_PADDING OFF (the server trims its values when it reads them)",
        )
    })
}

fn check_null_bits(physical: &[PhysColumn]) -> Result<(), Error> {
    let placed = || physical.iter().filter(|column| !column.sparse);
    let mut live: Vec<u16> = placed()
        .filter(|column| !column.dropped)
        .map(|column| column.null_bit)
        .collect();
    live.sort_unstable();
    if live.windows(2).any(|pair| pair.first() == pair.get(1)) {
        return Err(Error::Inconsistent(
            "two live columns of a rowset have one null bit",
        ));
    }
    let mut all: Vec<u16> = placed().map(|column| column.null_bit).collect();
    all.sort_unstable();
    all.dedup();
    if all
        .iter()
        .enumerate()
        .any(|(index, bit)| usize::from(*bit) != index + 1)
    {
        return Err(Error::Inconsistent(
            "the null bits of the physical columns of a rowset are not 1 to N",
        ));
    }
    Ok(())
}

fn assemble(
    sys_columns: &[SysColumn],
    physical: &[PhysColumn],
    compressed: bool,
) -> Result<(Vec<ColumnDef>, Vec<PhysSlot>), Error> {
    if !compressed {
        check_null_bits(physical)?;
    }
    let mut found: Vec<Option<(ColumnType, bool)>> = vec![None; sys_columns.len()];
    let mut layout: Vec<PhysSlot> = Vec::new();
    layout
        .try_reserve_exact(physical.len())
        .map_err(|_| Error::Limit("physical columns of a rowset"))?;
    for column in physical {
        if column.dropped || column.internal || column.column_id == 0 {
            layout.push(PhysSlot {
                null_bit: column.null_bit,
                storage: UNUSED,
                column: None,
            });
            continue;
        }
        let index = sys_columns
            .binary_search_by_key(&column.column_id, |known| known.column_id)
            .map_err(|_| Error::Inconsistent("a physical column has no entry in syscolpars"))?;
        let sys = sys_columns
            .get(index)
            .ok_or(Error::Inconsistent("a column was lost"))?;
        let stored_as = u8::try_from(column.ti & 0xff).unwrap_or(0);
        let xtype = if sys.xtype == CLR_TYPE {
            CLR_TYPE
        } else {
            stored_as
        };
        if xtype != sys.xtype {
            return Err(Error::Inconsistent(
                "sysrscols and syscolpars give a column two different types",
            ));
        }
        let this = column_type(xtype, column.ti)?;
        let slot = found
            .get_mut(index)
            .ok_or(Error::Inconsistent("a column was lost"))?;
        if slot.replace((this, column.default_for_absent)).is_some() {
            return Err(Error::Inconsistent("sysrscols has a column twice"));
        }
        let read = !compressed
            && refusal_of(sys.status, this.ty).is_none()
            && !matches!(this.ty, SqlType::Unsupported(_));
        layout.push(PhysSlot {
            null_bit: column.null_bit,
            storage: if read {
                storage_of(column, this)?
            } else {
                UNUSED
            },
            column: Some(index),
        });
    }
    let mut columns = Vec::new();
    columns
        .try_reserve_exact(sys_columns.len())
        .map_err(|_| Error::Limit("columns of a table"))?;
    for (sys, entry) in sys_columns.iter().zip(found) {
        let (ty, max_len, default_for_absent) = match entry {
            Some((this, default_for_absent)) => (this.ty, this.max_len, default_for_absent),
            None => match refusal_of_kind(sys.status) {
                Some(reason) => (SqlType::Unsupported(reason), 0, false),
                None => {
                    return Err(Error::Inconsistent(
                        "a column of the table has no physical storage",
                    ));
                }
            },
        };
        columns.push(ColumnDef {
            name: sys.name.clone(),
            ty,
            nullable: sys.status & COLUMN_NOT_NULL == 0,
            max_len,
            column_id: sys.column_id,
            default_for_absent,
            refused: refusal_of(sys.status, ty),
        });
    }
    Ok((columns, layout))
}

fn storage_of(column: &PhysColumn, found: ColumnType) -> Result<Storage, Error> {
    if matches!(found.ty, SqlType::Unsupported(_)) {
        return Ok(UNUSED);
    }
    let offset = u16::try_from(column.leaf_offset)
        .ok()
        .filter(|offset| *offset > 0);
    if found.ty == SqlType::Bit {
        let offset = offset.ok_or(Error::Inconsistent("a bit column without a fixed offset"))?;
        let bit = u8::try_from(column.bit_position)
            .ok()
            .filter(|bit| *bit < 8)
            .ok_or(Error::Inconsistent(
                "a bit column with a bit position above 7",
            ))?;
        return Ok(Storage::Bit { offset, bit });
    }
    let width = found.ty.storage_width(found.max_len);
    if let Some(offset) = offset {
        let width = width.ok_or(Error::Inconsistent(
            "a variable-length type at a fixed offset",
        ))?;
        return Ok(Storage::Fixed { offset, width });
    }
    if column.leaf_offset == 0 {
        return Err(Error::Inconsistent("a physical column without an offset"));
    }
    if width.is_some() && !matches!(found.ty, SqlType::Char | SqlType::NChar | SqlType::Binary) {
        return Err(Error::Inconsistent(
            "a fixed-width type stored as a variable-length column",
        ));
    }
    let ordinal = column
        .leaf_offset
        .checked_neg()
        .and_then(|ordinal| u16::try_from(ordinal).ok())
        .ok_or(Error::Inconsistent(
            "a variable column with an impossible ordinal",
        ))?;
    Ok(Storage::Variable { ordinal })
}
