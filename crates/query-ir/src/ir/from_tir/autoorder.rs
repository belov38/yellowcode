use super::{Builder, R, internal, later};
use crate::ir::{ColSel, Expr, ExprKind, SrcId, SrcKind};
use crate::schema::{FieldRole, QFieldId, QTableId, StdRole, TableKind};
use crate::tir;
use crate::types::{QType, RefAtom};
use crate::value::QValue;
use mdschema::MdKind;

#[derive(Clone, Copy)]
enum AtomKey {
    Field(QFieldId),
    NotField(QFieldId),
    SelfRef,
}

pub(super) struct MainOrder {
    pub keys: Vec<Expr>,
    pub hier: Option<(Expr, Expr)>,
}

fn num(n: i64) -> Expr {
    Expr {
        kind: ExprKind::Lit(QValue::Number(bsl_prim::Number::from_scaled_i64(n, 0))),
        ty: QType::default(),
    }
}

fn null() -> Expr {
    Expr {
        kind: ExprKind::Lit(QValue::Null),
        ty: QType::default(),
    }
}

pub(super) fn last_deref_tables(e: &Expr) -> Vec<QTableId> {
    match &e.kind {
        ExprKind::Deref { steps, .. } => steps
            .last()
            .map(|s| s.targets.iter().map(|(t, _)| *t).collect())
            .unwrap_or_default(),
        ExprKind::Cast { x, .. } => last_deref_tables(x),
        _ => Vec::new(),
    }
}

