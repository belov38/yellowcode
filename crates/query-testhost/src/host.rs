use bsl_prim::Collation;
use ibschema::dbnames::DbNames;
use ibschema::logical::LogicalSchema;
use ibschema::names::StdField;
use ibschema::storage::{StorageMap, TableId};
use ibschema::store::Store;
use infobase::{
    Infobase, MaybeCharged, OpenMode, OpenOptions, SchemaCheck, StartupOwners, Warning,
};
use md_model::{LoadProfile, MdKind};
use onecd::Locking;
use query_ir::host::{HostError, PredefinedName, PresentationInput};
use query_ir::schema::StdRole;
use query_ir::{
    BindHost, QStr, QTableId, QValue, QuerySchema, RefAtom, RefId, StorageBinding, ref_id,
};
use query_syntax::Lang;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct TestBase {
    db: Arc<onecd::Database>,
    pub(crate) store: Arc<dyn Store>,
    pub(crate) map: Arc<StorageMap>,
    pub schema_notes: Vec<String>,
    pub(crate) schema: Arc<QuerySchema>,
    pub(crate) storage: StorageBinding,
    pub(crate) collation: Arc<dyn Collation>,
    pub(crate) by_number: HashMap<u32, RefAtom>,
    pub(crate) predefined_names: HashMap<(u32, RefId), String>,
    pub(crate) predefined_refs: HashMap<(QTableId, String), QValue>,
    pub(crate) predefined_ids: HashMap<QTableId, Vec<([u8; 16], String)>>,
    pub(crate) enum_names: HashMap<(u32, RefId), String>,
    pub(crate) enum_ids: HashMap<(u32, String), RefId>,
    warnings: Vec<Warning>,
    bounds: crate::batch_memory::SchemaBounds,
    open_inputs: crate::open_model::HostInputs,
    _dbnames: MaybeCharged<DbNames>,
    _schema: MaybeCharged<Arc<LogicalSchema>>,
    metadata: Option<memory_budget::Reservation>,
}

impl TestBase {
    pub fn open(dir: &Path) -> Result<TestBase, String> {
        Self::open_options(dir, None, None, LoadProfile::Full).map_err(|e| e.to_string())
    }

    pub fn open_profile(dir: &Path, profile: LoadProfile) -> Result<TestBase, String> {
        Self::open_options(dir, None, None, profile).map_err(|e| e.to_string())
    }

    pub fn open_with_memory(
        dir: &Path,
        budget: memory_budget::MemoryBudget,
        cache_bytes: usize,
    ) -> Result<TestBase, Box<dyn std::error::Error + Send + Sync>> {
        Self::open_options(dir, Some(budget), Some(cache_bytes), LoadProfile::Query)
    }

