use crate::ColumnInfo;
use crate::ir::{BuildError, ColSel, Expr, ExprKind, Rel, SortKey, SourceS, SrcId, SrcKind, StmtS};
use crate::schema::{
    FieldRole, Periodicity, QFieldId, QTableId, QuerySchema, StdRole, TableKind, VtKind,
};
use crate::types::{DateQ, QType, StrQ};
use crate::value::{QStr, QValue};
use bsl_prim::fold_name;
use query_syntax::ast::CmpOp;

#[derive(Clone, Debug, PartialEq)]
pub enum BuildValue {
    Value(QValue),
    Param(String),
}

pub const LIKE_ESCAPE: char = '\\';

pub fn like_prefix_pattern(prefix: &str) -> String {
    let mut out = String::with_capacity(prefix.len() + 1);
    for c in prefix.chars() {
        if matches!(c, '%' | '_' | '[' | LIKE_ESCAPE) {
            out.push(LIKE_ESCAPE);
        }
        out.push(c);
    }
    out.push('%');
    out
}

#[derive(Clone, Debug)]
enum Filter {
    Eq(QFieldId, BuildValue),
    Range(QFieldId, Option<BuildValue>, Option<BuildValue>),
    Starts(QFieldId, BuildValue),
}

#[derive(Clone, Debug)]
struct FieldSnap {
    name: String,
    ty: QType,
    role: FieldRole,
}

#[derive(Clone, Debug)]
struct RegisterSelection {
    register: QTableId,
    kind: VtKind,
    inner_cols: Vec<Option<QFieldId>>,
}

#[derive(Clone, Debug)]
pub struct Selection {
    table: QTableId,
    state: Result<(), String>,
    unsupported: Option<(&'static str, &'static str)>,
    register: Option<RegisterSelection>,
    bounds: [Option<BuildValue>; 2],
    fields: Vec<FieldSnap>,
    star: Vec<QFieldId>,
    all_fields: bool,
    filters: Vec<Filter>,
    order: Vec<(QFieldId, bool)>,
    top: Option<u64>,
    hierarchical: bool,
}

impl Selection {
    pub fn of(schema: &QuerySchema, t: QTableId) -> Selection {
        let known = usize::try_from(t.0).is_ok_and(|i| i < schema.tables().len());
        let tab = schema.table(t);
        let register = tab.owner.and_then(|register| {
            let TableKind::Virtual(kind) = tab.kind else {
                return None;
            };
            let supported = matches!(
                (schema.table(register).kind, kind),
                (
                    TableKind::Object(mdschema::MdKind::AccumulationRegister),
                    VtKind::Balance | VtKind::Turnovers
                ) | (
                    TableKind::Object(mdschema::MdKind::InformationRegister),
                    VtKind::SliceLast | VtKind::SliceFirst
                )
            );
            if !supported {
                return None;
            }
            let inner_cols = if matches!(kind, VtKind::SliceLast | VtKind::SliceFirst) {
                (0..tab.fields.len())
                    .map(|i| u16::try_from(i).ok().map(QFieldId))
                    .collect()
            } else {
                let shape = crate::lower::vt_shape(schema, t);
                let mut cols =
                    vec![
                        None;
                        crate::ir::raw_layout::width(shape.dims.len(), shape.resources.len())
                    ];
                for (slot, f) in cols
                    .iter_mut()
                    .skip(crate::ir::raw_layout::DIMS)
                    .zip(shape.dims)
                {
                    *slot = Some(f);
                }
                cols
            };
            Some(RegisterSelection {
                register,
                kind,
                inner_cols,
            })
        });
        let unsupported = match tab.kind {
            TableKind::Virtual(VtKind::SequenceBoundaries) => None,
            TableKind::Virtual(_) if register.is_none() => Some(("виртуальные таблицы", "Q5")),
            _ => None,
        };
        Selection {
            table: t,
            state: if known {
                Ok(())
            } else {
                Err(format!("unknown table {}", t.0))
            },
            unsupported,
            register,
            bounds: [None, None],
            fields: tab
                .fields
                .iter()
                .map(|f| FieldSnap {
                    name: f.name_ru.clone(),
                    ty: f.ty.clone(),
                    role: f.role,
                })
                .collect(),
            star: tab
                .star
                .iter()
                .copied()
                .filter(|f| {
                    tab.fields.get(usize::from(f.0)).is_some_and(|x| {
                        x.nested.is_none() && x.role != FieldRole::Std(StdRole::PredefinedDataName)
                    })
                })
                .collect(),
            all_fields: true,
            filters: Vec::new(),
            order: Vec::new(),
            top: None,
            hierarchical: false,
        }
    }

