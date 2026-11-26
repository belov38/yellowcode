use std::collections::HashMap;
use std::sync::atomic::Ordering;

use query_ir::host::PresentationInput;
use query_ir::{ExecError, PresentTarget, QStr, QType, QValue, RefAtom, StmtL};
use query_syntax::Lang;

use super::{Cx, MemBudget, fatal};
use crate::host::ExecHost;
use crate::model::{HashModel, block, value_model};
use crate::source::{DataSource, Row};

pub(crate) trait Presenter {
    fn lang(&self) -> Lang;
    fn type_name(&self, atom: RefAtom) -> String;
    fn host_presentation(&self, input: &PresentationInput) -> Result<QStr, ExecError>;
}

pub(crate) struct HostPresenter<'a>(pub &'a dyn ExecHost);

impl Presenter for HostPresenter<'_> {
    fn lang(&self) -> Lang {
        self.0.lang()
    }

    fn type_name(&self, atom: RefAtom) -> String {
        self.0.schema().ref_info(atom).name.clone()
    }

    fn host_presentation(&self, input: &PresentationInput) -> Result<QStr, ExecError> {
        self.0.presentation(input).map_err(ExecError::Host)
    }
}

pub(crate) struct PresentCache {
    cache: HashMap<(u32, [u8; 16]), QValue>,
    cache_model: HashModel,
    sys_cache: HashMap<(platform_catalog::TypeIdx, u32), QValue>,
    sys_model: HashModel,
    held: u64,
}

impl PresentCache {
    pub(crate) fn new() -> PresentCache {
        PresentCache {
            cache: HashMap::new(),
            cache_model: HashModel::new(
                (std::mem::size_of::<(u32, [u8; 16])>() + crate::model::VALUE_SIZE) as u64,
            ),
            sys_cache: HashMap::new(),
            sys_model: HashModel::new(
                (std::mem::size_of::<(platform_catalog::TypeIdx, u32)>() + crate::model::VALUE_SIZE)
                    as u64,
            ),
            held: 0,
        }
    }

    pub(crate) fn clear(&mut self, mem: &MemBudget<'_>) {
        self.cache = HashMap::new();
        self.cache_model = HashModel::new(
            (std::mem::size_of::<(u32, [u8; 16])>() + crate::model::VALUE_SIZE) as u64,
        );
        self.sys_cache = HashMap::new();
        self.sys_model = HashModel::new(
            (std::mem::size_of::<(platform_catalog::TypeIdx, u32)>() + crate::model::VALUE_SIZE)
                as u64,
        );
        let held = std::mem::take(&mut self.held);
        if held > 0 {
            mem.shrink(held);
        }
    }
}

