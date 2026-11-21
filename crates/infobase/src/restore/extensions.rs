use super::*;
use ibschema::logical::{KeyKind, Table, TableKind};
use memory_budget::{Category, Charged, MemoryBudget, Reservation};

const SID: u32 = 1;
const HISTORY: [(&str, &str); 4] = [
    ("DataHistoryVersionsExt", "DataHistoryVersions"),
    ("DataHistoryLatestVerExt", "DataHistoryLatestVersions"),
    ("DataHistoryMetadataExt", "DataHistoryMetadata"),
    ("DataHistorySettingsExt", "DataHistorySettings"),
];

fn reserve(budget: &MemoryBudget, bytes: u64) -> Result<Reservation> {
    budget.try_reserve(Category::Metadata, bytes).map_err(|_| {
        bad("extension metadata exceeds the schema memory share; increase --memory-limit")
    })
}

fn add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b)
        .ok_or_else(|| bad("extension metadata size overflow"))
}
fn mul(a: u64, b: u64) -> Result<u64> {
    a.checked_mul(b)
        .ok_or_else(|| bad("extension metadata size overflow"))
}

fn metrics(schema: &LogicalSchema) -> Result<(u64, u64, usize)> {
    fn table(t: &Table, owner: Option<&Table>) -> Result<(u64, u64, usize)> {
        let mut nodes = 16u64;
        let mut strings = add(t.name.capacity() as u64, t.parent.capacity() as u64)?;
        strings = add(strings, t.tail_s.capacity() as u64)?;
        for splitter in &t.splitters {
            nodes = add(nodes, if splitter.is_empty() { 2 } else { 3 })?;
            for group in splitter {
                nodes = add(nodes, add(2, group.len() as u64)?)?;
                for name in group {
                    strings = add(strings, name.capacity() as u64)?;
                }
            }
        }
        for f in &t.fields {
            nodes = add(nodes, 7)?;
            strings = add(
                strings,
                add(f.name.capacity() as u64, f.tail_s.capacity() as u64)?,
            )?;
            for ty in &f.types {
                nodes = add(nodes, 6 + u64::from(ty.extra.is_some()))?;
                strings = add(strings, ty.ref_table.capacity() as u64)?;
            }
        }
        for i in &t.indexes {
            nodes = add(
                nodes,
                add(11, add(i.fields.len() as u64, i.list.len() as u64)?)?,
            )?;
            strings = add(strings, i.name.capacity() as u64)?;
            for name in &i.fields {
                strings = add(strings, name.capacity() as u64)?;
            }
        }
        fn columns(f: &ibschema::logical::Field) -> Result<(u64, u64)> {
            let count = mul(f.types.len().max(1) as u64, 2)?;
            Ok((count, mul(add(f.name.capacity() as u64, 16)?, count)?))
        }
        let prefix = add(
            add(t.name.capacity() as u64, 16)?,
            owner.map_or(0, |o| o.name.capacity() as u64),
        )?;
        let mut generated_columns = 0;
        for f in &t.fields {
            generated_columns = add(generated_columns, columns(f)?.1)?;
        }
        if let Some(o) = owner {
            generated_columns = add(generated_columns, add(o.name.capacity() as u64, 32)?)?;
        }
        let source = owner.unwrap_or(t);
        let [first, second] = &source.splitters;
        let splitters = if owner.is_some() { second } else { first };
        let mut splitter_columns = 0;
        let mut splitter_bytes = 0;
        for name in splitters.iter().flatten() {
            let field = source
                .fields
                .iter()
                .find(|f| f.name == *name)
                .ok_or_else(|| bad("extension metadata splitter field"))?;
            let (count, bytes) = columns(field)?;
            splitter_columns = add(splitter_columns, count)?;
            splitter_bytes = add(splitter_bytes, add(bytes, name.capacity() as u64)?)?;
        }
        let index_field_size = std::mem::size_of::<onecd::IndexField>() as u64;
        let mut occurrences = 0;
        for index in &t.indexes {
            for name in &index.fields {
                if let Some(owner) = owner.filter(|_| name == "ID") {
                    occurrences = add(
                        occurrences,
                        add(owner.name.capacity() as u64, 16 + index_field_size)?,
                    )?;
                } else {
                    let mut found = false;
                    for field in t.fields.iter().filter(|f| f.name == *name) {
                        found = true;
                        let (count, bytes) = columns(field)?;
                        occurrences = add(occurrences, add(bytes, mul(count, index_field_size)?)?)?;
                    }
                    if !found {
                        return Err(bad("extension metadata index field"));
                    }
                }
            }
        }
        let copies = add(t.indexes.len() as u64, 4)?;
        let generated = add(
            mul(
                add(
                    add(prefix, generated_columns)?,
                    add(splitter_bytes, mul(splitter_columns, 1 + index_field_size)?)?,
                )?,
                copies,
            )?,
            occurrences,
        )?;
        strings = add(strings, mul(generated, 2)?)?;
        let mut tables = 1usize;
        for child in &t.nested {
            let (n, s, c) = table(child, Some(t))?;
            nodes = add(nodes, n)?;
            strings = add(strings, s)?;
            tables = tables
                .checked_add(c)
                .ok_or_else(|| bad("schema table limit"))?;
        }
        Ok((nodes, strings, tables))
    }
    let (mut nodes, mut strings, mut tables) = (4u64, 0u64, 0usize);
    for t in &schema.tables {
        let (n, s, c) = table(t, None)?;
        nodes = add(nodes, n)?;
        strings = add(strings, s)?;
        tables = tables
            .checked_add(c)
            .ok_or_else(|| bad("schema table limit"))?;
    }
    let allowance = add(
        add(mul(nodes, 100)?, mul(strings, 8)?)?,
        mul(tables as u64, 4096)?,
    )?;
    Ok((nodes, allowance, tables))
}

