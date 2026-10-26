use super::layout::{R, Row, fatal, internal, later, p16};
use super::rel::Lower;
use super::{PExpr, RelL};
use crate::ir::{ColSel, Expr, SrcId};
use crate::schema::{FieldRole, Only, QFieldId, QTableId, StdRole, TableKind};
use crate::tir;
use crate::types::Other;
use crate::value::QValue;
use bsl_prim::fold_name;
use ibschema::names::StdField;
use ibschema::storage::{FieldId, TableId};
use query_syntax::ast::{CmpOp, JoinKind};
use std::rc::Rc;
use std::sync::Arc;

pub(super) type Sf = (TableId, FieldId);

#[derive(Clone, Debug)]
pub(super) enum Plan {
    Null,
    Col(Sf),
    Not(Sf),
    NonZero(Sf),
    SysEnum(Sf, platform_catalog::TypeIdx),
    PredefName(Sf, Vec<([u8; 16], String)>),
    RefType(Sf, Vec<(u32, crate::types::RefAtom)>),
    Only {
        inner: Box<Plan>,
        folder: Sf,
        groups: bool,
    },
}

impl Plan {
    fn needs_match_guard(&self) -> bool {
        match self {
            Plan::PredefName(..) | Plan::RefType(..) => true,
            Plan::Only { inner, .. } => inner.needs_match_guard(),
            _ => false,
        }
    }
    pub fn needs(&self, out: &mut Vec<Sf>) {
        match self {
            Plan::Null => {}
            Plan::Col(s)
            | Plan::Not(s)
            | Plan::NonZero(s)
            | Plan::SysEnum(s, _)
            | Plan::PredefName(s, _)
            | Plan::RefType(s, _) => out.push(*s),
            Plan::Only { inner, folder, .. } => {
                inner.needs(out);
                out.push(*folder);
            }
        }
    }
}