pub(crate) fn eval_inline(v: &QValue, cx: &Cx<'_>) -> Result<QValue, ExecError> {
    let Some(presenter) = cx.presenter else {
        return Err(ExecError::Later {
            what: "ПРЕДСТАВЛЕНИЕ без хоста",
            stage: "Q3",
        });
    };
    let Some(cache_cell) = cx.present.as_ref() else {
        return Err(fatal("inline presentation without cache"));
    };
    Ok(match v {
        QValue::Null => QValue::Null,
        QValue::Undefined => string(""),
        QValue::String(_) => v.clone(),
        QValue::Bool(_) | QValue::Number(_) | QValue::Date(_) => {
            QValue::String(QStr(crate::eval::text_of_charged(v, &cx.mem)?.into()))
        }
        QValue::Ref { table, id } => {
            if *id == [0; 16] {
                return Ok(string(""));
            }
            let key = (*table, *id);
            if let Some(hit) = cache_cell
                .try_borrow()
                .map_err(|_| fatal("re-entrant inline presentation cache"))?
                .cache
                .get(&key)
                .cloned()
            {
                return Ok(hit);
            }
            let s = inline_reference(v, *table, id, presenter, cx)?;
            let val_bytes = value_model(&s);
            cx.mem.check_value(val_bytes)?;
            let mut state = cache_cell
                .try_borrow_mut()
                .map_err(|_| fatal("re-entrant inline presentation cache"))?;
            let growth = state.cache_model.growth();
            let total = growth.saturating_add(val_bytes);
            cx.mem.grow(total)?;
            state.held = state.held.saturating_add(total);
            state.cache.insert(key, s.clone());
            let old = state.cache_model.inserted();
            if old > 0 {
                cx.mem.shrink(old);
                state.held = state.held.saturating_sub(old);
            }
            s
        }
        QValue::SysEnum(t, i) => {
            let key = (*t, *i);
            if let Some(hit) = cache_cell
                .try_borrow()
                .map_err(|_| fatal("re-entrant inline presentation cache"))?
                .sys_cache
                .get(&key)
                .cloned()
            {
                return Ok(hit);
            }
            let input = PresentationInput {
                reference: v.clone(),
                table: None,
                found: true,
                fields: Vec::new(),
            };
            cx.mem.grow(FIXED_PRESENT)?;
            let presented = presenter.host_presentation(&input);
            cx.mem.shrink(FIXED_PRESENT);
            let s = QValue::String(presented?);
            let val_bytes = value_model(&s);
            cx.mem.check_value(val_bytes)?;
            let mut state = cache_cell
                .try_borrow_mut()
                .map_err(|_| fatal("re-entrant inline presentation cache"))?;
            let growth = state.sys_model.growth();
            let total = growth.saturating_add(val_bytes);
            cx.mem.grow(total)?;
            state.held = state.held.saturating_add(total);
            state.sys_cache.insert(key, s.clone());
            let old = state.sys_model.inserted();
            if old > 0 {
                cx.mem.shrink(old);
                state.held = state.held.saturating_sub(old);
            }
            s
        }
        QValue::Type(t) => match t.refs.as_slice() {
            [r] if t.others.is_empty() && !t.null && !t.boolean => {
                let s = string(&presenter.type_name(*r));
                cx.mem.grow_scratch(value_model(&s))?;
                s
            }
            [] if *t == QType::undefined() || *t == QType::null_only() => string(""),
            _ => {
                return Err(ExecError::Later {
                    what: "ПРЕДСТАВЛЕНИЕ типа, не являющегося ссылочным",
                    stage: "Q3",
                });
            }
        },
        QValue::Uuid(_)
        | QValue::Binary(_)
        | QValue::Storage(_)
        | QValue::Nested(_)
        | QValue::PointInTime(_)
        | QValue::Boundary(_) => {
            return Err(ExecError::Later {
                what: "ПРЕДСТАВЛЕНИЕ значения этого типа",
                stage: "Q3",
            });
        }
    })
}

fn inline_reference(
    v: &QValue,
    table: u32,
    id: &[u8; 16],
    presenter: &dyn Presenter,
    cx: &Cx<'_>,
) -> Result<QValue, ExecError> {
    let Some(target) = cx.present_targets.iter().find(|t| t.number == table) else {
        return Ok(string(&not_found(presenter.lang(), table, id)));
    };
    if target.route_point {
        return Ok(string("<>"));
    }
    let wanted: Vec<ibschema::storage::FieldId> = target.fields.iter().map(|(_, f)| *f).collect();
    let Some(row) = cx.src.get_by_id_fields(target.storage, v, &wanted)? else {
        return Ok(string(&not_found(presenter.lang(), table, id)));
    };
    let fields: Vec<(query_ir::schema::StdRole, QValue)> = target
        .fields
        .iter()
        .map(|(role, f)| {
            let value = row.get(usize::from(f.0)).cloned().unwrap_or(QValue::Null);
            (*role, value)
        })
        .collect();
    let inputs: u64 = fields.iter().map(|(_, v)| 2 * value_model(v)).sum();
    let temp = inputs.saturating_add(FIXED_PRESENT);
    cx.mem.grow(temp)?;
    let input = PresentationInput {
        reference: v.clone(),
        table: target.table,
        found: true,
        fields,
    };
    let presented = presenter.host_presentation(&input);
    cx.mem.shrink(temp);
    Ok(QValue::String(presented?))
}