    pub fn columns(mut self, fields: &[QFieldId]) -> Selection {
        self.star = fields.to_vec();
        self.all_fields = false;
        self
    }

    pub fn eq(mut self, f: QFieldId, v: BuildValue) -> Selection {
        self.filters.push(Filter::Eq(f, v));
        self
    }

    pub fn range(
        mut self,
        f: QFieldId,
        lo: Option<BuildValue>,
        hi: Option<BuildValue>,
    ) -> Selection {
        self.filters.push(Filter::Range(f, lo, hi));
        self
    }

    pub fn starts_with(mut self, f: QFieldId, prefix: BuildValue) -> Selection {
        self.filters.push(Filter::Starts(f, prefix));
        self
    }

    pub fn virtual_bounds(
        mut self,
        start: Option<BuildValue>,
        end: Option<BuildValue>,
    ) -> Selection {
        self.bounds = [start, end];
        self
    }

    pub fn order(mut self, keys: &[(QFieldId, bool)]) -> Selection {
        self.order = keys.to_vec();
        self
    }

    pub fn hierarchical(mut self) -> Selection {
        self.hierarchical = true;
        self
    }

    pub fn top(mut self, n: u64) -> Selection {
        self.top = Some(n);
        self
    }

    pub fn build(self) -> Result<StmtS, BuildError> {
        Builder::new(&self)?.run()
    }
}

type R<T> = Result<T, BuildError>;

fn internal<T>(s: impl Into<String>) -> R<T> {
    Err(BuildError::Internal(s.into()))
}

struct Builder<'a> {
    sel: &'a Selection,
    params: Vec<String>,
    slots: std::collections::HashMap<String, u16>,
    source: SrcId,
}

const SRC: SrcId = SrcId(0);

impl<'a> Builder<'a> {
    fn new(sel: &'a Selection) -> R<Builder<'a>> {
        if let Err(e) = &sel.state {
            return internal(e.clone());
        }
        if let Some(u) = sel.unsupported {
            return Err(BuildError::Later {
                what: u.0,
                stage: u.1,
            });
        }
        if sel.bounds.iter().any(Option::is_some) && sel.register.is_none() {
            return internal("virtual bounds need a manager virtual table");
        }
        if sel.bounds[1].is_some()
            && sel
                .register
                .as_ref()
                .is_some_and(|v| v.kind != VtKind::Turnovers)
        {
            return internal("only turnovers accept an end boundary");
        }
        Ok(Builder {
            sel,
            params: Vec::new(),
            slots: Default::default(),
            source: SRC,
        })
    }

    fn field(&self, f: QFieldId) -> R<&'a FieldSnap> {
        match self.sel.fields.get(usize::from(f.0)) {
            Some(x) => Ok(x),
            None => internal(format!(
                "unknown field {} of table {}",
                f.0, self.sel.table.0
            )),
        }
    }

    fn col(&self, f: QFieldId) -> R<Expr> {
        if self.source != SRC
            && self
                .sel
                .register
                .as_ref()
                .is_some_and(|v| !v.inner_cols.contains(&Some(f)))
        {
            return internal("a manager filter field is unavailable before the register operation");
        }
        Ok(Expr {
            kind: ExprKind::Col {
                src: self.source,
                col: ColSel::Field(f),
            },
            ty: self.field(f)?.ty.clone(),
        })
    }

