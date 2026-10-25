use crate::Application;
use crate::errors::{self, keys};
use crate::model::{FieldDef, ObjDef, ObjId, ObjKind, ObjStorage, TypeAtom, Usage};
use crate::read::store_error;
use crate::values::refs::RefValue;
use bsl_prim::fold_name;
use bsl_vm::HostError;
use ibschema::fields::{FieldValue, encode, is_version};
use ibschema::logical::TypeCode;
use ibschema::names::StdField;
use ibschema::storage::{FieldId, IndexId, StorageMap, TableId, TableInfo};
use ibschema::store::{Direction, ReadTxn, RowHandle, StoreError};
use ibschema::values::{RefId, Uuid16};
use ibschema::{DateTime, Numeric};
use md_model::parts::{AccountType, PredefinedCode, PredefinedItem};
use md_model::{DateFractions, MdObject, TypeDescription};
use platform_catalog::messages::Lang;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::ops::Bound;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct AreaKey(pub Vec<FieldValue>);

impl Hash for AreaKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.len().hash(state);
        for v in &self.0 {
            format!("{v:?}").hash(state);
        }
    }
}

type Key = (ObjId, AreaKey);

type Pids = HashMap<[u8; 16], Vec<RefId>>;

type ItemNames = HashMap<[u8; 16], String>;

struct PidCache {
    epoch: u64,
    pids: Arc<Pids>,
}

const NOT_AUTOMATIC: i64 = 2;