const FIXED_PRESENT: u64 = 256;

pub fn not_found(lang: Lang, table: u32, id: &[u8; 16]) -> String {
    let head = match lang {
        Lang::Ru => "<Объект не найден>",
        Lang::En => "<Object not found>",
    };
    let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
    format!("{head} ({table}:{hex})")
}

pub fn apply(
    stmt: &StmtL,
    rows: &mut [Row],
    host: &dyn ExecHost,
    src: &dyn DataSource,
    mem: &mut MemBudget<'_>,
) -> Result<(), ExecError> {
    apply_columns(stmt, rows, &stmt.present, host, src, mem)
}

pub fn apply_totals(
    stmt: &StmtL,
    tree: &mut crate::totals::TotalsTree,
    host: &dyn ExecHost,
    src: &dyn DataSource,
    mem: &mut MemBudget<'_>,
) -> Result<(), ExecError> {
    if stmt.present.is_empty() || tree.totals.is_empty() {
        return Ok(());
    }
    let cols: Vec<u16> = stmt
        .present
        .iter()
        .copied()
        .filter(|c| {
            !tree
                .spec
                .items
                .iter()
                .any(|(_, col)| *col == usize::from(*c))
        })
        .collect();
    apply_columns(stmt, &mut tree.totals, &cols, host, src, mem)
}

fn apply_columns(
    stmt: &StmtL,
    rows: &mut [Row],
    columns: &[u16],
    host: &dyn ExecHost,
    src: &dyn DataSource,
    mem: &mut MemBudget<'_>,
) -> Result<(), ExecError> {
    if columns.is_empty() {
        return Ok(());
    }
    let env = host.env();
    let mut step = Step {
        stmt,
        host,
        src,
        lang: host.lang(),
        cache: HashMap::new(),
        cache_model: HashModel::new(
            (std::mem::size_of::<(u32, [u8; 16])>() + crate::model::VALUE_SIZE) as u64,
        ),
        sys_cache: HashMap::new(),
        sys_model: HashModel::new(
            (std::mem::size_of::<(platform_catalog::TypeIdx, u32)>() + crate::model::VALUE_SIZE)
                as u64,
        ),
    };
    let mut shared = crate::model::Payloads::default();
    for (n, row) in rows.iter_mut().enumerate() {
        if n.is_multiple_of(1024) && env.cancel.load(Ordering::Relaxed) {
            return Err(ExecError::Fatal("выполнение прервано".to_string()));
        }
        for &c in columns {
            let Some(v) = row.get_mut(usize::from(c)) else {
                return Err(fatal("a presentation column outside the row"));
            };
            let new = step.value(v, mem)?;
            *v = new;
            let (bytes, old) = shared.value_model_charged(v, &mut |n| mem.grow(n))?;
            mem.grow(bytes)?;
            mem.shrink(old);
        }
    }
    mem.shrink(shared.table());
    Ok(())
}

struct Step<'a> {
    stmt: &'a StmtL,
    host: &'a dyn ExecHost,
    src: &'a dyn DataSource,
    lang: Lang,
    cache: HashMap<(u32, [u8; 16]), QValue>,
    cache_model: HashModel,
    sys_cache: HashMap<(platform_catalog::TypeIdx, u32), QValue>,
    sys_model: HashModel,
}

fn string(s: &str) -> QValue {
    QValue::String(QStr::from_str(s))
}

fn cache_insert<K: std::hash::Hash + Eq>(
    cache: &mut HashMap<K, QValue>,
    model: &mut HashModel,
    key: K,
    value: &QValue,
    mem: &MemBudget<'_>,
) -> Result<(), ExecError> {
    mem.grow(model.growth())?;
    cache.insert(key, value.clone());
    mem.shrink(model.inserted());
    Ok(())
}

