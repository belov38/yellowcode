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
    fn empty_ref_expr(&mut self, atom: crate::types::RefAtom) -> R<Expr> {
        let empty_table = self.schema.ref_info(atom).table;
        let slot = u16::try_from(self.values.len())
            .map_err(|_| BuildError::Internal("too many values".into()))?;
        self.values.push(ValuePath::EmptyRef(empty_table));
        Ok(Expr {
            kind: ExprKind::Value(slot),
            ty: QType::reference(atom),
        })
    }
    fn addressing_main_empty(&mut self, main_expr: Expr) -> R<Expr> {
        let ty = main_expr.ty.clone();
        let atoms = ty.atom_count();
        let mut empties = Vec::new();
        if atoms != 1 {
            empties.push(Expr {
                kind: ExprKind::Lit(QValue::Undefined),
                ty: QType::undefined(),
            });
        }
        if ty.boolean {
            empties.push(Expr {
                kind: ExprKind::Lit(QValue::Bool(false)),
                ty: QType::boolean(),
            });
        }
        if let Some(q) = ty.number {
            empties.push(Expr {
                kind: ExprKind::Lit(QValue::Number(bsl_prim::Number::zero())),
                ty: QType::num(q),
            });
        }
        if let Some(q) = ty.string {
            empties.push(Expr {
                kind: ExprKind::Lit(QValue::String(crate::QStr::from_str(""))),
                ty: QType::str_(q),
            });
        }
        if let Some(q) = ty.date {
            empties.push(Expr {
                kind: ExprKind::Lit(QValue::Date(bsl_prim::BslDate::EMPTY)),
                ty: QType::date(q),
            });
        }
        for &atom in &ty.refs {
            empties.push(self.empty_ref_expr(atom)?);
        }
        for &other in &ty.others {
            match other {
                crate::types::Other::Uuid => empties.push(Expr {
                    kind: ExprKind::Lit(QValue::Uuid([0; 16])),
                    ty: QType::other(crate::types::Other::Uuid),
                }),
                _ => return later("Q7d-AddressingMainType"),
            }
        }
        let mut iter = empties.into_iter();
        let Some(first) = iter.next() else {
            return internal("addressing main empty value");
        };
        if iter.len() == 0 {
            Ok(equal(main_expr, first))
        } else {
            let mut rhs = Vec::with_capacity(1 + iter.len());
            rhs.push(first);
            rhs.extend(iter);
            Ok(boolean(ExprKind::InList {
                l: vec![main_expr],
                not: false,
                hierarchy: false,
                rhs,
            }))
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
        let main_empty = self.addressing_main_empty(main_expr.clone())?;
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
            equal(main_expr, performer),
            boolean(ExprKind::And(vec![main_empty, exists])),
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
        let value_ty = value.ty.clone();
        let (argument_src, argument_rows) = self.qr8_subquery(
            Rel::Project {
                input: Box::new(Rel::Empty),
                exprs: vec![value],
            },
            vec![value_ty.clone()],
        )?;
        let value = column(argument_src, 0, value_ty);
        let not_undefined = boolean(ExprKind::Compare(
            CmpOp::Ne,
            Box::new(value.clone()),
            Box::new(Expr {
                kind: ExprKind::Lit(QValue::Undefined),
                ty: QType::undefined(),
            }),
        ));
        let argument_rows = filter(argument_rows, not_undefined);
        let (members, rows) = self.qr8_subquery(
            Rel::Union { parts, all },
            vec![reference_ty.clone(), member_ty.clone()],
        )?;
        let rows = Rel::Join {
            kind: query_syntax::ast::JoinKind::Inner,
            l: Box::new(rows),
            r: Box::new(argument_rows),
            on: equal(column(members, 1, member_ty), value),
        };
        Ok(Rel::Subquery {
            src,
            input: Box::new(Rel::Distinct(Box::new(Rel::Project {
                input: Box::new(rows),
                exprs: vec![column(members, 0, reference_ty)],
            }))),
        })
    }
}
