use super::{Control, Error, Inventory, Item, Projection, ROW_BYTES, Result, SCHEMA_BYTES};
use memory_budget::{BudgetError, BudgetedText, Category, Reservation};
use onecd::{BlobVal, Storage, Value};
use std::{fs::File, io, path::Path, sync::Arc};

struct CheckedStorage {
    file: File,
    control: Control,
}

#[cfg(unix)]
fn read_exact_at(file: &File, bytes: &mut [u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(bytes, offset)
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut bytes: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !bytes.is_empty() {
        match file.seek_read(bytes, offset) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(read) => {
                bytes = bytes
                    .get_mut(read..)
                    .ok_or_else(|| io::Error::other("file read exceeded the buffer"))?;
                offset = offset.saturating_add(read as u64);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

impl Storage for CheckedStorage {
    fn len(&self) -> io::Result<u64> {
        self.control
            .check()
            .map_err(|_| io::Error::from(io::ErrorKind::Other))?;
        let len = self.file.metadata().map(|metadata| metadata.len());
        self.control
            .check()
            .map_err(|_| io::Error::from(io::ErrorKind::Other))?;
        len
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<()> {
        self.control
            .check()
            .map_err(|_| io::Error::from(io::ErrorKind::Other))?;
        let mut offset = off;
        for part in buf.chunks_mut(64 << 10) {
            let next = offset
                .checked_add(part.len() as u64)
                .ok_or_else(|| io::Error::from(io::ErrorKind::Other))?;
            self.control
                .read_chunk()
                .map_err(|_| io::Error::from(io::ErrorKind::Other))?;
            let read = read_exact_at(&self.file, part, offset);
            self.control
                .check()
                .map_err(|_| io::Error::from(io::ErrorKind::Other))?;
            read?;
            offset = next;
        }
        Ok(())
    }
    fn write_at(&self, _: u64, _: &[u8]) -> io::Result<()> {
        Err(io::ErrorKind::PermissionDenied.into())
    }
    fn set_len(&self, _: u64) -> io::Result<()> {
        Err(io::ErrorKind::PermissionDenied.into())
    }
    fn sync(&self) -> io::Result<()> {
        Ok(())
    }
}
pub struct ReadSource {
    ib: crate::raw::RawIb,
    control: Control,
    _external: Reservation,
}
fn arc_bytes<T>() -> Result<u64> {
    let header = std::mem::size_of::<usize>()
        .checked_mul(2)
        .ok_or(BudgetError::Overflow)?;
    let layout = std::alloc::Layout::from_size_align(header, std::mem::align_of::<usize>())
        .map_err(|_| BudgetError::Overflow)?;
    let (layout, _) = layout
        .extend(std::alloc::Layout::new::<T>())
        .map_err(|_| BudgetError::Overflow)?;
    u64::try_from(layout.pad_to_align().size()).map_err(|_| BudgetError::Overflow.into())
}
fn engine(control: &Control, error: onecd::Error) -> Error {
    if let Err(stop) = control.check() {
        return Error::Stopped(stop);
    }
    match error {
        onecd::Error::Memory(e) => Error::Memory(e),
        onecd::Error::Io(e) => Error::Io(e.kind()),
        _ => Error::Invalid("infobase source"),
    }
}
fn service(control: &Control, error: crate::Error) -> Error {
    if let Err(stop) = control.check() {
        return Error::Stopped(stop);
    }
    match error {
        crate::Error::Memory(e) => Error::Memory(e),
        crate::Error::Engine(e) => engine(control, e),
        _ => Error::Invalid("service metadata"),
    }
}
impl ReadSource {
    pub fn open(path: &Path, control: Control) -> Result<Self> {
        Self::open_with_cache(path, control, 8 << 20)
    }
    pub fn open_with_cache(path: &Path, control: Control, cache_bytes: usize) -> Result<Self> {
        Self::open_checked(path, control, cache_bytes, |_| Ok(()))
    }

    pub fn open_checked(
        path: &Path,
        control: Control,
        cache_bytes: usize,
        validate: impl FnOnce(&File) -> Result<()>,
    ) -> Result<Self> {
        control.check()?;
        let bytes = arc_bytes::<CheckedStorage>()?
            .checked_add(arc_bytes::<onecd::Database>()?)
            .ok_or(BudgetError::Overflow)?;
        let external = control
            .budget()
            .try_reserve(Category::ReaderWorking, bytes)
            .map_err(BudgetError::from)?;
        let file = {
            let _path = super::path_open_scratch(control.budget(), path)?;
            File::open(path)
        };
        control.check()?;
        let file = file.map_err(|e| Error::Io(e.kind()))?;
        validate(&file)?;
        let storage = Arc::new(CheckedStorage {
            file,
            control: control.clone(),
        });
        let db = onecd::Database::open_storage_readonly_bounded(
            storage,
            onecd::OpenOptions {
                memory_budget: Some(control.budget().clone()),
                cache_bytes: cache_bytes.min(8 << 20),
                locking: onecd::Locking::None,
                ..onecd::OpenOptions::read_only()
            },
        )
        .map_err(|e| engine(&control, e))?;
        control.check()?;
        Ok(Self {
            ib: crate::raw::RawIb::from_db(Arc::new(db)),
            control,
            _external: external,
        })
    }
    pub fn inventory(&self) -> Result<Inventory> {
        let txn = self.ib.db().read();
        self.inventory_in(&txn)
    }

    fn inventory_in(&self, txn: &onecd::ReadTxn) -> Result<Inventory> {
        self.control.check()?;
        let budget = self.control.budget();
        let slot = || {
            self.control
                .slot()
                .map_err(|_| crate::Error::UnsupportedStatic("inventory read stopped"))
        };
        let text = crate::raw::managed::dbschema_text_in_budgeted_checked(
            txn,
            budget,
            SCHEMA_BYTES,
            &slot,
        )
        .map_err(|e| service(&self.control, e))?;
        if let Some((main, _)) = crate::raw::managed::schema_storage_main_in_budgeted_checked(
            txn,
            budget,
            SCHEMA_BYTES,
            &slot,
        )
        .map_err(|e| service(&self.control, e))?
            && *main != *text
        {
            return Err(Error::Invalid("main storage schema differs from DBSCHEMA"));
        }
        let root =
            brace::parse_bytes_budgeted_limited(&text, budget, Category::Metadata, 65_536, 128)
                .map_err(|e| match e {
                    brace::LimitedParseError::Memory(e) => Error::Memory(e),
                    brace::LimitedParseError::Policy { .. } => Error::Limit("main schema reader"),
                    brace::LimitedParseError::Parse(_) => Error::Invalid("main schema syntax"),
                })?;
        Projection::from_text(&root, &self.control)?;
        drop(root);
        drop(text);
        let mut result = Inventory::new(budget)?;
        for (name, ngs) in [("_ExtensionsInfo", false), ("_ExtensionsInfoNGS", true)] {
            self.control.check()?;
            let table = txn
                .find_table(name)
                .map_err(|e| engine(&self.control, e))?
                .ok_or(Error::Invalid("missing declared inventory table"))?;
            super::schema::validate_physical(table.descr())?;
            for slot in 1..table.num_slots() {
                self.control.slot()?;
                if let Some(row) = table
                    .get_managed(slot, budget, Category::Metadata, ROW_BYTES)
                    .map_err(|e| engine(&self.control, e))?
                {
                    if ngs {
                        return Err(Error::Unsupported("pending next-generation inventory"));
                    }
                    result.push(item(&row, budget)?)?;
                }
            }
        }
        self.control.check()?;
        result.finish();
        self.control.check()?;
        Ok(result)
    }

    pub fn with_extension_snapshot<T>(
        &self,
        id: brace::Guid,
        inspect: impl FnOnce(Result<Option<[u8; 20]>>, &onecd::ReadTxn) -> T,
    ) -> Result<T> {
        let txn = self.ib.db().read();
        let inventory = self.inventory_in(&txn)?;
        if !inventory.items().iter().any(|item| item.id == id) {
            return Err(Error::Invalid("selected extension is not installed"));
        }
        drop(inventory);
        Ok(inspect(self.generation(&txn, id), &txn))
    }

    fn generation(&self, txn: &onecd::ReadTxn, id: brace::Guid) -> Result<Option<[u8; 20]>> {
        let budget = self.control.budget();
        let table = txn
            .find_table("_ExtensionsInfo")
            .map_err(|e| engine(&self.control, e))?
            .ok_or(Error::Invalid("missing declared inventory table"))?;
        for slot in 1..table.num_slots() {
            self.control.slot()?;
            let Some(row) = table
                .get_managed(slot, budget, Category::Metadata, ROW_BYTES)
                .map_err(|e| engine(&self.control, e))?
            else {
                continue;
            };
            let Some(Value::Binary(bytes)) = row.first() else {
                return Err(Error::Invalid("extension ID type"));
            };
            let raw = bytes
                .as_slice()
                .try_into()
                .map_err(|_| Error::Invalid("extension ID length"))?;
            if ibschema::values::RefId(raw).to_uuid() != id {
                continue;
            }
            let key = match row.get(6) {
                Some(Value::Image(BlobVal::Data(bytes))) => super::generation_key(bytes, budget)?,
                Some(Value::Image(BlobVal::Ref(reference))) => {
                    if reference.len as usize > super::GENERATION_BYTES {
                        return Err(Error::Limit("extension generation value"));
                    }
                    let bytes = table
                        .blob_bytes_managed(
                            *reference,
                            budget,
                            Category::ReaderWorking,
                            super::GENERATION_BYTES as u64 + 256,
                        )
                        .map_err(|e| engine(&self.control, e))?;
                    super::generation_key(&bytes, budget)?
                }
                _ => return Err(Error::Invalid("extension locator type")),
            };
            return Ok(key);
        }
        Err(Error::Changed)
    }
}
fn integer(value: &Value) -> Result<i64> {
    let Value::Numeric(n) = value else {
        return Err(Error::Invalid("inventory integer type"));
    };
    if n.scale() != 0 {
        return Err(Error::Invalid("fractional inventory integer"));
    }
    let n_abs = n
        .digits()
        .iter()
        .try_fold(0i64, |n, d| n.checked_mul(10)?.checked_add(i64::from(*d)))
        .ok_or(Error::Invalid("inventory integer overflow"))?;
    Ok(if n.is_negative() { -n_abs } else { n_abs })
}
fn omitted_blob(value: &Value, text: bool) -> Result<()> {
    let blob = match (value, text) {
        (Value::Image(b), false) | (Value::NText(b), true) => b,
        _ => return Err(Error::Invalid("omitted inventory value type")),
    };
    let len = match blob {
        BlobVal::Data(b) => b.len() as u64,
        BlobVal::Ref(r) => {
            if r.start == 0 && r.len != 0 {
                return Err(Error::Invalid("omitted inventory blob reference"));
            }
            u64::from(r.len)
        }
    };
    if len > ROW_BYTES {
        return Err(Error::Limit("omitted inventory value"));
    }
    if text && !len.is_multiple_of(2) {
        return Err(Error::Invalid("omitted text byte length"));
    }
    Ok(())
}
fn item(row: &[Value], budget: &memory_budget::MemoryBudget) -> Result<Item> {
    let [
        id,
        order,
        name,
        updated,
        purpose,
        scope,
        zipped,
        master,
        distributed,
        version,
    ] = row
    else {
        return Err(Error::Invalid("inventory row shape"));
    };
    let Value::Binary(id) = id else {
        return Err(Error::Invalid("inventory ID type"));
    };
    let id: [u8; 16] = id
        .as_slice()
        .try_into()
        .map_err(|_| Error::Invalid("inventory ID length"))?;
    let Value::VarChar(name) = name else {
        return Err(Error::Invalid("inventory name type"));
    };
    if name.len() > 1020 || name.encode_utf16().count() > 255 {
        return Err(Error::Limit("extension name"));
    }
    let mut owned = BudgetedText::try_with_capacity(budget, Category::Metadata, 1020, name.len())?;
    owned.try_push_str(name)?;
    let Value::DateTime(updated) = updated else {
        return Err(Error::Invalid("inventory calendar type"));
    };
    let Value::Bool(distributed) = distributed else {
        return Err(Error::Invalid("inventory distributed flag type"));
    };
    if !matches!(version, Value::RowVersion(_)) {
        return Err(Error::Invalid("inventory row-version type"));
    }
    omitted_blob(zipped, false)?;
    omitted_blob(master, true)?;
    Ok(Item {
        id: ibschema::values::RefId(id).to_uuid(),
        name: owned,
        order: integer(order)?,
        purpose_code: integer(purpose)?,
        scope_code: integer(scope)?,
        update_time: (updated.year != 0).then_some(*updated),
        used_in_distributed_infobase: *distributed,
    })
}