impl Builder<'_> {
    pub(super) fn role_field(&self, t: QTableId, role: StdRole) -> Option<QFieldId> {
        self.schema
            .table(t)
            .fields
            .iter()
            .position(|f| f.role == FieldRole::Std(role))
            .and_then(|i| u16::try_from(i).ok())
            .map(QFieldId)
    }

    fn atom_keys(&self, a: RefAtom) -> R<Vec<AtomKey>> {
        let info = self.schema.ref_info(a);
        let t = info.table;
        let need = |role: StdRole| match self.role_field(t, role) {
            Some(f) => Ok(f),
            None => later(
                "АВТОУПОРЯДОЧИВАНИЕ ссылки на объект без стандартного поля",
                "Q3",
            ),
        };
        match info.kind {
            MdKind::Catalog => {
                let mut v = Vec::new();
                if let Some(f) = self.role_field(t, StdRole::IsFolder) {
                    v.push(AtomKey::NotField(f));
                }
                v.push(AtomKey::Field(need(StdRole::Description)?));
                Ok(v)
            }
            MdKind::Document => Ok(vec![AtomKey::Field(need(StdRole::Date)?), AtomKey::SelfRef]),
            MdKind::Enum => Ok(vec![AtomKey::Field(need(StdRole::Order)?)]),
            _ => later("АВТОУПОРЯДОЧИВАНИЕ ссылки на объект этого вида", "Q3"),
        }
    }

    fn deref_field(&mut self, e: &Expr, a: RefAtom, f: QFieldId) -> Expr {
        let t = self.schema.ref_info(a).table;
        self.touch(t);
        Expr {
            kind: ExprKind::Deref {
                base: Box::new(e.clone()),
                steps: vec![tir::Deref {
                    name: format!("\u{0}auto:{}.{}", t.0, f.0),
                    targets: vec![(t, f)],
                }],
            },
            ty: QType::default(),
        }
    }

    fn atom_key_expr(&mut self, e: &Expr, a: RefAtom, k: AtomKey) -> Expr {
        match k {
            AtomKey::Field(f) => self.deref_field(e, a, f),
            AtomKey::NotField(f) => {
                let d = self.deref_field(e, a, f);
                Expr {
                    kind: ExprKind::Not(Box::new(d)),
                    ty: QType::boolean(),
                }
            }
            AtomKey::SelfRef => e.clone(),
        }
    }

    pub(super) fn auto_keys(&mut self, e: &Expr) -> R<Vec<Expr>> {
        let atoms = e.ty.refs.clone();
        if atoms.is_empty() {
            return Ok(vec![e.clone()]);
        }
        let mixed = atoms.len() > 1
            || e.ty.boolean
            || e.ty.number.is_some()
            || e.ty.string.is_some()
            || e.ty.date.is_some()
            || !e.ty.others.is_empty();
        let mut per_atom = Vec::with_capacity(atoms.len());
        for a in &atoms {
            per_atom.push(self.atom_keys(*a)?);
        }
        let width = per_atom.iter().map(Vec::len).max().unwrap_or(0);
        let mut out = Vec::with_capacity(width + 1);
        if mixed {
            let mut whens = Vec::new();
            for a in &atoms {
                let t = self.schema.ref_info(*a).table;
                let Some(rf) = self.role_field(t, StdRole::Ref) else {
                    return internal("a reference table without a reference field");
                };
                let d = self.deref_field(e, *a, rf);
                whens.push((
                    Expr {
                        kind: ExprKind::And(vec![
                            self.refs_of(e, *a),
                            Expr {
                                kind: ExprKind::IsNull {
                                    l: Box::new(d),
                                    not: false,
                                },
                                ty: QType::boolean(),
                            },
                        ]),
                        ty: QType::boolean(),
                    },
                    num(1),
                ));
            }
            whens.push((
                Expr {
                    kind: ExprKind::IsNull {
                        l: Box::new(e.clone()),
                        not: false,
                    },
                    ty: QType::boolean(),
                },
                num(0),
            ));
            out.push(Expr {
                kind: ExprKind::Case {
                    operand: None,
                    whens,
                    else_: Some(Box::new(num(2))),
                },
                ty: QType::default(),
            });
        }
        for j in 0..width {
            let mut whens = Vec::new();
            for (a, keys) in atoms.iter().zip(&per_atom) {
                if let Some(k) = keys.get(j) {
                    let x = self.atom_key_expr(e, *a, *k);
                    whens.push((self.refs_of(e, *a), x));
                }
            }
            if !mixed && atoms.len() == 1 {
                if let Some((_, x)) = whens.pop() {
                    out.push(x);
                }
                continue;
            }
            let else_ = if mixed && j == 0 { e.clone() } else { null() };
            out.push(Expr {
                kind: ExprKind::Case {
                    operand: None,
                    whens,
                    else_: Some(Box::new(else_)),
                },
                ty: QType::default(),
            });
        }
        Ok(out)
    }

    fn refs_of(&mut self, e: &Expr, a: RefAtom) -> Expr {
        let t = self.schema.ref_info(a).table;
        self.touch(t);
        Expr {
            kind: ExprKind::Refs {
                l: Box::new(e.clone()),
                table: t,
            },
            ty: QType::boolean(),
        }
    }

    fn col_of(&self, src: SrcId, t: QTableId, f: QFieldId) -> Expr {
        Expr {
            kind: ExprKind::Col {
                src,
                col: ColSel::Field(f),
            },
            ty: self
                .schema
                .field(t, f)
                .map(|x| x.ty.clone())
                .unwrap_or_default(),
        }
    }

    pub(super) fn hierarchy_of(&self, src: SrcId) -> Option<(Expr, Expr)> {
        let Some(SrcKind::Table(t)) = self.sources.get(src.0 as usize).map(|s| s.kind.clone())
        else {
            return None;
        };
        let id = self.role_field(t, StdRole::Ref)?;
        let parent = self.role_field(t, StdRole::Parent)?;
        Some((self.col_of(src, t, id), self.col_of(src, t, parent)))
    }

    pub(super) fn main_order(&mut self, src: SrcId) -> R<Option<MainOrder>> {
        let Some(SrcKind::Table(t)) = self.sources.get(src.0 as usize).map(|s| s.kind.clone())
        else {
            return Ok(None);
        };
        let kind = match self.schema.table(t).kind {
            TableKind::Object(k) => k,
            _ => return later("АВТОУПОРЯДОЧИВАНИЕ таблицы этого вида", "Q3"),
        };
        let role = |this: &Self, r: StdRole| this.role_field(t, r).map(|f| this.col_of(src, t, f));
        let keys = match kind {
            MdKind::Catalog | MdKind::Document | MdKind::Enum => {
                role(self, StdRole::Ref).into_iter().collect()
            }
            MdKind::AccumulationRegister => {
                [StdRole::Period, StdRole::Recorder, StdRole::LineNumber]
                    .into_iter()
                    .filter_map(|r| role(self, r))
                    .collect()
            }
            MdKind::InformationRegister => {
                let mut v: Vec<Expr> = role(self, StdRole::Period).into_iter().collect();
                let dims: Vec<QFieldId> = self
                    .schema
                    .table(t)
                    .fields
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| f.role == FieldRole::Dimension)
                    .filter_map(|(i, _)| u16::try_from(i).ok().map(QFieldId))
                    .collect();
                v.extend(dims.into_iter().map(|f| self.col_of(src, t, f)));
                v
            }
            _ => return later("АВТОУПОРЯДОЧИВАНИЕ таблицы этого вида", "Q3"),
        };
        let hier = if kind == MdKind::Catalog {
            self.hierarchy_of(src)
        } else {
            None
        };
        Ok(Some(MainOrder { keys, hier }))
    }
}

pub(super) fn first_src(e: &Expr) -> Option<SrcId> {
    match &e.kind {
        ExprKind::Col { src, .. } => Some(*src),
        ExprKind::Deref { .. } => None,
        ExprKind::Cast { x, .. } => first_src(x),
        _ => None,
    }
}
