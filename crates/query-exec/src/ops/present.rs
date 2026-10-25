use std::collections::HashMap;
use std::sync::atomic::Ordering;

use query_ir::host::PresentationInput;
use query_ir::{ExecError, PresentTarget, QStr, QType, QValue, StmtL};
use query_syntax::Lang;

use super::{MemBudget, fatal};
use crate::host::ExecHost;
use crate::model::{HashModel, block, value_model};
use crate::source::{DataSource, Row};

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
    if stmt.present.is_empty() {
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
        for &c in &stmt.present {
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