    fn value(&mut self, v: &BuildValue, ty: &QType) -> R<Expr> {
        let kind = match v {
            BuildValue::Value(x) => ExprKind::Lit(x.clone()),
            BuildValue::Param(name) => {
                let key = fold_name(name);
                let slot = match self.slots.get(&key) {
                    Some(i) => *i,
                    None => {
                        let Ok(i) = u16::try_from(self.params.len()) else {
                            return internal("too many parameters");
                        };
                        self.params.push(name.clone());
                        self.slots.insert(key, i);
                        i
                    }
                };
                ExprKind::Param(slot)
            }
        };
        Ok(Expr {
            kind,
            ty: ty.clone(),
        })
    }

    fn filter(&mut self, f: &Filter) -> R<Expr> {
        let boolean = QType::boolean();
        let mk = |kind| Expr {
            kind,
            ty: boolean.clone(),
        };
        match f {
            Filter::Eq(fld, v) => {
                let l = self.col(*fld)?;
                let r = self.value(v, &l.ty)?;
                Ok(mk(ExprKind::Compare(CmpOp::Eq, Box::new(l), Box::new(r))))
            }
            Filter::Range(fld, lo, hi) => {
                let l = self.col(*fld)?;
                match (lo, hi) {
                    (Some(lo), Some(hi)) => {
                        let (lo, hi) = (self.value(lo, &l.ty)?, self.value(hi, &l.ty)?);
                        Ok(mk(ExprKind::Between {
                            l: Box::new(l),
                            not: false,
                            lo: Box::new(lo),
                            hi: Box::new(hi),
                        }))
                    }
                    (Some(lo), None) => {
                        let lo = self.value(lo, &l.ty)?;
                        Ok(mk(ExprKind::Compare(CmpOp::Ge, Box::new(l), Box::new(lo))))
                    }
                    (None, Some(hi)) => {
                        let hi = self.value(hi, &l.ty)?;
                        Ok(mk(ExprKind::Compare(CmpOp::Le, Box::new(l), Box::new(hi))))
                    }
                    (None, None) => internal("a range without ends"),
                }
            }
            Filter::Starts(fld, v) => {
                let l = self.col(*fld)?;
                if !l.ty.has_string() {
                    return internal(format!(
                        "starts_with over the field {} that is not a string",
                        self.field(*fld)?.name
                    ));
                }
                let pat_ty = QType::str_(StrQ {
                    len: 0,
                    fixed: false,
                });
                let pat = match v {
                    BuildValue::Value(QValue::String(s)) => {
                        let text = String::from_utf16_lossy(&s.0);
                        Expr {
                            kind: ExprKind::Lit(QValue::String(QStr::from_str(
                                &like_prefix_pattern(&text),
                            ))),
                            ty: pat_ty,
                        }
                    }
                    BuildValue::Value(_) => return internal("starts_with needs a string prefix"),
                    p @ BuildValue::Param(_) => self.value(p, &pat_ty)?,
                };
                Ok(mk(ExprKind::Like {
                    l: Box::new(l),
                    not: false,
                    pat: Box::new(pat),
                    esc: Some(LIKE_ESCAPE),
                }))
            }
        }
    }