pub(super) fn plan_expr(plan: &Plan, mk: &mut dyn FnMut(Sf) -> R<PExpr>) -> R<PExpr> {
    Ok(match plan {
        Plan::Null => PExpr::Lit(QValue::Null),
        Plan::Col(s) => mk(*s)?,
        Plan::Not(s) => PExpr::Not(Box::new(mk(*s)?)),
        Plan::NonZero(s) => PExpr::Compare(
            CmpOp::Ne,
            Box::new(mk(*s)?),
            Box::new(PExpr::Lit(QValue::Binary(
                Arc::<[u8]>::from(vec![0u8; 16]).into(),
            ))),
        ),
        Plan::RefType(s, refs) => PExpr::TypeOf {
            x: Box::new(mk(*s)?),
            refs: refs.clone(),
            route_points: Vec::new(),
        },
        Plan::PredefName(s, names) => PExpr::Case {
            operand: Some(Box::new(mk(*s)?)),
            whens: names
                .iter()
                .map(|(id, name)| {
                    (
                        PExpr::Lit(QValue::Binary(Arc::<[u8]>::from(id.as_slice()).into())),
                        PExpr::Lit(QValue::String(crate::value::QStr::from_str(name))),
                    )
                })
                .collect(),
            else_: Some(Box::new(PExpr::Lit(QValue::String(
                crate::value::QStr::from_str(""),
            )))),
        },
        Plan::SysEnum(s, ty) => {
            let num = |n: i64| QValue::Number(bsl_prim::Number::from_scaled_i64(n, 0));
            PExpr::Case {
                operand: Some(Box::new(mk(*s)?)),
                whens: (0..2u32)
                    .map(|i| {
                        (
                            PExpr::Lit(num(i64::from(i))),
                            PExpr::Lit(QValue::SysEnum(*ty, i)),
                        )
                    })
                    .collect(),
                else_: None,
            }
        }
        Plan::Only {
            inner,
            folder,
            groups,
        } => {
            let f = mk(*folder)?;
            let cond = if *groups { PExpr::Not(Box::new(f)) } else { f };
            PExpr::Case {
                operand: None,
                whens: vec![(cond, plan_expr(inner, mk)?)],
                else_: None,
            }
        }
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum NodeKey {
    Base(SrcId, ColSel),
    Expr(String),
    Step(u32, String),
    Policy(u32),
}

#[derive(Clone, Debug)]
pub(super) struct Target {
    pub plan: Rc<Plan>,
    pub cols: Vec<Sf>,
    pub rel: i64,
}

#[derive(Clone, Debug)]
pub(super) struct Chain {
    pub anchor: Option<SrcId>,
    pub targets: Vec<Target>,
}

enum Cur {
    Base,
    Chain(u32),
}

impl Lower<'_> {
    pub(super) fn plan(&mut self, qt: QTableId, f: QFieldId) -> R<Rc<Plan>> {
        if let Some(p) = self.plans.get(&(qt, f)) {
            return Ok(p.clone());
        }
        let p = Rc::new(self.make_plan(qt, f)?);
        self.plans.insert((qt, f), p.clone());
        Ok(p)
    }

    fn make_plan(&mut self, qt: QTableId, f: QFieldId) -> R<Plan> {
        let t = self.schema.table(qt);
        let Some(fld) = t.fields.get(usize::from(f.0)) else {
            return internal("a field of a missing schema field");
        };
        if fld.nested.is_some() {
            return later("табличная часть в выражении", "Q3");
        }
        if t.kind == TableKind::Constants {
            let found = fld.md.and_then(|md| self.map.field(md));
            return match found {
                Some(s) => Ok(Plan::Col(s)),
                None => fatal(format!("константа {} не привязана к хранению", fld.name_ru)),
            };
        }
        if self.schema.is_unstored(qt) {
            return Ok(Plan::Null);
        }
        let Some(tid) = self.storage.table(qt) else {
            return fatal(format!("таблица {} не привязана к хранению", t.name_ru));
        };
        let Some(fid) = self.storage.field(qt, f) else {
            if matches!(fld.role, FieldRole::Std(StdRole::DataVersion)) {
                return Ok(Plan::Null);
            }
            if matches!(fld.role, FieldRole::Std(StdRole::Type)) {
                let r = t
                    .fields
                    .iter()
                    .enumerate()
                    .filter(|(_, x)| matches!(x.role, FieldRole::Std(StdRole::Ref)))
                    .find_map(|(i, _)| self.storage.field(qt, QFieldId(u16::try_from(i).ok()?)));
                if let Some(rf) = r {
                    return Ok(Plan::RefType((tid, rf), self.storage.ref_atoms()));
                }
            }
            if fld.role == FieldRole::Virtual {
                return later(
                    "вычисляемое поле",
                    if fld.name_ru == "Представление" {
                        "Q3"
                    } else {
                        "Q5"
                    },
                );
            }
            return fatal(format!(
                "поле {}.{} не имеет хранения",
                t.name_ru, fld.name_ru
            ));
        };
        let Some(info) = self.map.try_table(tid) else {
            return internal("a bound table is not in the storage map");
        };
        let std = info.field(fid).and_then(|x| x.std);
        let sf = (tid, fid);
        let base = match (fld.role, std) {
            (FieldRole::Std(StdRole::IsFolder), Some(StdField::Folder)) => Plan::Not(sf),
            (
                FieldRole::Std(StdRole::Predefined | StdRole::ThisNode),
                Some(StdField::PredefinedId),
            ) => Plan::NonZero(sf),
            (FieldRole::Std(StdRole::PredefinedDataName), Some(StdField::PredefinedId)) => {
                let names = match self.predef_names.get(&qt) {
                    Some(n) => n.clone(),
                    None => {
                        if !self.predef_wanted.contains(&qt) {
                            self.predef_wanted.push(qt);
                        }
                        Vec::new()
                    }
                };
                Plan::PredefName(sf, names)
            }
            (FieldRole::Std(StdRole::PredefinedDataName), _) => {
                return fatal(format!(
                    "поле {}.{}: имя предопределенного элемента без хранения",
                    t.name_ru, fld.name_ru
                ));
            }
            (FieldRole::Std(StdRole::RecordKind), _) => {
                match fld.ty.others.iter().find_map(|o| match o {
                    Other::SysEnum(t) => Some(*t),
                    _ => None,
                }) {
                    Some(ty) => Plan::SysEnum(sf, ty),
                    None => Plan::Col(sf),
                }
            }
            _ => Plan::Col(sf),
        };
        Ok(match fld.only {
            Only::Any => base,
            o => match info.field_by_std(StdField::Folder) {
                Some(ff) => Plan::Only {
                    inner: Box::new(base),
                    folder: (tid, ff),
                    groups: o == Only::Folders,
                },
                None if o == Only::Folders => Plan::Null,
                None => base,
            },
        })
    }

    fn node(&mut self, key: NodeKey) -> u32 {
        if let Some(n) = self.nodes.get(&key) {
            return *n;
        }
        let n = self.fresh_node();
        self.nodes.insert(key, n);
        n
    }

    fn fresh_node(&mut self) -> u32 {
        self.next_node += 1;
        self.next_node
    }

    fn anchored(&mut self, anchor: Option<SrcId>, rel: i64) -> R<u16> {
        match anchor {
            Some(a) => p16(self.offs.offset(a) + rel),
            None => p16(rel),
        }
    }

    fn chain_valid(&mut self, node: u32, row: &Row) -> bool {
        let Some(a) = self.chains.get(&node).and_then(|c| c.anchor) else {
            return false;
        };
        match row.rep {
            Some(r) => self.offs.same(a, r),
            None => false,
        }
    }

    fn chain_area(&mut self, node: u32) -> R<Vec<(String, PExpr)>> {
        let Some(chain) = self.chains.get(&node).cloned() else {
            return Ok(Vec::new());
        };
        let [t] = chain.targets.as_slice() else {
            return Ok(Vec::new());
        };
        let Some(&(tid, _)) = t.cols.first() else {
            return Ok(Vec::new());
        };
        let Some(info) = self.map.try_table(tid) else {
            return Ok(Vec::new());
        };
        let start = self.anchored(chain.anchor, t.rel)?;
        let mut out = Vec::with_capacity(info.separators.len());
        for s in &info.separators {
            let Some(pos) = t.cols.iter().position(|c| *c == (tid, s.field)) else {
                return Ok(Vec::new());
            };
            let name = info
                .field(s.field)
                .map(|f| f.md_name.clone())
                .unwrap_or_default();
            out.push((name, PExpr::Col(p16(i64::from(start) + pos as i64)?)));
        }
        Ok(out)
    }

    fn chain_value(&mut self, node: u32) -> R<PExpr> {
        let Some(chain) = self.chains.get(&node).cloned() else {
            return internal("a missing dereference chain");
        };
        let guard_single = chain
            .targets
            .first()
            .is_some_and(|t| t.plan.needs_match_guard());
        let mut values = Vec::with_capacity(chain.targets.len());
        let mut matched = Vec::with_capacity(chain.targets.len());
        for t in &chain.targets {
            let start = self.anchored(chain.anchor, t.rel)?;
            let mut mk = |sf: Sf| -> R<PExpr> {
                match t.cols.iter().position(|c| *c == sf) {
                    Some(i) => Ok(PExpr::Col(p16(i64::from(start) + i as i64)?)),
                    None => internal("a column of a dereference join is not scanned"),
                }
            };
            values.push(plan_expr(&t.plan, &mut mk)?);
            matched.push(PExpr::IsNull {
                l: Box::new(PExpr::Col(start)),
                not: true,
            });
        }
        Ok(match values.len() {
            0 => PExpr::Lit(QValue::Null),
            1 if !guard_single => values.pop().unwrap_or(PExpr::Lit(QValue::Null)),
            _ => PExpr::Case {
                operand: None,
                whens: matched.into_iter().zip(values).collect(),
                else_: None,
            },
        })
    }

    fn step_joins(
        &mut self,
        row: &mut Row,
        key: &PExpr,
        area: &[(String, PExpr)],
        step: &tir::Deref,
        base: &Expr,
        prefix: &[tir::Deref],
    ) -> R<Chain> {
        let anchor = row.rep;
        let anchor_off = match anchor {
            Some(a) => self.offs.offset(a),
            None => 0,
        };
        let mut targets = Vec::with_capacity(step.targets.len());
        for (qt, qf) in &step.targets {
            let plan = self.plan(*qt, *qf)?;
            let Some(tid) = self.storage.table(*qt) else {
                return fatal(format!(
                    "таблица {} не привязана к хранению",
                    self.schema.table(*qt).name_ru
                ));
            };
            let id = self
                .map
                .try_table(tid)
                .and_then(|t| t.field_by_std(StdField::Id));
            let Some(id) = id else {
                return fatal(format!(
                    "таблица {} не имеет поля ID",
                    self.schema.table(*qt).name_ru
                ));
            };
            let mut cols: Vec<Sf> = vec![(tid, id)];
            let mut need = Vec::new();
            plan.needs(&mut need);
            for c in need {
                if !cols.contains(&c) {
                    cols.push(c);
                }
            }
            let seps: Vec<(Sf, PExpr)> = self
                .map
                .try_table(tid)
                .map(|t| {
                    t.separators
                        .iter()
                        .filter_map(|s| {
                            let md = &t.field(s.field)?.md_name;
                            area.iter()
                                .find(|(n, _)| !n.is_empty() && n == md)
                                .map(|(_, e)| ((tid, s.field), e.clone()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let in_area = self
                .map
                .try_table(tid)
                .is_some_and(|t| !t.separators.is_empty() && seps.len() == t.separators.len());
            if in_area {
                for (sf, _) in &seps {
                    if !cols.contains(sf) {
                        cols.push(*sf);
                    }
                }
            }
            let start = p16(row.width as i64)?;
            let secured = if self.policy_depth == 0 {
                self.b.security.target(self.schema, base, prefix, *qt)
            } else {
                None
            };
            let (scan, scan_width, scan_signal) = if let Some(src) = secured {
                for field in self.scan_cols.get(src.0 as usize).into_iter().flatten() {
                    if !cols.contains(field) {
                        cols.push(*field);
                    }
                }
                let idx = cols
                    .iter()
                    .enumerate()
                    .map(|(i, c)| Ok((*c, p16(i as i64)?)))
                    .collect::<R<std::collections::HashMap<_, _>>>()?;
                if let Some(target) = self.scan_cols.get_mut(src.0 as usize) {
                    *target = cols.clone();
                }
                if let Some(target) = self.scan_idx.get_mut(src.0 as usize) {
                    *target = idx;
                }
                let right = self.scan(src, *qt)?;
                (right.rel, right.width, right.signal)
            } else {
                (
                    RelL::Scan {
                        table: tid,
                        fields: cols.iter().map(|c| c.1).collect(),
                    },
                    cols.len(),
                    None,
                )
            };
            let mut on = vec![PExpr::Compare(
                CmpOp::Eq,
                Box::new(key.clone()),
                Box::new(PExpr::Col(start)),
            )];
            let mut eq = vec![(key.clone(), PExpr::Col(0))];
            if in_area {
                for (sf, e) in &seps {
                    let Some(pos) = cols.iter().position(|c| c == sf) else {
                        return internal("a separator column of a dereference join is not scanned");
                    };
                    let rel = p16(pos as i64)?;
                    on.push(PExpr::Compare(
                        CmpOp::Eq,
                        Box::new(e.clone()),
                        Box::new(PExpr::Col(p16(i64::from(start) + pos as i64)?)),
                    ));
                    eq.push((e.clone(), PExpr::Col(rel)));
                }
            }
            let old = row.take();
            row.rel = RelL::Join {
                kind: JoinKind::Left,
                l: old,
                r: Box::new(scan),
                on: if on.len() == 1 {
                    on.pop().unwrap_or(PExpr::Lit(QValue::Null))
                } else {
                    PExpr::And(on)
                },
                eq,
                by_id: true,
            };
            let signal = if row.signal.is_some() || scan_signal.is_some() {
                Some(PExpr::And(vec![
                    row.signal_expr(),
                    scan_signal
                        .map(|i| p16(i64::from(i) + i64::from(start)).map(super::security::neutral))
                        .transpose()?
                        .unwrap_or(PExpr::Lit(QValue::Bool(true))),
                ]))
            } else {
                None
            };
            row.width += scan_width;
            if let Some(signal) = signal {
                row.append_signal(signal)?;
            }
            self.added += 1;
            targets.push(Target {
                plan,
                cols,
                rel: i64::from(start) - anchor_off,
            });
        }
        Ok(Chain { anchor, targets })
    }

    pub(super) fn deref_chain(
        &mut self,
        row: &mut Row,
        base: &Expr,
        steps: &[tir::Deref],
    ) -> R<PExpr> {
        let mut node = match &base.kind {
            crate::ir::ExprKind::Col { src, col } => self.node(NodeKey::Base(*src, *col)),
            _ => self.node(NodeKey::Expr(format!("{:?}", base))),
        };
        if self.policy_depth > 0 {
            node = self.node(NodeKey::Policy(node));
        }
        let mut cur = Cur::Base;
        for (i, step) in steps.iter().enumerate() {
            let child = self.node(NodeKey::Step(node, fold_name(&step.name)));
            if !self.chain_valid(child, row) {
                if self.frozen {
                    return internal("a dereference join is created after its place was fixed");
                }
                let key = match cur {
                    Cur::Base => self.expr(base, row)?,
                    Cur::Chain(n) => self.chain_value(n)?,
                };
                let area = match cur {
                    Cur::Base => match &base.kind {
                        crate::ir::ExprKind::Col { src, .. } => self.area_of(*src)?,
                        _ => Vec::new(),
                    },
                    Cur::Chain(n) => self.chain_area(n)?,
                };
                let chain = self.step_joins(
                    row,
                    &key,
                    &area,
                    step,
                    base,
                    steps
                        .get(..i)
                        .ok_or_else(|| crate::ExecError::Fatal("dereference prefix".into()))?,
                )?;
                let old = self.chains.insert(child, chain);
                if let Some(log) = &mut self.chain_log {
                    log.push((child, old));
                }
            }
            node = child;
            cur = Cur::Chain(child);
        }
        match cur {
            Cur::Base => self.expr(base, row),
            Cur::Chain(n) => self.chain_value(n),
        }
    }
}