fn node_count(node: &brace::Node) -> Result<u64> {
    match node {
        brace::Node::List(children) => children.iter().try_fold(1, |n, c| add(n, node_count(c)?)),
        _ => Ok(1),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndexLayout {
    Full,
    Short,
}

fn full_layout(root: &brace::Node, remaining_tables: usize) -> Result<IndexLayout> {
    #[derive(Default)]
    struct Seen {
        short: bool,
        full: bool,
    }
    fn table(
        node: &brace::Node,
        depth: usize,
        remaining: &mut usize,
        seen: &mut Seen,
    ) -> Result<()> {
        *remaining = remaining
            .checked_sub(1)
            .ok_or_else(|| bad("cumulative schema table limit"))?;
        if depth > 8 {
            return Err(bad("unsupported extension schema nesting"));
        }
        let children = node
            .as_list()
            .filter(|v| v.len() == 14)
            .ok_or_else(|| bad("unsupported extension CURRENTSCHEMA table layout"))?;
        let indexes = children
            .get(6)
            .and_then(brace::Node::as_list)
            .ok_or_else(|| bad("extension index list"))?;
        for i in indexes.iter().skip(1) {
            match i.as_list().map(<[_]>::len) {
                Some(6) => seen.short = true,
                Some(9) => seen.full = true,
                _ => return Err(bad("unsupported extension CURRENTSCHEMA index layout")),
            }
            if seen.short && seen.full {
                return Err(bad(
                    "unsupported extension CURRENTSCHEMA layout: six-element and nine-element indexes are mixed",
                ));
            }
        }
        let nested = children
            .get(5)
            .and_then(brace::Node::as_list)
            .ok_or_else(|| bad("extension nested table list"))?;
        for child in nested.iter().skip(1) {
            table(child, depth + 1, remaining, seen)?;
        }
        Ok(())
    }
    let tables = root
        .at(1)
        .and_then(brace::Node::as_list)
        .ok_or_else(|| bad("extension schema table list"))?;
    let mut remaining = remaining_tables;
    let mut seen = Seen::default();
    for t in tables.iter().skip(1) {
        table(t, 1, &mut remaining, &mut seen)?;
    }
    Ok(if seen.short {
        IndexLayout::Short
    } else {
        IndexLayout::Full
    })
}

fn legacy_allowance(nodes: u64, text_len: usize) -> Result<u64> {
    let bytes = text_len as u64;
    add(add(mul(nodes, 116)?, mul(bytes, 8)?)?, bytes)
}

const LEGACY_TAIL: &[u8] = b",\r\n{0},0,0}";

fn only_tails_added(short: &[u8], full: &[u8], indexes: usize) -> bool {
    let mut rebuilt = Vec::with_capacity(short.len());
    let mut rest = full;
    let mut found = 0;
    while let Some(at) = rest
        .windows(LEGACY_TAIL.len())
        .position(|w| w == LEGACY_TAIL)
    {
        let (head, tail) = rest.split_at(at);
        rebuilt.extend_from_slice(head);
        rebuilt.push(b'}');
        rest = tail.get(LEGACY_TAIL.len()..).unwrap_or_default();
        found += 1;
    }
    rebuilt.extend_from_slice(rest);
    found == indexes && rebuilt == short
}

fn index_count(schema: &LogicalSchema) -> usize {
    fn table(t: &Table) -> usize {
        t.indexes.len() + t.nested.iter().map(table).sum::<usize>()
    }
    schema.tables.iter().map(table).sum()
}

fn state(t: &Table) -> Result<()> {
    if t.splitters.iter().any(|s| !s.is_empty())
        || t.one != 1
        || !t.tail_s.is_empty()
        || t.tail_n1 != 0
        || t.tail_n2 != 0
        || t.fields.iter().any(|f| {
            !f.tail_s.is_empty() || f.tail_n != 0 || f.types.iter().any(|ty| ty.extra.is_some())
        })
        || t.indexes
            .iter()
            .any(|i| i.flag2 || i.n3 != 0 || i.list != [0] || i.n4 != 0 || i.n5 != 0)
    {
        return Err(bad("unsupported extension schema state"));
    }
    for child in &t.nested {
        state(child)?;
    }
    Ok(())
}

fn catalog(t: &Table) -> bool {
    t.kind == TableKind::Main
        && t.key == KeyKind::Ref
        && t.name == format!("Reference{}", t.number)
        && t.parent.is_empty()
}

fn own_kind(t: &Table) -> String {
    let prefix = t.name.trim_end_matches(|c: char| c.is_ascii_digit());
    let kind = match prefix {
        "Reference" => "catalog with an unsupported table shape",
        "Document" => "document",
        "InfoRg" => "information register",
        "AccumRg" => "accumulation register",
        "AccRg" => "accounting register",
        "CalcRg" => "calculation register",
        "Chrc" => "chart of characteristic types",
        "Acc" => "chart of accounts",
        "CKinds" => "chart of calculation types",
        "Enum" => "enum",
        "Node" => "exchange plan",
        "BPr" => "business process",
        "Task" => "task",
        "Const" => "constant",
        _ => "object",
    };
    format!("{kind} {} (table {}X1)", t.name, t.name)
}

fn schema_profile(schema: &LogicalSchema, main: &LogicalSchema) -> Result<()> {
    for t in &schema.tables {
        if let Some((_, base)) = HISTORY.iter().find(|(name, _)| t.name == *name) {
            let mut expected = seed()
                .schema
                .table(base)
                .cloned()
                .ok_or_else(|| bad("history seed"))?;
            expected.name.clone_from(&t.name);
            expected.number = t.number;
            if t.name == "DataHistoryMetadataExt" {
                expected
                    .fields
                    .retain(|f| f.name != "IsExtensions" && f.name != "ActionOnAccept");
                for index in &mut expected.indexes {
                    index.fields.retain(|name| name != "IsExtensions");
                }
            }
            if *t != expected {
                return Err(bad("unsupported extension history schema"));
            }
        } else {
            match main.table(&t.name) {
                Some(base) => adopted(t, base)?,
                None if catalog(t) => {}
                None => {
                    return Err(bad(format!(
                        "unsupported extension object schema; catalog storage is required: the extension's own {} \
                         is not restored yet",
                        own_kind(t)
                    )));
                }
            }
            for child in &t.nested {
                let digits = child.name.strip_prefix("VT").unwrap_or("");
                let canonical = digits
                    .parse::<u32>()
                    .ok()
                    .filter(|n| *n != 0)
                    .map(|n| format!("VT{n}"));
                if child.kind != TableKind::Nested
                    || child.key != KeyKind::Plain
                    || child.number != 0
                    || child.parent != t.name
                    || !child.nested.is_empty()
                    || canonical.as_deref() != Some(child.name.as_str())
                {
                    return Err(bad("unsupported extension tabular section schema"));
                }
            }
        }
        state(t)?;
    }
    Ok(())
}

fn adopted(t: &Table, base: &Table) -> Result<()> {
    if !t.fields.starts_with(&base.fields)
        || !t.indexes.starts_with(&base.indexes)
        || t.kind != base.kind
        || t.key != base.key
        || t.parent != base.parent
    {
        return Err(bad(
            "extension adopted table does not preserve the base schema",
        ));
    }
    for child in &base.nested {
        let extended = t
            .nested
            .iter()
            .find(|t| t.name == child.name)
            .ok_or_else(|| bad("missing extension adopted tabular section"))?;
        adopted(extended, child)?;
    }
    Ok(())
}

fn namespace(main: &LogicalSchema, extension: &[TableDescr]) -> Result<()> {
    let mut tables = protected_names()?;
    let mut indexes = HashSet::new();
    for d in descrs(main, NameCase::Upper)?.iter().chain(extension) {
        if !tables.insert(d.name.to_ascii_uppercase()) {
            return Err(bad("duplicate or protected extension physical table"));
        }
        let mut fields = HashSet::new();
        if d.fields
            .iter()
            .any(|f| !fields.insert(f.name.to_ascii_uppercase()))
        {
            return Err(bad("duplicate extension physical column namespace"));
        }
        let mut local = HashSet::new();
        for i in &d.indexes {
            let key = i.name.to_ascii_uppercase();
            if !local.insert(key.clone()) || (key != "_IDRREFIDX" && !indexes.insert(key)) {
                return Err(bad("duplicate extension physical index namespace"));
            }
        }
    }
    Ok(())
}

pub(super) fn profile(compat: i64, cas: [u64; 2], plans: &[TablePlan]) -> Result<()> {
    if ![80327, 80501].contains(&compat) {
        return Err(bad("unsupported extension compatibility profile"));
    }
    let count = |name: &str| plans.iter().find(|p| p.table == name).map(|p| p.rows);
    if cas[0] == 0
        || cas[1] != 0
        || count("_EXTENSIONSINFO").is_none_or(|n| n == 0)
        || count("_EXTENSIONSINFONGS") != Some(0)
        || count("_EXTENSIONSRESTRUCTNGS") != Some(0)
    {
        return Err(bad(
            "unsupported extension installed/applied state (ConfigCAS, ConfigCASSave or NGS)",
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub(super) struct StoragePin {
    id: i64,
    schema_len: usize,
    schema_sha: [u8; 32],
}

#[derive(Default)]
pub(super) struct Memory {
    budget: Option<MemoryBudget>,
    main: Option<Reservation>,
    raw: Option<Reservation>,
    derived: Option<Reservation>,
    transient: Option<Reservation>,
    plans: Option<Reservation>,
}

impl Memory {
    pub(super) fn new(limits: &RestoreLimits, parent: Option<&MemoryBudget>) -> Result<Self> {
        let share = mul(limits.schema_nodes.min(SCHEMA_NODES_CAP), 100)?.min(limits.row_bytes);
        let root = parent
            .cloned()
            .unwrap_or_else(|| MemoryBudget::new(memory_budget::Limits::new(share)));
        Ok(Self {
            budget: Some(root.try_scope(Category::Metadata, share)?),
            ..Default::default()
        })
    }

    pub(super) fn retain_plans(&mut self, plans: &Vec<TablePlan>) -> Result<()> {
        let Some(budget) = &self.budget else {
            return Err(bad("missing extension metadata scope"));
        };
        let mut bytes = mul(
            plans.capacity() as u64,
            std::mem::size_of::<TablePlan>() as u64,
        )?;
        for plan in plans {
            bytes = add(bytes, plan.table.capacity() as u64)?;
            let mut names = |items: &Vec<String>| -> Result<()> {
                bytes = add(
                    bytes,
                    mul(
                        items.capacity() as u64,
                        std::mem::size_of::<String>() as u64,
                    )?,
                )?;
                for name in items {
                    bytes = add(bytes, name.capacity() as u64)?;
                }
                Ok(())
            };
            names(&plan.kept)?;
            if let Decision::ClusteredOnly { dropped } = &plan.decision {
                names(dropped)?;
            }
        }
        self.plans = Some(reserve(budget, bytes)?);
        self.main.take();
        self.derived.take();
        Ok(())
    }
}

impl Memory {
    pub(super) fn scratch(&self, bytes: u64) -> Result<Reservation> {
        self.budget
            .as_ref()
            .ok_or_else(|| bad("missing restore metadata scope"))?
            .try_reserve(Category::Metadata, bytes)
            .map_err(|error| Error::Memory(error.into()))
    }

    pub(super) fn reduced_reports(&self, plans: &[TablePlan]) -> Result<Reservation> {
        let mut bytes = 0;
        let mut count = 0;
        for plan in plans {
            if let Decision::ClusteredOnly { dropped } = &plan.decision {
                count = add(count, 1)?;
                bytes = add(bytes, mul(plan.table.capacity() as u64, 2)?)?;
                for names in [&plan.kept, dropped] {
                    bytes = add(
                        bytes,
                        mul(
                            names.capacity() as u64,
                            std::mem::size_of::<String>() as u64,
                        )?,
                    )?;
                    for name in names {
                        bytes = add(bytes, name.capacity() as u64)?;
                    }
                }
            }
        }
        self.scratch(add(
            bytes,
            mul(mul(count, 8)?, std::mem::size_of::<ReducedIndexes>() as u64)?,
        )?)
    }

    pub(super) fn object_names<'a>(
        &self,
        names: impl Iterator<Item = (Option<&'a str>, Option<&'a str>)>,
    ) -> Result<Reservation> {
        let mut bytes = 0;
        for (object, service) in names {
            bytes = add(bytes, std::mem::size_of::<TableObject>() as u64)?;
            for name in [object, service].into_iter().flatten() {
                bytes = add(bytes, mul(name.len() as u64, 2)?)?;
            }
        }
        self.scratch(bytes)
    }
}

pub(super) struct Block {
    pub schema: Charged<LogicalSchema>,
    pub heads: Vec<TableHead>,
    pub descriptors: Vec<TableDescr>,
    rows: Vec<Vec<ColumnValue>>,
    pins: Vec<StoragePin>,
    budget: MemoryBudget,
}

impl Block {
    pub fn read<R: Read>(
        reader: &mut DtReader<R>,
        main: &LogicalSchema,
        limits: &RestoreLimits,
        row_limits: ReadLimits,
        memory: &mut Memory,
        parent_budget: Option<&MemoryBudget>,
    ) -> Result<Self> {
        let (main_nodes, main_live, main_tables) = metrics(main)?;
        let share = mul(limits.schema_nodes.min(SCHEMA_NODES_CAP), 100)?.min(limits.row_bytes);
        if memory.budget.is_none() {
            *memory = Memory::new(limits, parent_budget)?;
        }
        let budget = memory
            .budget
            .as_ref()
            .ok_or_else(|| bad("missing extension metadata scope"))?
            .clone();
        memory.main = Some(reserve(&budget, main_live)?);
        let current = budget
            .scope_snapshot()
            .ok_or_else(|| bad("extension metadata scope"))?
            .current;
        let row_cap = (share.saturating_sub(current) / 16).min(limits.row_bytes / 16);
        if row_cap == 0 {
            return Err(bad("extension metadata memory share"));
        }
        memory.raw = Some(reserve(&budget, mul(row_cap, 8)?)?);
        let main_text = main.to_text();
        if main_text.len() > SCHEMA_TEXT_MAX {
            return Err(bad("schema byte limit"));
        }
        let head = TableHead::system(&Source::SchemaStorage).ok_or_else(|| bad("storage head"))?;
        expect(reader.next_event()?, Event::TableStart(head.clone()))?;
        reader.set_read_limits(ReadLimits {
            atom_bytes: row_cap.min(limits.atom_bytes),
            retained_bytes: row_cap,
            nodes: row_limits.nodes.min(row_cap / 128),
        });
        let mut rows = Vec::new();
        loop {
            match reader.next_event()? {
                Some(Event::Row(row)) => {
                    if rows.len() == 2 {
                        return Err(bad(
                            "unsupported extension storage cardinality; only main0 and schema1 are supported",
                        ));
                    }
                    check_row(&head, &row)?;
                    let id = if matches!(row.first(), Some(ColumnValue::Number(n)) if n.eq_value(&Numeric::from_i64(0)))
                    {
                        0
                    } else if matches!(row.first(), Some(ColumnValue::Number(n)) if n.eq_value(&Numeric::from_i64(1)))
                    {
                        1
                    } else {
                        return Err(bad(
                            "unsupported extension SCHEMAID; only schema1 is supported",
                        ));
                    };
                    if !matches!(row.get(1), Some(ColumnValue::Number(n)) if n.eq_value(&Numeric::from_i64(100)))
                        || !matches!(row.get(3), Some(ColumnValue::Bytes(b)) if b == &seed().newgen)
                        || !matches!(row.get(4), Some(ColumnValue::Bytes(b)) if b == &seed().newgen)
                    {
                        return Err(bad("unsupported extension STATUS or NEWGEN state"));
                    }
                    let Some(ColumnValue::Bytes(text)) = row.get(2) else {
                        return Err(bad("extension CURRENTSCHEMA"));
                    };
                    if id == 0 && *text != main_text {
                        return Err(bad(
                            "extension main CURRENTSCHEMA differs from DataBase schema",
                        ));
                    }
                    rows.push(row);
                }
                Some(Event::TableEnd) => break,
                _ => return Err(bad("extension storage table end")),
            }
        }
        reader.set_read_limits(row_limits);
        if rows.len() != 2 {
            return Err(bad("extension storage requires main0 and schema1"));
        }
        let mut pins = Vec::new();
        for row in &rows {
            let id = if matches!(row.first(), Some(ColumnValue::Number(n)) if n.eq_value(&Numeric::from_i64(0)))
            {
                0
            } else {
                1
            };
            let Some(ColumnValue::Bytes(text)) = row.get(2) else {
                return Err(bad("extension CURRENTSCHEMA"));
            };
            pins.push(StoragePin {
                id,
                schema_len: text.len(),
                schema_sha: Sha256::digest(text).into(),
            });
        }
        pins.sort_by_key(|p| p.id);
        if pins.iter().map(|p| p.id).ne([0, 1]) {
            return Err(bad("extension storage schema identifiers"));
        }
        let text = rows.iter().find(|row| matches!(row.first(), Some(ColumnValue::Number(n)) if n.eq_value(&Numeric::from_i64(1))))
            .and_then(|row| row.get(2)).and_then(|v| match v { ColumnValue::Bytes(b) => Some(b.as_slice()), _ => None })
            .ok_or_else(|| bad("missing extension schema1"))?;
        let main_len = main_text.len() as u64;
        if add(main_len, text.len() as u64)? > SCHEMA_TEXT_MAX as u64 {
            return Err(bad("cumulative schema byte limit"));
        }
        drop(main_text);
        let baseline = budget
            .scope_snapshot()
            .ok_or_else(|| bad("extension metadata scope"))?
            .current;
        let root = brace::parse_bytes_budgeted(text, &budget, Category::Metadata)
            .map_err(|e| bad(format!("extension CURRENTSCHEMA: {e}")))?;
        let nodes = node_count(&root)?;
        if add(main_nodes, nodes)? > limits.schema_nodes {
            return Err(bad("cumulative schema node limit"));
        }
        let layout = full_layout(&root, TABLES_MAX.saturating_sub(main_tables))?;
        drop(root);
        let mut lease = None;
        let canonical = match layout {
            IndexLayout::Full => None,
            IndexLayout::Short => {
                lease = Some(reserve(&budget, legacy_allowance(nodes, text.len())?)?);
                let legacy = LogicalSchema::from_text_legacy_indexes(text)
                    .map_err(|e| bad(format!("unsupported extension CURRENTSCHEMA: {e}")))?;
                let full = legacy.to_text();
                if !only_tails_added(text, &full, index_count(&legacy)) {
                    return Err(bad(
                        "unsupported extension CURRENTSCHEMA layout: short indexes in a noncanonical text",
                    ));
                }
                drop(legacy);
                if add(main_len, full.len() as u64)? > SCHEMA_TEXT_MAX as u64 {
                    return Err(bad("cumulative schema byte limit"));
                }
                let root = brace::parse_bytes_budgeted(&full, &budget, Category::Metadata)
                    .map_err(|e| bad(format!("extension CURRENTSCHEMA: {e}")))?;
                if add(main_nodes, node_count(&root)?)? > limits.schema_nodes {
                    return Err(bad("cumulative schema node limit"));
                }
                if full_layout(&root, TABLES_MAX.saturating_sub(main_tables))? != IndexLayout::Full
                {
                    return Err(bad("extension CURRENTSCHEMA legacy index tails"));
                }
                Some(full)
            }
        };
        let text = canonical.as_deref().unwrap_or(text);
        let schema = LogicalSchema::from_text_budgeted(text, &budget)
            .map_err(|e| bad(format!("unsupported extension CURRENTSCHEMA: {e}")))?;
        let (_, extension_live, extension_tables) = metrics(&schema)?;
        if main_tables
            .checked_add(extension_tables)
            .is_none_or(|n| n > TABLES_MAX)
        {
            return Err(bad("cumulative schema table limit"));
        }
        memory.derived = Some(reserve(&budget, mul(extension_live, 4)?)?);
        schema_profile(&schema, main)?;
        if let Some(full) = canonical {
            for pin in &mut pins {
                if pin.id == 1 {
                    pin.schema_len = full.len();
                    pin.schema_sha = Sha256::digest(&full).into();
                }
            }
            let cell = rows
                .iter_mut()
                .find(|row| matches!(row.first(), Some(ColumnValue::Number(n)) if n.eq_value(&Numeric::from_i64(1))))
                .and_then(|row| row.get_mut(2))
                .ok_or_else(|| bad("extension CURRENTSCHEMA"))?;
            *cell = ColumnValue::Bytes(full);
        }
        drop(lease);
        let heads = TableHead::extension(SID, &schema)?;
        let mut descriptors = descrs(&schema, NameCase::Upper)?;
        for d in &mut descriptors {
            d.name.push_str("X1");
            for index in &mut d.indexes {
                if index.name != "_IDRREFIDX" {
                    index.name.push_str("X1");
                }
            }
        }
        namespace(main, &descriptors)?;
        memory.transient = Some(reserve(
            &budget,
            mul(
                budget
                    .scope_snapshot()
                    .ok_or_else(|| bad("extension metadata scope"))?
                    .peak
                    .saturating_sub(baseline),
                2,
            )?,
        )?);
        match reader.next_event()? {
            Some(Event::ExtensionStart {
                schema_id: SID,
                schema: actual,
            }) if actual == *schema => {}
            _ => return Err(bad("unsupported or mismatched extension schema block")),
        }
        Ok(Self {
            schema,
            heads,
            descriptors,
            rows,
            pins,
            budget,
        })
    }

    pub fn load<R: Read>(
        &mut self,
        reader: &mut DtReader<R>,
        run: &mut Run<'_, '_>,
        notes: &mut Notes<'_, '_>,
        batches: &mut Batches<'_>,
        db: &Database,
        plans: &[TablePlan],
    ) -> Result<Vec<u64>> {
        if plans.len() != self.heads.len()
            || plans
                .iter()
                .zip(&self.heads)
                .any(|(p, h)| !p.table.eq_ignore_ascii_case(&h.phys_name))
        {
            return Err(bad("extension tables differ from the pre-pass"));
        }
        self.descriptors = reduced(std::mem::take(&mut self.descriptors), plans);
        for d in &self.descriptors {
            batches
                .unit(4 * PAGE, "extension table description")?
                .create_table(d)?;
        }
        batches.commit()?;
        let scope = import_scope(&self.heads, &db.info().tables, &Source::Extension(SID))?;
        let descriptor = batches
            .unit(0, "storage")?
            .read_table("SCHEMASTORAGE")?
            .descr()
            .clone();
        let storage = Plan::new(
            &descriptor,
            &TableHead::system(&Source::SchemaStorage).ok_or_else(|| bad("storage head"))?,
        )?;
        let ids = batches
            .unit(0, "storage")?
            .read_table("SCHEMASTORAGE")?
            .scan()
            .map(|r| r.map(|(id, _)| id))
            .collect::<onecd::Result<Vec<_>>>()?;
        for id in ids {
            batches
                .unit(0, "storage")?
                .table("SCHEMASTORAGE")?
                .delete(id)?;
        }
        for row in self.rows.drain(..) {
            let tx = batches.unit(storage.estimate(&row), "extension storage row")?;
            tx.table("SCHEMASTORAGE")?.insert(&storage.row(row)?)?;
        }
        let mut counts = Vec::new();
        for (head, table_plan) in self.heads.iter().zip(plans) {
            expect(reader.next_event()?, Event::TableStart(head.clone()))?;
            run.table.clone_from(&head.phys_name);
            run.progress(notes, Phase::Read);
            batches.commit()?;
            let (descriptor, mut meter) = {
                let table = batches
                    .unit(0, "extension table")?
                    .read_table(&head.phys_name)?;
                (
                    table.descr().clone(),
                    CommitMeter::new(table.layout().record_len),
                )
            };
            let plan = Plan::new(&descriptor, head)?;
            if !scope.contains(&plan.table) {
                return Err(bad("extension row outside import scope"));
            }
            let mut count = 0;
            loop {
                match reader.next_event()? {
                    Some(Event::TableEnd) => break,
                    Some(Event::Row(row)) => {
                        if HISTORY.iter().any(|(name, _)| head.name == *name) {
                            return Err(bad("unsupported nonempty extension history state"));
                        }
                        check_row(head, &row)?;
                        let tx = batches.unit(plan.estimate(&row), "extension data row")?;
                        let row = plan.row(row)?;
                        tx.table_for_restore(&plan.table)?
                            .insert(&row)
                            .map_err(|e| cap_hit(e.into(), Some(table_plan)))?;
                        if meter.row(blob_lens(&row)) {
                            batches.commit()?;
                        }
                        count += 1;
                        run.rows += 1;
                        run.tick(notes);
                    }
                    _ => return Err(bad("missing extension table end")),
                }
            }
            batches.commit()?;
            if count != table_plan.rows {
                return Err(bad("extension row count differs from the pre-pass"));
            }
            counts.push(count);
        }
        Ok(counts)
    }

    pub fn finish<R: Read>(
        self,
        reader: &mut DtReader<R>,
        memory: &mut Memory,
    ) -> Result<Verified> {
        expect(reader.next_event()?, Event::ExtensionEnd)?;
        expect(reader.next_event()?, Event::ExtensionsEnd)?;
        expect(reader.next_event()?, Event::DataBaseEnd)?;
        if reader.next_event()?.is_some() {
            return Err(bad("element after DataBase"));
        }
        let Self {
            schema,
            heads,
            descriptors,
            rows,
            pins,
            budget,
        } = self;
        drop(rows);
        drop(schema);
        memory.raw.take();
        memory.transient.take();
        Ok(Verified {
            heads,
            descriptors,
            pins,
            budget,
        })
    }
}

pub(super) fn check_data_head(head: &TableHead) -> Result<()> {
    if head.source == Source::Extension(SID) && HISTORY.iter().any(|(n, _)| head.name == *n) {
        return Err(bad("unsupported nonempty extension history state"));
    }
    Ok(())
}

pub(super) struct Verified {
    pub heads: Vec<TableHead>,
    pub descriptors: Vec<TableDescr>,
    pins: Vec<StoragePin>,
    budget: MemoryBudget,
}
impl Verified {
    pub fn verify_storage(&self, table: &onecd::TableRead<'_>) -> Result<()> {
        let mut seen = HashSet::new();
        for entry in table.scan() {
            let (_, row) = entry?;
            let value = |name: &str| {
                table
                    .descr()
                    .field(name)
                    .and_then(|(i, _)| row.get(i))
                    .ok_or_else(|| bad("final storage field"))
            };
            let pin = self.pins.iter().find(|p| matches!(value("SCHEMAID"), Ok(Value::Numeric(n)) if n.eq_value(&Numeric::from_i64(p.id))))
                .ok_or_else(|| bad("final extension storage identifier"))?;
            if !seen.insert(pin.id)
                || !matches!(value("STATUS")?, Value::Numeric(n) if n.eq_value(&Numeric::from_i64(100)))
            {
                return Err(bad("final extension storage state"));
            }
            for (name, size, hash) in [
                ("CURRENTSCHEMA", pin.schema_len, pin.schema_sha),
                (
                    "NEWGENCREATED",
                    seed().newgen.len(),
                    Sha256::digest(&seed().newgen).into(),
                ),
                (
                    "NEWGENDROPPED",
                    seed().newgen.len(),
                    Sha256::digest(&seed().newgen).into(),
                ),
            ] {
                let _lease = reserve(&self.budget, mul(size as u64, 2)?)?;
                let bytes = match value(name)? {
                    Value::Image(BlobVal::Ref(r)) if r.len as usize == size => {
                        table.blob_bytes(*r)?
                    }
                    Value::Image(BlobVal::Data(b)) if b.len() == size => b.clone(),
                    _ => return Err(bad("final extension storage blob length")),
                };
                if <[u8; 32]>::from(Sha256::digest(&bytes)) != hash {
                    return Err(bad("final extension storage bytes"));
                }
            }
        }
        if seen.len() != self.pins.len() {
            return Err(bad("final extension storage cardinality"));
        }
        Ok(())
    }
}
