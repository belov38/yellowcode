use super::*;
use query_syntax::ast::CmpOp;

fn later<T>(what: &'static str) -> R<T> {
    Err(BuildError::Later { what, stage: "Q7d" })
}
fn boolean(kind: ExprKind) -> Expr {
    Expr {
        kind,
        ty: QType::boolean(),
    }
}
fn equal(l: Expr, r: Expr) -> Expr {
    boolean(ExprKind::Compare(CmpOp::Eq, Box::new(l), Box::new(r)))
}
fn filter(input: Rel, pred: Expr) -> Rel {
    Rel::Filter {
        input: Box::new(input),
        pred,
    }
}
fn column(src: SrcId, index: u16, ty: QType) -> Expr {
    Expr {
        kind: ExprKind::Col {
            src,
            col: ColSel::Column(index),
        },
        ty,
    }
}

impl<'a> Builder<'a> {
    fn qr8_subquery(&mut self, input: Rel, columns: Vec<QType>) -> R<(SrcId, Rel)> {
        let id = SrcId(
            u32::try_from(self.sources.len())
                .map_err(|_| BuildError::Internal("too many QR8 sources".into()))?,
        );
        self.sources.push(SourceS {
            id,
            all_fields: false,
            kind: SrcKind::Subquery { columns },
        });
        Ok((
            id,
            Rel::Subquery {
                src: id,
                input: Box::new(input),
            },
        ))
    }
    fn qr8_col(&self, src: SrcId, table: QTableId, field: QFieldId) -> R<Expr> {
        let Some(f) = self.schema.field(table, field) else {
            return internal("QR8 field identity");
        };
        Ok(Expr {
            kind: ExprKind::Col {
                src,
                col: ColSel::Field(field),
            },
            ty: f.ty.clone(),
        })
    }
    fn qr8_scan(&mut self, table: QTableId) -> R<(SrcId, Rel)> {
        let id = SrcId(
            u32::try_from(self.sources.len())
                .map_err(|_| BuildError::Internal("too many QR8 sources".into()))?,
        );
        self.sources.push(SourceS {
            id,
            all_fields: false,
            kind: SrcKind::Table(table),
        });
        self.touch(table);
        Ok((id, Rel::Scan { src: id, table }))
    }
    pub(super) fn qr8_source(
        &mut self,
        src: SrcId,
        table: QTableId,
        args: &'a [Option<TExpr>],
    ) -> R<Rel> {
        match self.schema.table(table).kind {
            TableKind::Virtual(VtKind::TaskByPerformer) => self.task_performer(src, table, args),
            TableKind::Virtual(VtKind::FilterCriterion) => self.criterion(src, table, args),
            _ => internal("not a QR8 source"),
        }
    }
    fn task_performer(&mut self, src: SrcId, table: QTableId, args: &'a [Option<TExpr>]) -> R<Rel> {
        let Some(owner) = self.schema.table(table).owner else {
            return internal("task owner");
        };
        let Some(meta) = self.schema.meta(owner).and_then(|m| m.addressing.clone()) else {
            return later("Q7d-AddressingMetadata");
        };
        if let Some(s) = self.sources.get_mut(src.0 as usize) {
            s.kind = SrcKind::Table(table);
        }
        self.touch(table);
        let map_field = |f: QFieldId| -> R<QFieldId> {
            let original = self
                .schema
                .field(owner, f)
                .ok_or_else(|| BuildError::Internal("task field".into()))?;
            self.schema
                .table(table)
                .fields
                .iter()
                .enumerate()
                .find_map(|(i, c)| {
                    (c.md == original.md && c.role == original.role)
                        .then(|| u16::try_from(i).ok().map(QFieldId))
                        .flatten()
                })
                .ok_or_else(|| BuildError::Internal("task VT field".into()))
        };
        let main = map_field(meta.main)?;
        let mapped = meta
            .fields
            .iter()
            .map(|(t, r)| Ok((map_field(*t)?, *r)))
            .collect::<R<Vec<_>>>()?;
        let performer = match args.first().and_then(Option::as_ref) {
            Some(e) => self.expr(e)?,
            None => {
                let slot = self.param(&format!("\0QR8:{}", src.0))?;
                self.qr8.push(super::super::Qr8Param::Session {
                    slot,
                    name: meta.session,
                });
                Expr {
                    kind: ExprKind::Param(slot),
                    ty: self
                        .schema
                        .field(table, main)
                        .map(|f| f.ty.clone())
                        .unwrap_or_default(),
                }
            }
        };
        let performer_ty = performer.ty.clone();
        let (argument_src, argument_rows) = self.qr8_subquery(
            Rel::Project {
                input: Box::new(Rel::Empty),
                exprs: vec![performer],
            },
            vec![performer_ty.clone()],
        )?;
        let performer = column(argument_src, 0, performer_ty);
        let main_expr = self.qr8_col(src, table, main)?;
        let Some(atom) = main_expr.ty.refs.first() else {
            return later("Q7d-AddressingMainType");
        };
        if main_expr.ty.refs.len() != 1 {
            return later("Q7d-AddressingMainType");
        }
        let empty_table = self.schema.ref_info(*atom).table;
        let slot = u16::try_from(self.values.len())
            .map_err(|_| BuildError::Internal("too many values".into()))?;
        self.values.push(ValuePath::EmptyRef(empty_table));
        let empty = Expr {
            kind: ExprKind::Value(slot),
            ty: main_expr.ty.clone(),
        };
        let (address_src, address_rows) = self.qr8_scan(meta.register)?;
        let mut conditions = Vec::new();
        for (task, reg) in mapped {
            let left = self.qr8_col(address_src, meta.register, reg)?;
            let right = if task == main {
                performer.clone()
            } else {
                self.qr8_col(src, table, task)?
            };
            conditions.push(equal(left, right));
        }
        let exists = boolean(ExprKind::InQuery {
            l: vec![Expr {
                kind: ExprKind::Lit(QValue::Bool(true)),
                ty: QType::boolean(),
            }],
            not: false,
            hierarchy: false,
            q: Box::new(Rel::Project {
                input: Box::new(filter(address_rows, boolean(ExprKind::And(conditions)))),
                exprs: vec![Expr {
                    kind: ExprKind::Lit(QValue::Bool(true)),
                    ty: QType::boolean(),
                }],
            }),
        });
        let pred = boolean(ExprKind::Or(vec![
            equal(main_expr.clone(), performer),
            boolean(ExprKind::And(vec![equal(main_expr, empty), exists])),
        ]));
        let rows = Rel::Join {
            kind: query_syntax::ast::JoinKind::Inner,
            l: Box::new(Rel::Scan { src, table }),
            r: Box::new(argument_rows),
            on: boolean(ExprKind::Lit(QValue::Bool(true))),
        };
        let mut rel = filter(rows, pred);
        if let Some(Some(cond)) = args.get(1) {
            self.levels.push(vec![src]);
            let pred = self.expr(cond);
            self.levels.pop();
            rel = filter(rel, pred?);
        }
        Ok(rel)
    }
    fn criterion(&mut self, src: SrcId, table: QTableId, args: &'a [Option<TExpr>]) -> R<Rel> {
        let Some(meta) = self.schema.meta(table).and_then(|m| m.criterion.clone()) else {
            return later("Q7d-CriterionMetadata");
        };
        let Some(Some(arg)) = args.first() else {
            return later("Q7d-CriterionParameter");
        };
        let value = self.expr(arg)?;
        if let ExprKind::Param(slot) = value.kind {
            self.qr8
                .push(super::super::Qr8Param::Criterion { slot, table });
        }
        let fields = self.schema.table(table).fields.len();
        if let Some(s) = self.sources.get_mut(src.0 as usize) {
            s.kind = SrcKind::Virtual {
                table,
                cols: (0..fields)
                    .map(|i| u16::try_from(i).ok().map(QFieldId))
                    .collect(),
            };
        }
        let mut parts = Vec::new();
        let mut reference_ty = QType::default();
        let mut member_ty = QType::default();
        for (member, field) in meta.members {
            let Some(reference) = self
                .schema
                .table(member)
                .fields
                .iter()
                .enumerate()
                .find_map(|(i, f)| {
                    (f.role == FieldRole::Std(StdRole::Ref))
                        .then(|| u16::try_from(i).ok().map(QFieldId))
                        .flatten()
                })
            else {
                return later("Q7d-CriterionMemberRef");
            };
            let (id, scan) = self.qr8_scan(member)?;
            let reference = self.qr8_col(id, member, reference)?;
            let member_value = self.qr8_col(id, member, field)?;
            reference_ty = reference_ty.merge(&reference.ty);
            member_ty = member_ty.merge(&member_value.ty);
            parts.push(Rel::Project {
                input: Box::new(scan),
                exprs: vec![reference, member_value],
            });
        }
        if parts.is_empty() {
            return later("Q7d-EmptyCriterionContent");
        }
        let all = vec![true; parts.len().saturating_sub(1)];
        let (members, rows) = self.qr8_subquery(
            Rel::Union { parts, all },
            vec![reference_ty.clone(), member_ty.clone()],
        )?;
        let rows = filter(rows, equal(column(members, 1, member_ty), value));
        Ok(Rel::Subquery {
            src,
            input: Box::new(Rel::Distinct(Box::new(Rel::Project {
                input: Box::new(rows),
                exprs: vec![column(members, 0, reference_ty)],
            }))),
        })
    }
}