    fn run(mut self) -> R<StmtS> {
        let sel = self.sel;
        let mut rel = Rel::Scan {
            src: SRC,
            table: sel.table,
        };
        let mut sources = vec![SourceS {
            id: SRC,
            all_fields: sel.all_fields,
            kind: SrcKind::Table(sel.table),
        }];
        let mut tables = vec![sel.table];
        if sel.register.is_some() {
            self.source = SrcId(1);
        }
        let mut preds = Vec::with_capacity(sel.filters.len());
        for f in &sel.filters {
            preds.push(self.filter(f)?);
        }
        let pred = match preds.len() {
            0 => None,
            1 => preds.pop(),
            _ => Some(Expr {
                kind: ExprKind::And(preds),
                ty: QType::boolean(),
            }),
        };
        self.source = SRC;
        if let Some(v) = &sel.register {
            if let Some(source) = sources.first_mut() {
                source.kind = SrcKind::Virtual {
                    table: sel.table,
                    cols: (0..sel.fields.len())
                        .map(|i| u16::try_from(i).ok().map(QFieldId))
                        .collect(),
                };
            }
            sources.push(SourceS {
                id: SrcId(1),
                all_fields: false,
                kind: SrcKind::Virtual {
                    table: sel.table,
                    cols: v.inner_cols.clone(),
                },
            });
            tables = vec![v.register];
            let ty = QType::date(DateQ::DateTime);
            let bounds = [
                sel.bounds[0]
                    .as_ref()
                    .map(|b| self.value(b, &ty))
                    .transpose()?,
                sel.bounds[1]
                    .as_ref()
                    .map(|b| self.value(b, &ty))
                    .transpose()?,
            ];
            let step = Box::new(crate::ir::RegisterVtS {
                src: SRC,
                inner: SrcId(1),
                table: sel.table,
                register: v.register,
                kind: v.kind,
                bounds,
                cond: pred,
                periodicity: Periodicity::Absent,
                complement: None,
                area: None,
                access: None,
                span: None,
            });
            rel = if matches!(v.kind, VtKind::SliceLast | VtKind::SliceFirst) {
                Rel::Slice(step)
            } else {
                Rel::RegisterVt(step)
            };
        } else if let Some(pred) = pred {
            rel = Rel::Filter {
                input: Box::new(rel),
                pred,
            };
        }
        let mut cols: Vec<QFieldId> = sel.star.clone();
        let mut columns = Vec::with_capacity(cols.len());
        for f in &cols {
            let snap = self.field(*f)?;
            let ty = snap.ty.clone().with_null();
            columns.push(ColumnInfo {
                name: snap.name.clone(),
                width: ty.width(),
                ty,
                nested: None,
            });
        }
        let out_col = |f: QFieldId, cols: &mut Vec<QFieldId>| -> R<u16> {
            let i = match cols.iter().position(|c| *c == f) {
                Some(i) => i,
                None => {
                    cols.push(f);
                    cols.len() - 1
                }
            };
            match u16::try_from(i) {
                Ok(i) => Ok(i),
                Err(_) => internal("too many columns"),
            }
        };
        let mut keys = Vec::with_capacity(sel.order.len());
        for (f, desc) in &sel.order {
            let ty = self.field(*f)?.ty.clone();
            let i = out_col(*f, &mut cols)?;
            keys.push(SortKey {
                expr: Expr {
                    kind: ExprKind::Out(i),
                    ty,
                },
                desc: *desc,
            });
        }
        let hier = if sel.hierarchical {
            let find = |role| {
                sel.fields
                    .iter()
                    .position(|f| f.role == FieldRole::Std(role))
                    .and_then(|i| u16::try_from(i).ok())
                    .map(QFieldId)
            };
            let (Some(id), Some(parent)) = (find(StdRole::Ref), find(StdRole::Parent)) else {
                return internal("a hierarchical selection over a table without a parent");
            };
            Some((out_col(id, &mut cols)?, out_col(parent, &mut cols)?))
        } else {
            None
        };
        let mut exprs = Vec::with_capacity(cols.len());
        for f in &cols {
            exprs.push(self.col(*f)?);
        }
        rel = Rel::Project {
            input: Box::new(rel),
            exprs,
        };
        rel = match hier {
            Some((id, parent)) => Rel::Hierarchy {
                input: Box::new(rel),
                id,
                parent,
                keys,
            },
            None if !keys.is_empty() => Rel::Sort {
                input: Box::new(rel),
                keys,
            },
            None => rel,
        };
        if let Some(n) = sel.top {
            rel = Rel::Top {
                input: Box::new(rel),
                n,
            };
        }
        Ok(StmtS {
            qr8: Vec::new(),
            static_analysis: false,
            root: rel,
            columns,
            sources,
            params: self.params,
            values: Vec::new(),
            tables,
            allowed: false,
            auto_number: None,
            nested: Vec::new(),
            present: Vec::new(),
            totals: None,
            max_depth: 8,
        })
    }
}