impl Step<'_> {
    fn value(&mut self, v: &QValue, mem: &mut MemBudget<'_>) -> Result<QValue, ExecError> {
        Ok(match v {
            QValue::Null => QValue::Null,
            QValue::Undefined => string(""),
            QValue::String(_) => v.clone(),
            QValue::Bool(_) | QValue::Number(_) | QValue::Date(_) => {
                QValue::String(QStr(crate::eval::text_of_charged(v, mem)?.into()))
            }
            QValue::Ref { table, id } => {
                if *id == [0; 16] {
                    return Ok(string(""));
                }
                let key = (*table, *id);
                if let Some(s) = self.cache.get(&key) {
                    return Ok(s.clone());
                }
                let s = self.reference(v, *table, id, mem)?;
                cache_insert(&mut self.cache, &mut self.cache_model, key, &s, mem)?;
                s
            }
            QValue::SysEnum(t, i) => {
                if let Some(s) = self.sys_cache.get(&(*t, *i)) {
                    return Ok(s.clone());
                }
                let input = PresentationInput {
                    reference: v.clone(),
                    table: None,
                    found: true,
                    fields: Vec::new(),
                };
                mem.grow(FIXED_PRESENT)?;
                let s = QValue::String(self.host.presentation(&input).map_err(ExecError::Host)?);
                cache_insert(&mut self.sys_cache, &mut self.sys_model, (*t, *i), &s, mem)?;
                s
            }
            QValue::Type(t) => self.type_name(t)?,
            QValue::Uuid(_)
            | QValue::Binary(_)
            | QValue::Storage(_)
            | QValue::Nested(_)
            | QValue::PointInTime(_)
            | QValue::Boundary(_) => {
                return Err(ExecError::Later {
                    what: "ПРЕДСТАВЛЕНИЕ значения этого типа",
                    stage: "Q3",
                });
            }
        })
    }

    fn type_name(&self, t: &QType) -> Result<QValue, ExecError> {
        match t.refs.as_slice() {
            [r] if t.others.is_empty() && !t.null && !t.boolean => {
                Ok(string(&self.host.schema().ref_info(*r).name))
            }
            [] if *t == QType::undefined() || *t == QType::null_only() => Ok(string("")),
            _ => Err(ExecError::Later {
                what: "ПРЕДСТАВЛЕНИЕ типа, не являющегося ссылочным",
                stage: "Q3",
            }),
        }
    }

    fn target(&self, number: u32) -> Option<&PresentTarget> {
        self.stmt
            .present_targets
            .iter()
            .find(|t| t.number == number)
    }

    fn reference(
        &mut self,
        v: &QValue,
        table: u32,
        id: &[u8; 16],
        mem: &MemBudget<'_>,
    ) -> Result<QValue, ExecError> {
        let Some(target) = self.target(table).cloned() else {
            mem.grow(block(64))?;
            return Ok(string(&not_found(self.lang, table, id)));
        };
        if target.route_point {
            return Ok(string("<>"));
        }
        let wanted: Vec<ibschema::storage::FieldId> =
            target.fields.iter().map(|(_, f)| *f).collect();
        let Some(row) = self.src.get_by_id_fields(target.storage, v, &wanted)? else {
            mem.grow(block(64))?;
            return Ok(string(&not_found(self.lang, table, id)));
        };
        let fields: Vec<(query_ir::schema::StdRole, QValue)> = target
            .fields
            .iter()
            .map(|(role, f)| {
                let value = row.get(usize::from(f.0)).cloned().unwrap_or(QValue::Null);
                (*role, value)
            })
            .collect();
        let inputs: u64 = fields.iter().map(|(_, v)| 2 * value_model(v)).sum();
        mem.grow(inputs + FIXED_PRESENT)?;
        let input = PresentationInput {
            reference: v.clone(),
            table: target.table,
            found: true,
            fields,
        };
        let s = self.host.presentation(&input).map_err(ExecError::Host)?;
        Ok(QValue::String(s))
    }
}
