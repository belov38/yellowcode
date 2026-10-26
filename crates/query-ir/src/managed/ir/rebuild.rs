use super::{BuildCtx, BuildProblem, BuildResult, hash};
use crate::ir::{self, Expr, ExprKind as E, Rel};
use crate::managed::{Index, NativeOwned, ScratchVec};
use std::hash::Hasher;

pub(super) struct Rebuild<'c, 'a, 'r, 'w, 'k> {
    pub(super) ctx: &'c BuildCtx<'a, 'r, 'w>,
    pub(super) keys: &'k ScratchVec<NativeOwned<'a, Expr>>,
    pub(super) index: &'k Index<u64, ScratchVec<u16>>,
    pub(super) own: &'k [ir::SrcId],
    pub(super) anchor: Option<ir::SrcId>,
    pub(super) in_sub: bool,
    pub(super) replace: bool,
}
impl<'a> Rebuild<'_, 'a, '_, '_, '_> {
    pub(super) fn find(&self, e: &Expr) -> Option<u16> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        hash::shallow(e, &mut h);
        self.index.get(&h.finish())?.iter().copied().find(|&i| {
            self.keys
                .get(usize::from(i))
                .map(|k| &k.trusted_borrow().kind)
                == Some(&e.kind)
        })
    }
    fn reads_own(&self, e: &Expr) -> bool {
        let mut found = false;
        ir::visit_expr_in_place(e, false, &mut |e| {
            if let E::Col { src, .. } = &e.kind {
                found |= self.own.contains(src);
            }
        });
        found
    }
    fn bx(&self, e: &Expr) -> BuildResult<NativeOwned<'a, Box<Expr>>> {
        let e = self.expr(e)?;
        self.ctx.parent_depth(std::iter::once(e.depth()))?;
        self.ctx.boxed(e)
    }
    fn maybe(&self, e: Option<&Expr>) -> BuildResult<NativeOwned<'a, Option<Box<Expr>>>> {
        match e {
            Some(e) => Ok(self.bx(e)?.map(Some)),
            None => Ok(self.ctx.scalar(()).map(|()| None)),
        }
    }
    pub(super) fn exprs(&self, es: &[Expr]) -> BuildResult<NativeOwned<'a, Vec<Expr>>> {
        let mut output = self.ctx.vector(es.len())?;
        for e in es {
            let e = self.expr(e)?;
            self.ctx.parent_depth(std::iter::once(e.depth()))?;
            self.ctx.push(&mut output, e)?;
        }
        Ok(output.finish())
    }
    pub(super) fn expr(&self, e: &Expr) -> BuildResult<NativeOwned<'a, Expr>> {
        let c = self.ctx;
        c.checkpoint()?;
        if self.replace
            && let Some(key) = self.find(e)
        {
            if !self.in_sub {
                return c.expr(c.scalar(key).map(E::KeyRef), c.copy(&e.ty)?);
            }
            if let Some(src) = self.anchor
                && self.reads_own(e)
            {
                let copier = Self {
                    replace: false,
                    ..*self
                };
                let orig = copier.bx(e)?;
                return c.expr(
                    orig.map(|orig| E::OuterKey { src, key, orig }),
                    c.copy(&e.ty)?,
                );
            }
        }
        let kind = match &e.kind {
            E::Col { src, col } => c.scalar((*src, *col)).map(|(src, col)| E::Col { src, col }),
            E::Lit(v) => c.copy(v)?.map(E::Lit),
            E::Param(i) => c.scalar(*i).map(E::Param),
            E::Value(i) => c.scalar(*i).map(E::Value),
            E::KeyRef(i) => c.scalar(*i).map(E::KeyRef),
            E::AggRef(i) => c.scalar(*i).map(E::AggRef),
            E::Out(i) => c.scalar(*i).map(E::Out),
            E::Unit(k) => c.scalar(*k).map(E::Unit),
            E::OuterKey { src, key, orig } => {
                let copier = Self {
                    replace: false,
                    ..*self
                };
                copier.bx(orig)?.map(|orig| E::OuterKey {
                    src: *src,
                    key: *key,
                    orig,
                })
            }
            E::Deref { base, steps } => {
                let base = self.bx(base)?;
                c.allocated(base.zip(c.copy(steps)?))?
                    .map(|(base, steps)| E::Deref { base, steps })
            }
            E::Neg(x) => self.bx(x)?.map(E::Neg),
            E::Not(x) => self.bx(x)?.map(E::Not),
            E::Arith { first, steps } => {
                let first = self.bx(first)?;
                let mut output = c.vector(steps.len())?;
                for (op, x, ty) in steps {
                    let x = self.expr(x)?;
                    c.parent_depth(std::iter::once(x.depth()))?;
                    let pair = c.allocated(x.zip(c.copy(ty)?))?.map(|(x, ty)| (*op, x, ty));
                    c.push(&mut output, pair)?;
                }
                c.allocated(first.zip(output.finish()))?
                    .map(|(first, steps)| E::Arith { first, steps })
            }
            E::And(v) => self.exprs(v)?.map(E::And),
            E::Or(v) => self.exprs(v)?.map(E::Or),
            E::Compare(op, l, r) => {
                let l = self.bx(l)?;
                c.allocated(l.zip(self.bx(r)?))?
                    .map(|(l, r)| E::Compare(*op, l, r))
            }
            E::InList {
                l,
                not,
                hierarchy,
                rhs,
            } => {
                let l = self.exprs(l)?;
                c.allocated(l.zip(self.exprs(rhs)?))?
                    .map(|(l, rhs)| E::InList {
                        l,
                        not: *not,
                        hierarchy: *hierarchy,
                        rhs,
                    })
            }
            E::InQuery {
                l,
                not,
                hierarchy,
                q,
            } => {
                let l = self.exprs(l)?;
                let sub = Self {
                    in_sub: true,
                    ..*self
                };
                let q = sub.rel(q)?;
                c.parent_depth([l.depth(), q.depth()].into_iter())?;
                let q = c.boxed(q)?;
                c.allocated(l.zip(q))?.map(|(l, q)| E::InQuery {
                    l,
                    not: *not,
                    hierarchy: *hierarchy,
                    q,
                })
            }
            E::SectionAgg { .. } => {
                return Err(BuildProblem::Later {
                    what: "агрегат по табличной части в оплачиваемом построении",
                    stage: "Q3a",
                });
            }
            E::Between { l, not, lo, hi } => {
                let l = self.bx(l)?;
                let lo = self.bx(lo)?;
                let pair = c.allocated(l.zip(lo))?;
                c.allocated(pair.zip(self.bx(hi)?))?
                    .map(|((l, lo), hi)| E::Between {
                        l,
                        not: *not,
                        lo,
                        hi,
                    })
            }
            E::IsNull { l, not } => self.bx(l)?.map(|l| E::IsNull { l, not: *not }),
            E::Refs { l, table } => self.bx(l)?.map(|l| E::Refs { l, table: *table }),
            E::Like { l, not, pat, esc } => {
                let l = self.bx(l)?;
                c.allocated(l.zip(self.bx(pat)?))?.map(|(l, pat)| E::Like {
                    l,
                    not: *not,
                    pat,
                    esc: *esc,
                })
            }
            E::Case {
                operand,
                whens,
                else_,
            } => {
                let operand = self.maybe(operand.as_deref())?;
                let mut output = c.vector(whens.len())?;
                for (condition, result) in whens {
                    let condition = self.expr(condition)?;
                    let result = self.expr(result)?;
                    c.parent_depth([condition.depth(), result.depth()].into_iter())?;
                    c.push(&mut output, c.allocated(condition.zip(result))?)?;
                }
                let pair = c.allocated(operand.zip(output.finish()))?;
                c.allocated(pair.zip(self.maybe(else_.as_deref())?))?.map(
                    |((operand, whens), else_)| E::Case {
                        operand,
                        whens,
                        else_,
                    },
                )
            }
            E::Cast { x, to, num } => {
                let x = self.bx(x)?;
                c.allocated(x.zip(c.copy(to)?))?
                    .map(|(x, to)| E::Cast { x, to, num: *num })
            }
            E::Func { f, args } => self.exprs(args)?.map(|args| E::Func { f: *f, args }),
        };
        c.expr(kind, c.copy(&e.ty)?)
    }
    fn sort_keys(&self, keys: &[ir::SortKey]) -> BuildResult<NativeOwned<'a, Vec<ir::SortKey>>> {
        let c = self.ctx;
        let mut out = c.vector(keys.len())?;
        for key in keys {
            let expr = self.expr(&key.expr)?;
            c.parent_depth(std::iter::once(expr.depth()))?;
            c.push(
                &mut out,
                expr.map(|expr| ir::SortKey {
                    expr,
                    desc: key.desc,
                }),
            )?;
        }
        Ok(out.finish())
    }
    pub(super) fn rel(&self, r: &Rel) -> BuildResult<NativeOwned<'a, Rel>> {
        let c = self.ctx;
        c.checkpoint()?;
        match r {
            Rel::Empty => c.empty_rel(),
            Rel::Scan { src, table } => {
                let d = c.parent_depth(std::iter::empty())?;
                Ok(c.scalar((*src, *table))
                    .map(|(src, table)| Rel::Scan { src, table })
                    .set_depth(d))
            }
            Rel::TempScan { src, name } => {
                let d = c.parent_depth(std::iter::empty())?;
                Ok(c.string(name)?
                    .map(|name| Rel::TempScan { src: *src, name })
                    .set_depth(d))
            }
            Rel::ParamScan { src, param } => {
                let d = c.parent_depth(std::iter::empty())?;
                Ok(c.scalar((*src, *param))
                    .map(|(src, param)| Rel::ParamScan { src, param })
                    .set_depth(d))
            }
            Rel::Subquery { src, input } => c.subquery(*src, self.rel(input)?),
            Rel::RegisterVt(_) | Rel::Slice(_) | Rel::RunningBalance(_) => {
                Err(BuildProblem::Later {
                    what: "виртуальные таблицы",
                    stage: "Q5",
                })
            }
            Rel::Join { kind, l, r, on } => {
                let l = self.rel(l)?;
                let r = self.rel(r)?;
                c.join(*kind, l, r, self.expr(on)?)
            }
            Rel::Filter { input, pred } => {
                let input = self.rel(input)?;
                c.filter(input, self.expr(pred)?)
            }
            Rel::Aggregate {
                input,
                keys,
                sets,
                aggs,
            } => {
                let input = self.rel(input)?;
                let keys = self.exprs(keys)?;
                let sets = c.copy(sets)?;
                let mut output = c.vector(aggs.len())?;
                for agg in aggs {
                    let arg = match &agg.arg {
                        Some(arg) => self.expr(arg)?.map(Some),
                        None => c.scalar(()).map(|()| None),
                    };
                    let a = c
                        .allocated(arg.zip(c.copy(&agg.ty)?))?
                        .map(|(arg, ty)| ir::AggCall {
                            f: agg.f,
                            distinct: agg.distinct,
                            arg,
                            ty,
                        });
                    c.parent_depth(std::iter::once(a.depth()))?;
                    c.push(&mut output, a)?;
                }
                c.aggregate(input, keys, sets, output.finish())
            }
            Rel::Project { input, exprs } => {
                let input = self.rel(input)?;
                c.project(input, self.exprs(exprs)?)
            }
            Rel::Distinct(input) => {
                let input = self.rel(input)?;
                let depth = c.parent_depth(std::iter::once(input.depth()))?;
                Ok(c.boxed(input)?.map(Rel::Distinct).set_depth(depth))
            }
            Rel::Sort { input, keys } => {
                let input = self.rel(input)?;
                c.sort(input, self.sort_keys(keys)?, None)
            }
            Rel::Hierarchy {
                input,
                id,
                parent,
                keys,
            } => {
                let input = self.rel(input)?;
                c.sort(input, self.sort_keys(keys)?, Some((*id, *parent)))
            }
            Rel::Top { input, n } => {
                let input = self.rel(input)?;
                let depth = c.parent_depth(std::iter::once(input.depth()))?;
                Ok(c.boxed(input)?
                    .map(|input| Rel::Top { input, n: *n })
                    .set_depth(depth))
            }
            Rel::Union { parts, all } => {
                let mut output = c.vector(parts.len())?;
                for part in parts {
                    let part = self.rel(part)?;
                    c.parent_depth(std::iter::once(part.depth()))?;
                    c.push(&mut output, part)?;
                }
                c.union(output.finish(), c.copy(all)?)
            }
        }
    }
    pub(super) fn correlated(&self, e: &Expr) -> bool {
        let mut found = false;
        ir::visit_expr_in_place(e, false, &mut |n| {
            if let E::InQuery { q, .. } = &n.kind {
                found |= self.rel_reads_own(q);
            }
        });
        found
    }
    fn rel_reads_own(&self, r: &Rel) -> bool {
        let reads = |e: &Expr| {
            let mut found = false;
            ir::visit_expr_in_place(e, true, &mut |n| {
                if let E::Col { src, .. } = &n.kind {
                    found |= self.own.contains(src);
                }
            });
            found
        };
        match r {
            Rel::Empty | Rel::Scan { .. } | Rel::TempScan { .. } | Rel::ParamScan { .. } => false,
            Rel::Subquery { input, .. } | Rel::Distinct(input) | Rel::Top { input, .. } => {
                self.rel_reads_own(input)
            }
            Rel::Join { l, r, on, .. } => {
                self.rel_reads_own(l) || self.rel_reads_own(r) || reads(on)
            }
            Rel::Filter { input, pred } => self.rel_reads_own(input) || reads(pred),
            Rel::Aggregate {
                input, keys, aggs, ..
            } => {
                self.rel_reads_own(input)
                    || keys.iter().any(reads)
                    || aggs.iter().filter_map(|a| a.arg.as_ref()).any(reads)
            }
            Rel::Project { input, exprs } => self.rel_reads_own(input) || exprs.iter().any(reads),
            Rel::Sort { input, keys } | Rel::Hierarchy { input, keys, .. } => {
                self.rel_reads_own(input) || keys.iter().any(|k| reads(&k.expr))
            }
            Rel::Union { parts, .. } => parts.iter().any(|p| self.rel_reads_own(p)),
            Rel::RegisterVt(v) | Rel::Slice(v) | Rel::RunningBalance(v) => {
                v.bounds.iter().flatten().any(reads)
                    || v.cond
                        .iter()
                        .chain(v.area.iter().map(|a| &a.pred))
                        .chain(v.access.iter())
                        .any(reads)
            }
        }
    }
}