    fn open_options(
        dir: &Path,
        memory_budget: Option<memory_budget::MemoryBudget>,
        cache_bytes: Option<usize>,
        load_profile: LoadProfile,
    ) -> Result<TestBase, Box<dyn std::error::Error + Send + Sync>> {
        let mut opts = OpenOptions {
            memory_budget,
            locking: Locking::None,
            schema_check: SchemaCheck::Warn,
            load_profile,
            ..OpenOptions::mode(OpenMode::ReadOnly)
        };
        if let Some(bytes) = cache_bytes {
            opts.cache_bytes = bytes;
        }
        let ib = Infobase::open(dir, opts).map_err(|e| OpenFailure {
            path: dir.to_path_buf(),
            source: e,
        })?;
        let StartupOwners {
            store,
            map,
            config,
            warnings,
            db,
            dbnames,
            schema: logical,
            metadata,
            transient,
        } = ib.into_startup_owners();
        let cfg = config.ok_or_else(|| format!("{}: no configuration", dir.display()))?;
        let schema_notes: Vec<String> = warnings
            .iter()
            .flat_map(|w| match w {
                Warning::SchemaMismatch(v) => v.clone(),
                _ => Vec::new(),
            })
            .collect();
        let inputs = crate::open_model::HostInputs::of(&map, &cfg);
        let grow = |bytes: u64| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            if let Some(lease) = &metadata {
                lease
                    .try_grow_or_evict(bytes, &*db)
                    .map_err(|m| infobase::Error::Memory(memory_budget::BudgetError::Limit(m)))?;
                infobase::open::probe_input("grow", bytes);
            }
            Ok(())
        };
        let shrink = |bytes: u64| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            if let Some(lease) = &metadata {
                lease
                    .shrink(bytes)
                    .map_err(|m| infobase::Error::Memory(memory_budget::BudgetError::Limit(m)))?;
                infobase::open::probe_input("shrink", bytes);
            }
            Ok(())
        };
        let upper = inputs.query_schema();
        infobase::open::probe("host:query_schema");
        grow(upper)?;
        let schema = QuerySchema::build(&cfg, platform_catalog::catalog())
            .map_err(|e| format!("query schema: {e:?}"))?;
        let exact = inputs.query_schema_with(crate::open_model::HostInputs::fanout_of(&schema));
        shrink(upper.saturating_sub(exact))?;
        grow(exact.saturating_sub(upper))?;
        infobase::open::probe("host:bind_storage");
        grow(inputs.binding())?;
        let storage = schema
            .bind_storage(&*map)
            .map_err(|e| format!("bind storage: {e:?}"))?;
        infobase::open::probe("host:index");
        grow(
            inputs
                .reference_maps()
                .saturating_add(crate::open_model::INDEX_SCAN),
        )?;
        let mut base = TestBase {
            db,
            store,
            map,
            schema_notes,
            schema,
            storage,
            collation: query_testkit::ru_collation(),
            by_number: HashMap::new(),
            predefined_names: HashMap::new(),
            predefined_refs: HashMap::new(),
            predefined_ids: HashMap::new(),
            enum_names: HashMap::new(),
            enum_ids: HashMap::new(),
            warnings,
            bounds: crate::batch_memory::SchemaBounds::default(),
            open_inputs: inputs,
            _dbnames: dbnames,
            _schema: logical,
            metadata,
        };
        base.index(&cfg)?;
        base.bounds = crate::batch_memory::SchemaBounds::of(&base);
        drop(cfg);
        if let Some(lease) = &base.metadata {
            lease
                .shrink(transient.saturating_add(crate::open_model::INDEX_SCAN))
                .map_err(|m| infobase::Error::Memory(memory_budget::BudgetError::Limit(m)))?;
            infobase::open::probe_input(
                "shrink",
                transient.saturating_add(crate::open_model::INDEX_SCAN),
            );
        }
        infobase::open::probe("host:done");
        Ok(base)
    }

    pub fn empty_store(&self) -> TestBase {
        TestBase {
            db: Arc::clone(&self.db),
            store: Arc::new(ibschema::store::mem::MemStore::new(Arc::clone(&self.map))),
            map: Arc::clone(&self.map),
            schema_notes: self.schema_notes.clone(),
            schema: Arc::clone(&self.schema),
            storage: self.storage.clone(),
            collation: Arc::clone(&self.collation),
            by_number: self.by_number.clone(),
            predefined_names: HashMap::new(),
            predefined_refs: HashMap::new(),
            predefined_ids: self.predefined_ids.clone(),
            enum_names: self.enum_names.clone(),
            enum_ids: self.enum_ids.clone(),
            warnings: self.warnings.clone(),
            bounds: self.bounds,
            open_inputs: self.open_inputs,
            _dbnames: MaybeCharged::Legacy((*self._dbnames).clone()),
            _schema: MaybeCharged::Legacy(Arc::clone(&self._schema)),
            metadata: None,
        }
    }

    pub fn metadata_lease(&self) -> Option<&memory_budget::Reservation> {
        self.metadata.as_ref()
    }

    #[doc(hidden)]
    pub fn open_inputs(&self) -> crate::open_model::HostInputs {
        self.open_inputs
    }

    #[doc(hidden)]
    pub fn open_inputs_of(
        map: &StorageMap,
        cfg: &md_model::Configuration,
    ) -> crate::open_model::HostInputs {
        crate::open_model::HostInputs::of(map, cfg)
    }

    pub fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    #[doc(hidden)]
    pub fn open_dump(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let mut line = |s: String| {
            out.push_str(&s);
            out.push('\n');
        };
        line(format!("warnings {:?}", self.warnings));
        line(format!("notes {:?}", self.schema_notes));
        let map = &self.map;
        line(format!("dbschema {:?}", map.schema().to_text().len()));
        for (i, t) in map.tables().iter().enumerate() {
            let id = TableId(u32::try_from(i).unwrap_or(u32::MAX));
            line(format!("table {i} ref {:?} {t:?}", map.ref_number(id)));
        }
        let schema = &self.schema;
        line(format!(
            "qschema script {:?} diagnostics {:?}",
            schema.script(),
            schema.diagnostics()
        ));
        for (i, t) in schema.tables().iter().enumerate() {
            let q = QTableId(u32::try_from(i).unwrap_or(u32::MAX));
            let mut fields = String::new();
            for f in 0..t.fields.len() {
                let f = query_ir::schema::QFieldId(u16::try_from(f).unwrap_or(u16::MAX));
                let _ = write!(fields, "{:?},", self.storage.field(q, f));
            }
            line(format!(
                "qtable {i} storage {:?} fields [{fields}] {t:?}",
                self.storage.table(q)
            ));
        }
        for (i, r) in schema.refs().iter().enumerate() {
            let atom = RefAtom(u32::try_from(i).unwrap_or(u32::MAX));
            let object = map.object(r.object);
            line(format!(
                "ref {i} number {:?} {r:?} storage {object:?}",
                self.storage.ref_number(atom)
            ));
        }
        line(format!(
            "unbound {:?} {:?}",
            self.storage.unbound(),
            self.storage.unbound_fields()
        ));
        fn sorted<K: Ord + std::fmt::Debug, V: std::fmt::Debug>(m: &HashMap<K, V>) -> String {
            let mut v: Vec<_> = m.iter().collect();
            v.sort_by(|a, b| a.0.cmp(b.0));
            format!("{v:?}")
        }
        line(format!("by_number {}", sorted(&self.by_number)));
        line(format!(
            "predefined_names {}",
            sorted(&self.predefined_names)
        ));
        let refs: HashMap<(QTableId, String), String> = self
            .predefined_refs
            .iter()
            .map(|(k, v)| (k.clone(), format!("{v:?}")))
            .collect();
        line(format!("predefined_refs {}", sorted(&refs)));
        line(format!("predefined_ids {}", sorted(&self.predefined_ids)));
        line(format!("enum_names {}", sorted(&self.enum_names)));
        line(format!("enum_ids {}", sorted(&self.enum_ids)));
        line(format!("max_ref_fanout {}", self.max_ref_fanout()));
        out
    }

    pub fn reader_stats(&self) -> onecd::ReaderStats {
        self.db.reader_stats().unwrap_or_default()
    }

    pub fn evict_clean_cache(&self) {
        self.db.evict_clean_cache();
    }

    pub fn memory_budget(&self) -> Option<&memory_budget::MemoryBudget> {
        self.db.memory_budget()
    }

    pub fn evictor(&self) -> Arc<onecd::Database> {
        Arc::clone(&self.db)
    }

    pub fn max_ref_fanout(&self) -> u32 {
        self.bounds.max_ref_fanout
    }

    pub fn schema_bounds(&self) -> crate::batch_memory::SchemaBounds {
        self.bounds
    }

    #[doc(hidden)]
    pub fn fresh_schema_bounds(&self) -> crate::batch_memory::SchemaBounds {
        crate::batch_memory::SchemaBounds::of(self)
    }

    pub fn ref_xml_name(&self, number: u32) -> Option<&str> {
        let atom = self.by_number.get(&number)?;
        Some(self.schema.ref_info(*atom).xml_name.as_str())
    }

    pub fn ref_kinds(&self) -> Vec<(u32, &str)> {
        let mut v: Vec<(u32, &str)> = self
            .by_number
            .iter()
            .map(|(n, a)| (*n, self.schema.ref_info(*a).xml_name.as_str()))
            .collect();
        v.sort_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(&b.0)));
        v
    }

    pub fn named_refs(&self, number: u32) -> Vec<(RefId, String)> {
        let mut v: Vec<(RefId, String)> = self
            .predefined_names
            .iter()
            .chain(self.enum_names.iter())
            .filter(|((n, _), _)| *n == number)
            .map(|((_, id), name)| (*id, name.clone()))
            .collect();
        v.sort_by(|a, b| a.1.cmp(&b.1));
        v
    }

    pub fn schema(&self) -> &Arc<QuerySchema> {
        &self.schema
    }

    pub(crate) fn map(&self) -> &StorageMap {
        &self.map
    }

    #[doc(hidden)]
    pub fn storage_parts(&self) -> (&Arc<StorageMap>, &Arc<dyn Store>) {
        (&self.map, &self.store)
    }

    fn index(&mut self, cfg: &md_model::Configuration) -> Result<(), String> {
        for (n, atom) in self.storage.ref_atoms() {
            self.by_number.insert(n, atom);
        }
        let txn = self
            .store
            .read()
            .map_err(|e| format!("read transaction: {e}"))?;
        let map = Arc::clone(&self.map);
        let atoms: Vec<(u32, RefAtom)> = self.by_number.iter().map(|(n, a)| (*n, *a)).collect();
        for (number, atom) in atoms {
            let info = self.schema.ref_info(atom);
            let Some(obj) = cfg.objects.iter().find(|o| o.id == info.object) else {
                continue;
            };
            match info.kind {
                MdKind::Enum => {
                    for v in obj.enum_values() {
                        let id = ref_id(v.id);
                        self.enum_names.insert((number, id), v.name.clone());
                        self.enum_ids.insert((number, v.name.to_lowercase()), id);
                    }
                }
                MdKind::Catalog
                | MdKind::ChartOfCharacteristicTypes
                | MdKind::ChartOfAccounts
                | MdKind::ChartOfCalculationTypes => {
                    let Some(pre) = obj.predefined() else {
                        continue;
                    };
                    self.predefined_ids.insert(
                        info.table,
                        pre.iter()
                            .map(|it| (ref_id(it.id), it.name.clone()))
                            .collect(),
                    );
                    let by_pid: HashMap<RefId, &str> = pre
                        .iter()
                        .map(|it| (ref_id(it.id), it.name.as_str()))
                        .collect();
                    let Some(tid) = self.storage.table(info.table) else {
                        continue;
                    };
                    let (id_f, pid_f) = fields_of(&map, tid);
                    let (Some(id_f), Some(pid_f)) = (id_f, pid_f) else {
                        continue;
                    };
                    let fields = [field_id(id_f), field_id(pid_f)];
                    let tinfo = map.table(tid);
                    let rows = txn
                        .scan_fields(tid, &fields, None)
                        .map_err(|e| format!("scan: {e}"))?;
                    for row in rows {
                        let (_, row) = row.map_err(|e| format!("scan: {e}"))?;
                        let row = query_ir::convert::row_to_q(tinfo, &row);
                        let (Some(id), Some(pid)) = (
                            row.get(id_f).and_then(id_bytes),
                            row.get(pid_f).and_then(id_bytes),
                        ) else {
                            continue;
                        };
                        if let Some(name) = by_pid.get(&pid) {
                            self.predefined_names
                                .insert((number, id), (*name).to_string());
                            self.predefined_refs.insert(
                                (info.table, (*name).to_string()),
                                QValue::Ref { table: number, id },
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn field_id(i: usize) -> ibschema::storage::FieldId {
    ibschema::storage::FieldId(u16::try_from(i).unwrap_or(u16::MAX))
}

fn fields_of(map: &StorageMap, t: TableId) -> (Option<usize>, Option<usize>) {
    let info = map.table(t);
    (
        info.field_by_std(StdField::Id).map(|f| usize::from(f.0)),
        info.field_by_std(StdField::PredefinedId)
            .map(|f| usize::from(f.0)),
    )
}

fn id_bytes(v: &QValue) -> Option<RefId> {
    let b: RefId = match v {
        QValue::Ref { id, .. } => *id,
        QValue::Binary(b) => b.as_ref().try_into().ok()?,
        QValue::Uuid(u) => *u,
        _ => return None,
    };
    (b != [0; 16]).then_some(b)
}

impl BindHost for TestBase {
    fn predefined_ref(&self, p: &PredefinedName) -> Result<QValue, HostError> {
        self.predefined_refs
            .get(&(p.table, p.name.clone()))
            .cloned()
            .ok_or_else(|| HostError::new(format!("predefined item {} is not in the base", p.name)))
    }

    fn touch_table(&self, _: TableId) -> Result<(), HostError> {
        Ok(())
    }

    fn predefined_names(&self, table: QTableId) -> Result<Vec<([u8; 16], String)>, HostError> {
        Ok(self.predefined_ids.get(&table).cloned().unwrap_or_default())
    }

    fn week_start(&self) -> Result<u32, HostError> {
        let locale = self
            .locale()
            .ok_or_else(|| HostError::new("the base has no PARAMS/locale.inf"))?;
        bsl_prim::locale::first_weekday(&locale)
            .ok_or_else(|| HostError::new(format!("unknown locale of the base: {locale}")))
    }
}

impl TestBase {
    pub fn collation(&self) -> &dyn Collation {
        self.collation.as_ref()
    }

    pub fn locale(&self) -> Option<String> {
        let raw = infobase::raw::RawIb::from_db(Arc::clone(&self.db));
        let file = raw.folder_file("PARAMS", "locale.inf").ok()??;
        let parse = |b: &[u8]| -> Option<String> {
            let b = b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b);
            let node = brace::parse_bytes(b).ok()?;
            node.as_list()?.first()?.as_str().map(str::to_string)
        };
        parse(&file.data).or_else(|| {
            infobase::raw::inflate_raw(&file.data)
                .ok()
                .and_then(|d| parse(&d))
        })
    }
}

impl TestBase {
    pub(crate) fn presentation(
        &self,
        p: &PresentationInput,
        lang: Lang,
    ) -> Result<QStr, HostError> {
        let text = |v: &QValue| -> Result<String, HostError> {
            match v {
                QValue::Null | QValue::Undefined => Ok(String::new()),
                QValue::String(s) => Ok(s.to_string_lossy()),
                other => query_exec::value_text(other)
                    .map(|u| String::from_utf16_lossy(&u))
                    .map_err(|e| HostError::new(format!("presentation: {e}"))),
            }
        };
        let field = |role: StdRole| p.fields.iter().find(|(r, _)| *r == role).map(|(_, v)| v);
        let s = match &p.reference {
            QValue::SysEnum(t, i) => {
                let cat = platform_catalog::catalog();
                let value = cat
                    .types()
                    .get(usize::from(t.0))
                    .and_then(|t| t.values.get(usize::try_from(*i).ok()?))
                    .ok_or_else(|| HostError::new("presentation: unknown system enumeration"))?;
                match lang {
                    Lang::Ru => value.name_ru.clone(),
                    Lang::En => value.name_en.clone(),
                }
            }
            QValue::Ref { table, id } => {
                let atom = p
                    .table
                    .and_then(|t| self.schema.table(t).reference)
                    .or_else(|| self.by_number.get(table).copied())
                    .ok_or_else(|| {
                        HostError::new("presentation: a reference to an unknown table")
                    })?;
                let info = self.schema.ref_info(atom);
                match info.kind {
                    MdKind::Enum => match self.enum_names.get(&(*table, *id)) {
                        Some(name) => name.clone(),
                        None => query_exec::not_found(lang, *table, id),
                    },
                    MdKind::Document | MdKind::BusinessProcess | MdKind::Task => {
                        let number = field(StdRole::Number).map(text).transpose()?;
                        let date = field(StdRole::Date)
                            .map(text)
                            .transpose()?
                            .unwrap_or_default();
                        let from = match lang {
                            Lang::Ru => "от",
                            Lang::En => "dated",
                        };
                        format!("{} {} {from} {date}", info.name, number.unwrap_or_default())
                    }
                    _ => match field(StdRole::Description) {
                        Some(d) => {
                            let d = text(d)?;
                            if d.is_empty() { "<>".to_string() } else { d }
                        }
                        None => field(StdRole::Code)
                            .map(text)
                            .transpose()?
                            .unwrap_or_default(),
                    },
                }
            }
            other => {
                return Err(HostError::new(format!(
                    "presentation: not a reference: {other:?}"
                )));
            }
        };
        Ok(QStr::from_str(&s))
    }
}

pub fn project_root() -> Option<PathBuf> {
    if let Some(p) =
        std::env::var_os("ONEC_PROJECT_ROOT").or_else(|| std::env::var_os("BSL_PROJECT_ROOT"))
    {
        return Some(PathBuf::from(p));
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|a| a.join("tests/conformance/manifest.json").is_file())
        .map(Path::to_path_buf)
}

fn bases_dir() -> Option<PathBuf> {
    std::env::var_os("QUERY_BASES")
        .map(PathBuf::from)
        .or_else(|| Some(project_root()?.join("out/query/bases")))
}

pub fn try_base(name: &str) -> Result<Option<TestBase>, String> {
    match bases_dir() {
        Some(d) => try_base_in(&d, name),
        None => {
            eprintln!("SKIP: QUERY_BASES is not set");
            Ok(None)
        }
    }
}

pub fn try_base_in(bases: &Path, name: &str) -> Result<Option<TestBase>, String> {
    let dir = bases.join(name);
    if !dir.join("1Cv8.1CD").is_file() {
        eprintln!(
            "SKIP: QUERY_BASES: {} not found",
            dir.join("1Cv8.1CD").display()
        );
        return Ok(None);
    }
    let base = TestBase::open(&dir)?;
    if !base.schema_notes.is_empty() {
        eprintln!(
            "NOTE: {name}: lenient open: {}",
            base.schema_notes.join("; ")
        );
    }
    Ok(Some(base))
}

pub fn base(name: &str) -> Option<TestBase> {
    match try_base(name) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("ERROR: {e}");
            None
        }
    }
}

#[derive(Debug)]
struct OpenFailure {
    path: PathBuf,
    source: infobase::Error,
}
impl std::fmt::Display for OpenFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "open {}: {}", self.path.display(), self.source)
    }
}
impl std::error::Error for OpenFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}