#[derive(Default)]
pub(crate) struct Predefined {
    inited: Mutex<HashSet<Key>>,
    serial: Mutex<()>,
    pids: Mutex<HashMap<Key, PidCache>>,
    names: Mutex<HashMap<ObjId, Arc<ItemNames>>>,
    tables: OnceLock<HashMap<TableId, ObjId>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn l5(key: &'static str, args: &[&str]) -> HostError {
    HostError::text(
        &errors::render(key, args, Lang::Ru),
        &errors::render(key, args, Lang::En),
    )
}

fn other(key: &'static str, args: &[&str]) -> HostError {
    let mut e = l5(key, args);
    e.categories = Some(bsl_vm::CategorySet::of(&[bsl_vm::Category::OtherError]));
    e
}

fn internal(text: &str) -> HostError {
    let t = format!("internal: {text}");
    HostError::text(&t, &t)
}

fn write_error(e: &StoreError) -> HostError {
    match e {
        StoreError::WriterBusy => l5(keys::DB_LOCKING, &[]),
        e => store_error(e),
    }
}

pub(crate) fn engine_error(e: &HostError) -> query_ir::HostError {
    let text = match &e.msg {
        bsl_vm::MsgRef::Text { ru, .. } => ru.clone(),
        bsl_vm::MsgRef::Key(k) => errors::render(k.key, &[], Lang::Ru),
    };
    query_ir::HostError::new(text)
}

fn same_name(a: &str, b: &str) -> bool {
    fold_name(a) == fold_name(b)
}

fn full_name(def: &ObjDef, item: &str) -> String {
    format!("{}.{}.{}", def.kind.name(Lang::Ru), def.name, item)
}

impl Predefined {
    pub(crate) fn names(&self, app: &Application, obj: ObjId) -> Vec<([u8; 16], String)> {
        items_of(app, obj)
            .map(|p| {
                p.iter()
                    .map(|it| (RefId::from_uuid(it.id).0, it.name.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn name_of(&self, app: &Application, obj: ObjId, pid: [u8; 16]) -> Option<String> {
        let map = {
            let mut cache = lock(&self.names);
            cache
                .entry(obj)
                .or_insert_with(|| Arc::new(self.names(app, obj).into_iter().collect()))
                .clone()
        };
        map.get(&pid).cloned()
    }

    pub(crate) fn object_of_table(&self, app: &Application, t: TableId) -> Option<ObjId> {
        let map = self.tables.get_or_init(|| {
            let mut m = HashMap::new();
            if let Some(s) = app.model().storage.as_ref() {
                for (i, st) in s.objects.iter().enumerate() {
                    let (Some(st), Ok(id)) = (st, u32::try_from(i)) else {
                        continue;
                    };
                    m.insert(st.main, ObjId(id));
                    for (t, _) in &st.tabular {
                        m.insert(*t, ObjId(id));
                    }
                }
            }
            m
        });
        map.get(&t).copied()
    }

    pub(crate) fn touch(&self, app: &Application, t: TableId) -> Result<(), HostError> {
        match self.object_of_table(app, t) {
            Some(obj) => self.ensure(app, obj, &AreaKey::default()),
            None => Ok(()),
        }
    }

    pub(crate) fn ensure(
        &self,
        app: &Application,
        obj: ObjId,
        area: &AreaKey,
    ) -> Result<(), HostError> {
        if app.model().obj(obj).behavior.predefined_count == 0 {
            return Ok(());
        }
        let key = (obj, area.clone());
        if lock(&self.inited).contains(&key) {
            return Ok(());
        }
        let _one = lock(&self.serial);
        if lock(&self.inited).contains(&key) {
            return Ok(());
        }
        let mut plan = Vec::new();
        for dep in dependencies(app, obj) {
            if !lock(&self.inited).contains(&(dep, area.clone())) {
                plan.push(dep);
            }
        }
        plan.push(obj);
        self.initialize(app, &plan, area)?;
        let mut done = lock(&self.inited);
        for o in plan {
            done.insert((o, area.clone()));
        }
        Ok(())
    }

    pub(crate) fn by_name(
        &self,
        app: &Application,
        obj: ObjId,
        area: &AreaKey,
        name: &str,
    ) -> Result<RefValue, HostError> {
        self.by_name_with(app, obj, area, name, l5)
    }

    pub(crate) fn by_name_with(
        &self,
        app: &Application,
        obj: ObjId,
        area: &AreaKey,
        name: &str,
        mk: fn(&'static str, &[&str]) -> HostError,
    ) -> Result<RefValue, HostError> {
        let def = app.model().obj(obj);
        let item = items_of(app, obj)
            .and_then(|p| p.iter().find(|i| same_name(&i.name, name)))
            .ok_or_else(|| mk(keys::PREDEFINED_ITEM_NOT_EXISTS, &[]))?;
        self.ensure(app, obj, area)?;
        let pids = self.pids(app, obj, area)?;
        match pids
            .get(&RefId::from_uuid(item.id).0)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            [one] => Ok(RefValue { obj, id: *one }),
            [] => Err(mk(
                keys::PREDEFINED_NOT_IN_DATA,
                &[&full_name(def, &item.name)],
            )),
            _ => Err(mk(keys::PREDEFINED_ITEM_NOT_UNIQUE, &[])),
        }
    }

    fn pids(&self, app: &Application, obj: ObjId, area: &AreaKey) -> Result<Arc<Pids>, HostError> {
        let key = (obj, area.clone());
        let epoch = app.write_epoch();
        if let Some(c) = lock(&self.pids).get(&key)
            && c.epoch == epoch
        {
            return Ok(c.pids.clone());
        }
        let pids = Arc::new(scan_pids(app, obj)?);
        lock(&self.pids).insert(
            key,
            PidCache {
                epoch,
                pids: pids.clone(),
            },
        );
        Ok(pids)
    }

    fn initialize(
        &self,
        app: &Application,
        plan: &[ObjId],
        area: &AreaKey,
    ) -> Result<(), HostError> {
        if !area.0.is_empty() {
            return Err(HostError::not_implemented(
                "Предопределенные",
                "Разделение данных",
            ));
        }
        let rd = app.store().read().map_err(|e| store_error(&e))?;
        let mut jobs = Vec::with_capacity(plan.len());
        for &obj in plan {
            let t = Tables::of(app, obj)?;
            let flag = t.flag(&*rd)?;
            let mode = update_mode(app, &*rd, obj)?;
            jobs.push((t, flag, mode));
        }
        drop(rd);
        if jobs.iter().all(|(_, f, _)| f.set) {
            return Ok(());
        }
        let mut w = app
            .store()
            .write(app.options().lock_timeout)
            .map_err(|e| write_error(&e))?;
        for (t, flag, _) in &mut jobs {
            *flag = t.flag(&*w)?;
        }
        if jobs.iter().all(|(_, f, _)| f.set) {
            return Ok(());
        }
        let mut made: HashMap<ObjId, Pids> = HashMap::new();
        for (t, flag, mode) in &jobs {
            if flag.set {
                continue;
            }
            if *mode != NOT_AUTOMATIC {
                let rows = Rows::new(self, app, t, &made)?;
                let created = rows.insert_all(&mut *w).map_err(|e| store_error(&e))?;
                made.insert(t.obj, created);
            }
            t.write_flag(&mut *w, flag).map_err(|e| store_error(&e))?;
        }
        w.commit().map_err(|e| store_error(&e))?;
        app.bump_write_epoch();
        Ok(())
    }
}

fn dependencies(app: &Application, obj: ObjId) -> Vec<ObjId> {
    let def = app.model().obj(obj);
    if def.kind != ObjKind::ChartOfAccounts {
        return Vec::new();
    }
    ext_dimension_chart(app, obj)
        .filter(|c| app.model().obj(*c).behavior.predefined_count > 0)
        .into_iter()
        .collect()
}

fn ext_dimension_chart(app: &Application, obj: ObjId) -> Option<ObjId> {
    let md = app.model().config.object(app.model().obj(obj).md)?;
    let md_model::Value::Ref(r) = md.prop("ExtDimensionTypes")? else {
        return None;
    };
    app.model()
        .of_kind(ObjKind::ChartOfCharacteristicTypes)
        .find(|o| r.id == Some(o.md) || r.name.ends_with(&format!(".{}", o.name)))
        .map(|o| o.id)
}

fn md_of(app: &Application, obj: ObjId) -> Option<&MdObject> {
    app.model().config.object(app.model().obj(obj).md)
}

fn items_of(app: &Application, obj: ObjId) -> Option<&md_model::parts::Predefined> {
    md_of(app, obj)?.predefined()
}

struct Tables<'a> {
    app: &'a Application,
    obj: ObjId,
    def: &'a ObjDef,
    md: &'a MdObject,
    st: &'a ObjStorage,
    map: &'a StorageMap,
    sinf: Option<TableId>,
}

struct Flag {
    set: bool,
    row: Option<(RowHandle, Vec<FieldValue>)>,
}

impl<'a> Tables<'a> {
    fn of(app: &'a Application, obj: ObjId) -> Result<Tables<'a>, HostError> {
        let model = app.model();
        let (Some(st), Some(md)) = (model.storage_of(obj), md_of(app, obj)) else {
            return Err(internal("the object with predefined items has no table"));
        };
        let map = app.store().storage();
        let sinf = st.service.iter().map(|(_, t)| *t).find(|t| {
            map.try_table(*t)
                .is_some_and(|ti| ti.field_by_std(StdField::PdInitialized).is_some())
        });
        Ok(Tables {
            app,
            obj,
            def: model.obj(obj),
            md,
            st,
            map,
            sinf,
        })
    }

    fn flag_table(&self) -> Result<(TableId, &'a TableInfo, FieldId), HostError> {
        let t = self
            .sinf
            .ok_or_else(|| internal("no table of the predefined data flag"))?;
        let ti = self
            .map
            .try_table(t)
            .ok_or_else(|| internal("no table of the predefined data flag"))?;
        let f = ti
            .field_by_std(StdField::PdInitialized)
            .ok_or_else(|| internal("no field PDInitialized"))?;
        if !ti.separators.is_empty() {
            return Err(HostError::not_implemented(
                "Предопределенные",
                "Разделение данных",
            ));
        }
        Ok((t, ti, f))
    }

    fn flag(&self, rd: &dyn ReadTxn) -> Result<Flag, HostError> {
        let (t, _, f) = self.flag_table()?;
        let mut out = Flag {
            set: false,
            row: None,
        };
        for item in rd.scan(t).map_err(|e| store_error(&e))? {
            let (h, row) = item.map_err(|e| store_error(&e))?;
            if matches!(row.get(usize::from(f.0)), Some(FieldValue::Bool(true))) {
                out.set = true;
            } else if out.row.is_none() {
                out.row = Some((h, row));
            }
        }
        Ok(out)
    }

    fn write_flag(
        &self,
        w: &mut dyn ibschema::store::WriteTxn,
        flag: &Flag,
    ) -> Result<(), StoreError> {
        let Ok((t, ti, f)) = self.flag_table() else {
            return Err(StoreError::Unsupported(
                "no table of the predefined data flag".into(),
            ));
        };
        match &flag.row {
            Some((h, row)) => {
                let mut row = row.clone();
                if let Some(c) = row.get_mut(usize::from(f.0)) {
                    *c = FieldValue::Bool(true);
                }
                w.update(t, *h, &row)
            }
            None => {
                let mut row = blank_row(self.app, ti, &[]);
                if let Some(c) = row.get_mut(usize::from(f.0)) {
                    *c = FieldValue::Bool(true);
                }
                w.insert(t, &row).map(drop)
            }
        }
    }
}

fn update_mode(app: &Application, rd: &dyn ReadTxn, obj: ObjId) -> Result<i64, HostError> {
    let Some((t, mdid, mode)) = options_table(app, obj) else {
        return Ok(0);
    };
    let md = app.model().obj(obj).md;
    for item in rd.scan(t).map_err(|e| store_error(&e))? {
        let (_, row) = item.map_err(|e| store_error(&e))?;
        if matches!(row.get(usize::from(mdid.0)), Some(FieldValue::Uuid(u)) if *u == md)
            && let Some(FieldValue::Number(n)) = row.get(usize::from(mode.0))
        {
            return Ok([2, 1, 0]
                .into_iter()
                .find(|k| n.eq_value(&Numeric::from_i64(*k)))
                .unwrap_or(0));
        }
    }
    Ok(0)
}

fn options_table(app: &Application, obj: ObjId) -> Option<(TableId, FieldId, FieldId)> {
    let name = match app.model().obj(obj).kind {
        ObjKind::Catalog => "RefOpt",
        ObjKind::ChartOfCharacteristicTypes => "ChrcOpt",
        ObjKind::ChartOfAccounts => "AccOpt",
        ObjKind::ChartOfCalculationTypes => "CKindsOpt",
        _ => return None,
    };
    let map = app.store().storage();
    let t = map.table_by_name(name)?;
    let ti = map.try_table(t)?;
    Some((t, ti.field_by_name("MDID")?, ti.field_by_name("PDUpdMode")?))
}

fn scan_pids(app: &Application, obj: ObjId) -> Result<Pids, HostError> {
    let Some(st) = app.model().storage_of(obj) else {
        return Ok(Pids::new());
    };
    let ti = app
        .store()
        .storage()
        .try_table(st.main)
        .ok_or_else(|| internal("no table"))?;
    let (Some(pf), Some(idf)) = (
        ti.field_by_std(StdField::PredefinedId),
        ti.field_by_std(StdField::Id),
    ) else {
        return Ok(Pids::new());
    };
    let rd = app.store().read().map_err(|e| store_error(&e))?;
    let mut out = Pids::new();
    let mut take = |row: &[FieldValue]| {
        if let (Some(FieldValue::Uuid(p)), Some(FieldValue::Ref { id, .. })) =
            (row.get(usize::from(pf.0)), row.get(usize::from(idf.0)))
            && !p.as_bytes().iter().all(|b| *b == 0)
        {
            out.entry(RefId::from_uuid(*p).0).or_default().push(*id);
        }
    };
    let by_index = ti
        .indexes
        .iter()
        .position(|ix| ix.prefix == 0 && ix.fields.first() == Some(&pf))
        .and_then(|k| u16::try_from(k).ok());
    let nil = [FieldValue::Uuid(Uuid16::NIL)];
    let ranged = by_index.and_then(|k| {
        rd.range(
            st.main,
            IndexId::Named(k),
            Bound::Excluded(&nil[..]),
            Bound::Unbounded,
            Direction::Forward,
        )
        .ok()
    });
    match ranged {
        Some(it) => {
            for item in it {
                take(&item.map_err(|e| store_error(&e))?.1);
            }
        }
        None => {
            for item in rd.scan(st.main).map_err(|e| store_error(&e))? {
                take(&item.map_err(|e| store_error(&e))?.1);
            }
        }
    }
    Ok(out)
}

struct Flat<'a> {
    item: &'a PredefinedItem,
    parent: Option<usize>,
    id: RefId,
}

struct Rows<'a, 'b> {
    t: &'b Tables<'a>,
    flat: Vec<Flat<'a>>,
    dims: Pids,
    dim_number: Option<u32>,
}

impl<'a, 'b> Rows<'a, 'b> {
    fn new(
        pd: &Predefined,
        app: &Application,
        t: &'b Tables<'a>,
        made: &HashMap<ObjId, Pids>,
    ) -> Result<Rows<'a, 'b>, HostError> {
        let mut flat: Vec<Flat<'a>> = Vec::new();
        if let Some(p) = t.md.predefined() {
            let mut stack: Vec<(&PredefinedItem, Option<usize>)> =
                p.items.iter().rev().map(|i| (i, None)).collect();
            while let Some((item, parent)) = stack.pop() {
                let idx = flat.len();
                flat.push(Flat {
                    item,
                    parent,
                    id: app.uuid1().next_ref(),
                });
                stack.extend(item.children.iter().rev().map(|c| (c, Some(idx))));
            }
        }
        let (mut dims, mut dim_number) = (Pids::new(), None);
        if t.def.kind == ObjKind::ChartOfAccounts
            && let Some(chart) = ext_dimension_chart(app, t.obj)
        {
            dim_number = app.model().storage_of(chart).and_then(|s| s.ref_number);
            dims = match made.get(&chart) {
                Some(m) => m.clone(),
                None => (*pd.pids(app, chart, &AreaKey::default())?).clone(),
            };
        }
        Ok(Rows {
            t,
            flat,
            dims,
            dim_number,
        })
    }

    fn insert_all(&self, w: &mut dyn ibschema::store::WriteTxn) -> Result<Pids, StoreError> {
        let mut created = Pids::new();
        for (i, f) in self.flat.iter().enumerate() {
            let row = self.main_row(i, f);
            w.insert(self.t.st.main, &row)?;
            created
                .entry(RefId::from_uuid(f.item.id).0)
                .or_default()
                .push(f.id);
        }
        for (i, f) in self.flat.iter().enumerate() {
            for (table, row) in self.sub_rows(i, f) {
                w.insert(table, &row)?;
            }
        }
        Ok(created)
    }

    fn main_table(&self) -> Option<&'a TableInfo> {
        self.t.map.try_table(self.t.st.main)
    }

    fn hints(&self, ti: &TableInfo) -> Vec<Option<&'a FieldDef>> {
        let mut hints = vec![None; ti.fields.len()];
        for (f, fid) in self.t.def.fields.iter().zip(&self.t.st.fields) {
            if let Some(slot) = fid.and_then(|fid| hints.get_mut(usize::from(fid.0))) {
                *slot = Some(f);
            }
        }
        hints
    }

    fn main_row(&self, _i: usize, f: &Flat<'a>) -> Vec<FieldValue> {
        let app = self.t.app;
        let Some(ti) = self.main_table() else {
            return Vec::new();
        };
        let hints = self.hints(ti);
        let mut row = blank_row(app, ti, &hints);
        let number = self.t.st.ref_number.unwrap_or(0);
        let it = f.item;
        set_std(
            &mut row,
            ti,
            StdField::Id,
            FieldValue::Ref {
                table: number,
                id: f.id,
            },
        );
        set_std(
            &mut row,
            ti,
            StdField::PredefinedId,
            FieldValue::Uuid(it.id),
        );
        set_std(
            &mut row,
            ti,
            StdField::Description,
            FieldValue::String(it.description.clone()),
        );
        set_std(
            &mut row,
            ti,
            StdField::Folder,
            FieldValue::Bool(!it.is_folder),
        );
        let parent = f
            .parent
            .and_then(|p| self.flat.get(p))
            .map_or(RefId::NIL, |p| p.id);
        set_std(
            &mut row,
            ti,
            StdField::ParentId,
            FieldValue::Ref {
                table: number,
                id: parent,
            },
        );
        if let Some(code) = ti.field_by_std(StdField::Code) {
            set_std(&mut row, ti, StdField::Code, code_value(ti, code, &it.code));
        }
        if let Some(td) = &it.type_desc {
            set_std(
                &mut row,
                ti,
                StdField::Type,
                FieldValue::Binary(type_blob(td)),
            );
        }
        if let Some(a) = &it.account {
            set_std(
                &mut row,
                ti,
                StdField::Kind,
                FieldValue::Number(Numeric::from_i64(match a.account_type {
                    AccountType::Active => 0,
                    AccountType::Passive => 1,
                    AccountType::ActivePassive => 2,
                })),
            );
            set_std(
                &mut row,
                ti,
                StdField::OffBalance,
                FieldValue::Bool(a.off_balance),
            );
            set_std(
                &mut row,
                ti,
                StdField::OrderField,
                FieldValue::String(a.order.clone()),
            );
            for (r, v) in &a.flags {
                if let Some(slot) =
                    field_of_md(ti, r.id).and_then(|fid| row.get_mut(usize::from(fid.0)))
                {
                    *slot = FieldValue::Bool(*v);
                }
            }
        }
        if let Some(c) = &it.calc {
            set_std(
                &mut row,
                ti,
                StdField::BaseAsAct,
                FieldValue::Bool(c.action_period_is_base),
            );
        }
        self.hide_by_usage(&mut row, ti, &hints, it.is_folder);
        row
    }

    fn hide_by_usage(
        &self,
        row: &mut [FieldValue],
        ti: &TableInfo,
        hints: &[Option<&FieldDef>],
        folder: bool,
    ) {
        if !self.t.def.behavior.hierarchy_folders {
            return;
        }
        for (i, h) in hints.iter().enumerate() {
            let Some(h) = h.filter(|h| h.std.is_none()) else {
                continue;
            };
            let hidden = match h.usage {
                Usage::ForItem => folder,
                Usage::ForFolder => !folder,
                Usage::ForFolderAndItem => false,
            };
            let Ok(fid) = u16::try_from(i).map(FieldId) else {
                continue;
            };
            if hidden
                && encode(ti, fid, &FieldValue::Null).is_ok()
                && let Some(slot) = row.get_mut(i)
            {
                *slot = FieldValue::Null;
            }
        }
    }

    fn sub_rows(&self, _i: usize, f: &Flat<'a>) -> Vec<(TableId, Vec<FieldValue>)> {
        let mut out = Vec::new();
        let number = self.t.st.ref_number.unwrap_or(0);
        let owner = FieldValue::Ref {
            table: number,
            id: f.id,
        };
        let Some(obj_storage) = self.t.map.object(self.t.def.md) else {
            return out;
        };
        let find = |std: StdField| -> Option<(TableId, &TableInfo)> {
            obj_storage.tabular.iter().find_map(|(_, t)| {
                let ti = self.t.map.try_table(*t)?;
                ti.field_by_std(std).map(|_| (*t, ti))
            })
        };
        if let Some(edts) = f
            .item
            .account
            .as_ref()
            .and_then(|a| a.ext_dimension_types.as_ref())
            && let Some((table, ti)) = find(StdField::DimKind)
        {
            for (j, edt) in edts.iter().enumerate() {
                let Some(kind) = edt
                    .kind
                    .id
                    .and_then(|g| self.dims.get(&RefId::from_uuid(g).0))
                    .and_then(|v| v.first())
                    .zip(self.dim_number)
                else {
                    continue;
                };
                let mut row = blank_row(self.t.app, ti, &[]);
                let line = FieldValue::Number(Numeric::from_i64(i64::try_from(j + 1).unwrap_or(0)));
                set_std(&mut row, ti, StdField::Id, owner.clone());
                set_std(&mut row, ti, StdField::KeyField, line.clone());
                set_std(&mut row, ti, StdField::LineNo, line);
                set_std(
                    &mut row,
                    ti,
                    StdField::DimKind,
                    FieldValue::Ref {
                        table: kind.1,
                        id: *kind.0,
                    },
                );
                set_std(
                    &mut row,
                    ti,
                    StdField::DimIsMetadata,
                    FieldValue::Bool(true),
                );
                set_std(
                    &mut row,
                    ti,
                    StdField::TurnoverOnly,
                    FieldValue::Bool(edt.turnover),
                );
                for (r, v) in &edt.flags {
                    if let Some(slot) =
                        field_of_md(ti, r.id).and_then(|fid| row.get_mut(usize::from(fid.0)))
                    {
                        *slot = FieldValue::Bool(*v);
                    }
                }
                out.push((table, row));
            }
        }
        if let Some(c) = &f.item.calc {
            let lists = [
                (
                    &c.base,
                    StdField::BaseCkLineNo,
                    StdField::BaseCkBaseCk,
                    StdField::PredefinedBaseCkTableLine,
                ),
                (
                    &c.leading,
                    StdField::LeadingCkLineNo,
                    StdField::LeadingCkLeadingCk,
                    StdField::PredefinedLeadingCkTableLine,
                ),
                (
                    &c.displaced,
                    StdField::DisplacedCkLineNo,
                    StdField::DisplacedCkDisplCk,
                    StdField::PredefinedDisplCkTableLine,
                ),
            ];
            for (list, line_f, kind_f, pre_f) in lists {
                let Some((table, ti)) = find(kind_f) else {
                    continue;
                };
                for (j, r) in list.iter().enumerate() {
                    let Some(target) = self.flat.iter().find(|x| r.id == Some(x.item.id)) else {
                        continue;
                    };
                    let mut row = blank_row(self.t.app, ti, &[]);
                    let line =
                        FieldValue::Number(Numeric::from_i64(i64::try_from(j + 1).unwrap_or(0)));
                    set_std(&mut row, ti, StdField::Id, owner.clone());
                    set_std(&mut row, ti, StdField::KeyField, line.clone());
                    set_std(&mut row, ti, line_f, line);
                    set_std(
                        &mut row,
                        ti,
                        kind_f,
                        FieldValue::Ref {
                            table: number,
                            id: target.id,
                        },
                    );
                    set_std(&mut row, ti, pre_f, FieldValue::Bool(true));
                    out.push((table, row));
                }
            }
        }
        out
    }
}

fn set_std(row: &mut [FieldValue], ti: &TableInfo, std: StdField, v: FieldValue) {
    if let Some(slot) = ti
        .field_by_std(std)
        .and_then(|fid| row.get_mut(usize::from(fid.0)))
    {
        *slot = v;
    }
}

fn field_of_md(ti: &TableInfo, id: Option<Uuid16>) -> Option<FieldId> {
    let id = id?;
    let i = ti.fields.iter().position(|f| f.md == Some(id))?;
    u16::try_from(i).ok().map(FieldId)
}

fn code_value(ti: &TableInfo, fid: FieldId, code: &PredefinedCode) -> FieldValue {
    let numeric = ti
        .field(fid)
        .and_then(|f| f.types.first())
        .is_some_and(|t| t.code == TypeCode::N);
    let text = match code {
        PredefinedCode::Str(s) | PredefinedCode::Number(s) => s.as_str(),
    };
    if numeric {
        FieldValue::Number(
            Numeric::parse(text.trim())
                .or_else(|| Numeric::parse("0"))
                .unwrap_or_else(|| Numeric::from_i64(0)),
        )
    } else {
        FieldValue::String(text.to_string())
    }
}

fn blank_row(app: &Application, ti: &TableInfo, hints: &[Option<&FieldDef>]) -> Vec<FieldValue> {
    ti.fields
        .iter()
        .enumerate()
        .map(|(i, fi)| {
            if is_version(fi) {
                return FieldValue::RowVersion([0; 4]);
            }
            let Ok(fid) = u16::try_from(i).map(FieldId) else {
                return FieldValue::Null;
            };
            let hint = hints.get(i).copied().flatten();
            let mut cands: Vec<FieldValue> = Vec::new();
            match hint.and_then(|h| h.types.single()) {
                Some(TypeAtom::Ref(o)) => {
                    if let Some(n) = app.model().storage_of(*o).and_then(|s| s.ref_number) {
                        cands.push(FieldValue::Ref {
                            table: n,
                            id: RefId::NIL,
                        });
                    }
                    cands.push(FieldValue::Uuid(Uuid16::NIL));
                }
                Some(TypeAtom::Bool) => cands.push(FieldValue::Bool(false)),
                Some(TypeAtom::Number) => cands.extend(Numeric::parse("0").map(FieldValue::Number)),
                Some(TypeAtom::String) => cands.push(FieldValue::String(String::new())),
                Some(TypeAtom::Date) => cands.push(FieldValue::Date(DateTime::EMPTY)),
                Some(TypeAtom::Uuid) => cands.push(FieldValue::Uuid(Uuid16::NIL)),
                None if hint.is_some() => cands.push(FieldValue::Undefined),
                _ => {}
            }
            if let Some(t) = fi.types.first() {
                match t.code {
                    TypeCode::L => cands.push(FieldValue::Bool(false)),
                    TypeCode::N => cands.extend(Numeric::parse("0").map(FieldValue::Number)),
                    TypeCode::S => cands.push(FieldValue::String(String::new())),
                    TypeCode::T => cands.push(FieldValue::Date(DateTime::EMPTY)),
                    TypeCode::R => {
                        if let Some(n) = ibschema::fields::ref_number(&t.ref_table) {
                            cands.push(FieldValue::Ref {
                                table: n,
                                id: RefId::NIL,
                            });
                        }
                    }
                    _ => {}
                }
            }
            cands.push(FieldValue::Uuid(Uuid16::NIL));
            cands.push(FieldValue::Undefined);
            cands.push(FieldValue::Null);
            cands
                .into_iter()
                .find(|c| encode(ti, fid, c).is_ok())
                .unwrap_or(FieldValue::Null)
        })
        .collect()
}

fn type_blob(td: &TypeDescription) -> Vec<u8> {
    let mut text = b"\xEF\xBB\xBF".to_vec();
    text.extend_from_slice(
        format!(
            "{{\"#\",f5c65050-3bbb-11d5-b988-0050bae0a95d,\r\n{}\r\n}}",
            pattern_text(td)
        )
        .as_bytes(),
    );
    let mut out = vec![1, 1];
    out.extend_from_slice(&(text.len() as u64).to_le_bytes());
    out.extend_from_slice(&text);
    out
}

fn pattern_text(td: &TypeDescription) -> String {
    let mut parts = Vec::new();
    for t in &td.types {
        if let Some(id) = t.type_id {
            parts.push(format!("{{\"#\",{id}}}"));
            continue;
        }
        let flag = |fixed: bool| if fixed { 0 } else { 1 };
        match t.qname.as_str() {
            "xs:boolean" => parts.push("{\"B\"}".to_string()),
            "xs:string" => {
                let s = td.string.unwrap_or(md_model::StringQualifiers {
                    length: 0,
                    fixed: false,
                });
                parts.push(format!("{{\"S\",{},{}}}", s.length, flag(s.fixed)));
            }
            "xs:decimal" => {
                if let Some(n) = td.number {
                    parts.push(format!(
                        "{{\"N\",{},{},{}}}",
                        n.digits,
                        n.fraction,
                        u8::from(n.nonnegative)
                    ));
                }
            }
            "xs:dateTime" => parts.push(format!(
                "{{\"D\",\"{}\"}}",
                match td.date {
                    Some(DateFractions::Date) => "D",
                    Some(DateFractions::Time) => "T",
                    _ => "F",
                }
            )),
            "xs:base64Binary" => {
                let b = td.binary.unwrap_or(md_model::BinaryQualifiers {
                    length: 0,
                    fixed: false,
                });
                parts.push(format!("{{\"R\",{},{}}}", b.length, flag(b.fixed)));
            }
            _ => {}
        }
    }
    format!("{{\"Pattern\",\r\n{}\r\n}}", parts.join(",\r\n"))
}

impl Predefined {
    pub(crate) fn update_mode(&self, app: &Application, obj: ObjId) -> Result<i64, HostError> {
        let rd = app.store().read().map_err(|e| store_error(&e))?;
        update_mode(app, &*rd, obj)
    }

    pub(crate) fn set_update_mode(
        &self,
        app: &Application,
        obj: ObjId,
        mode: i64,
    ) -> Result<(), HostError> {
        let Some((t, mdid, modef)) = options_table(app, obj) else {
            return Err(HostError::not_implemented(
                "Менеджер",
                "ОбновлениеПредопределенныхДанных",
            ));
        };
        let md = app.model().obj(obj).md;
        let mut w = app
            .store()
            .write(app.options().lock_timeout)
            .map_err(|e| write_error(&e))?;
        let mut found = None;
        for item in w.scan(t).map_err(|e| store_error(&e))? {
            let (h, row) = item.map_err(|e| store_error(&e))?;
            if matches!(row.get(usize::from(mdid.0)), Some(FieldValue::Uuid(u)) if *u == md) {
                found = Some((h, row));
                break;
            }
        }
        let n = FieldValue::Number(Numeric::from_i64(mode));
        match found {
            Some((h, mut row)) => {
                if let Some(c) = row.get_mut(usize::from(modef.0)) {
                    *c = n;
                }
                w.update(t, h, &row).map_err(|e| store_error(&e))?;
            }
            None => {
                let Some(ti) = app.store().storage().try_table(t) else {
                    return Err(internal("no options table"));
                };
                let mut row = blank_row(app, ti, &[]);
                for (fid, v) in [(mdid, FieldValue::Uuid(md)), (modef, n)] {
                    if let Some(c) = row.get_mut(usize::from(fid.0)) {
                        *c = v;
                    }
                }
                w.insert(t, &row).map_err(|e| store_error(&e))?;
            }
        }
        w.commit().map_err(|e| store_error(&e))?;
        app.bump_write_epoch();
        Ok(())
    }
}

pub(crate) fn item_name(app: &Application, obj: ObjId, i: usize) -> Option<String> {
    items_of(app, obj)?.iter().nth(i).map(|it| it.name.clone())
}

pub(crate) fn item_position(app: &Application, obj: ObjId, name: &str) -> Option<usize> {
    items_of(app, obj)?
        .iter()
        .position(|it| same_name(&it.name, name))
}

const UPDATE_MODES: [&str; 3] = ["Авто", "ОбновлятьАвтоматически", "НеОбновлятьАвтоматически"];

fn update_enum() -> Option<platform_catalog::TypeIdx> {
    platform_catalog::catalog().find_type("ОбновлениеПредопределенныхДанных")
}

pub(crate) fn update_mode_value(mode: i64) -> Option<bsl_vm::Value> {
    let name = UPDATE_MODES.get(usize::try_from(mode).ok()?)?;
    bsl_stdlib::enums::enum_value(update_enum()?, name)
}

pub(crate) fn update_mode_of(v: &bsl_vm::Value) -> Option<i64> {
    let e = bsl_stdlib::enums::as_enum_value(v)?;
    if Some(e.type_idx()) != update_enum() {
        return None;
    }
    UPDATE_MODES
        .iter()
        .position(|n| *n == e.name_ru())
        .and_then(|i| i64::try_from(i).ok())
}

pub(crate) fn register(app: &Arc<Application>, reg: &mut bsl_vm::Registry) {
    let app_for_init = app.clone();
    let app = app.clone();
    reg.global_fn(
        "ПредопределенноеЗначение",
        "PredefinedValue",
        Arc::new(move |ex, args| {
            let v = crate::meta::arg_value(ex, args, 0)?;
            let bsl_vm::Value::String(s) = &v else {
                return Err(crate::meta::type_mismatch(1).into());
            };
            Ok(predefined_value(&app, &s.to_string_lossy())?)
        }),
    );
    let app = app_for_init;
    reg.global_fn(
        "ИнициализироватьПредопределенныеДанные",
        "InitializePredefinedData",
        Arc::new(move |_, _| {
            initialize_all(&app)?;
            Ok(bsl_vm::Value::Undefined)
        }),
    );
}

fn initialize_all(app: &Application) -> Result<(), HostError> {
    let ids: Vec<ObjId> = app
        .model()
        .objects
        .iter()
        .filter(|o| o.behavior.predefined_count > 0)
        .map(|o| o.id)
        .collect();
    for obj in ids {
        app.predefined().ensure(app, obj, &AreaKey::default())?;
    }
    Ok(())
}

fn predefined_value(app: &Application, text: &str) -> Result<bsl_vm::Value, HostError> {
    let not_found = || l5(keys::PREDEFINED_VALUE_NOT_FOUND, &[text]);
    let parts: Vec<&str> = text.split('.').collect();
    let (Some(kind), Some(name)) = (parts.first(), parts.get(1)) else {
        return Err(not_found());
    };
    let obj = ObjKind::from_word(kind)
        .and_then(|k| app.model().find(k, name))
        .ok_or_else(not_found)?;
    let def = app.model().obj(obj);
    if !def.kind.is_reference() {
        return Err(not_found());
    }
    let rest = parts.get(2..).unwrap_or_default();
    let item = match rest {
        [] => {
            let ty = app
                .types()
                .and_then(|t| t.type_of(obj, crate::types::Category::Ref))
                .ok_or_else(|| internal("no reference type for the object"))?;
            return Ok(bsl_vm::Value::Type(ty));
        }
        [item] => *item,
        _ => "",
    };
    if rest.len() == 1 && matches!(fold_name(item).as_str(), "пустаяссылка" | "emptyref")
    {
        return crate::values::refs::empty_ref(app, obj);
    }
    if def.kind == ObjKind::Enum {
        let found = match rest {
            [item] => crate::managers::enum_value(app, obj, item)?,
            _ => None,
        };
        return found.ok_or_else(|| {
            other(
                keys::PREDEFINED_NOT_IN_DATA,
                &[&full_name(def, &rest.join("."))],
            )
        });
    }
    if rest.len() != 1 {
        return Err(other(keys::PREDEFINED_ITEM_NOT_EXISTS, &[]));
    }
    let r = app
        .predefined()
        .by_name_with(app, obj, &AreaKey::default(), item, other)?;
    crate::values::refs::ref_value(app, r)
}
