use super::{self as mem, CompileArena, ManagedClone, NativeOwned, NativeVec, ScratchVec};
use memory_budget::{
    BudgetError, RequestBudget, ResourceCause, ResourceFailure, ResourcePhase, StructureLimit,
};

mod from_tir;
mod hash;
mod rebuild;
pub(crate) use from_tir::build;

pub(crate) enum BuildProblem {
    Resource(ResourceFailure),
    Later {
        what: &'static str,
        stage: &'static str,
    },
    Internal(&'static str),
}
pub(crate) type BuildResult<T> = Result<T, BuildProblem>;

pub(crate) struct BuildCtx<'a, 'r, 'w> {
    pub(crate) arena: &'a CompileArena,
    pub(crate) request: &'a RequestBudget<'r, 'w>,
}
impl<'a> BuildCtx<'a, '_, '_> {
    pub(crate) fn refuse(&self, error: BudgetError) -> BuildProblem {
        BuildProblem::Resource(self.request.record(crate::worker_build::resource(error)))
    }
    pub(crate) fn checkpoint(&self) -> BuildResult<()> {
        if let Some(error) = self.arena.error() {
            return Err(self.refuse(error));
        }
        self.request
            .checkpoint(ResourcePhase::Build)
            .map_err(BuildProblem::Resource)
    }
    pub(crate) fn allocated<T>(&self, value: mem::AResult<T>) -> BuildResult<T> {
        value.map_err(|error| self.refuse(error))
    }
    pub(crate) fn managed<T>(&self, value: crate::sema::R<T>) -> BuildResult<T> {
        value.map_err(|_| self.refuse(self.arena.error().unwrap_or(BudgetError::Overflow)))
    }
    pub(crate) fn copy<T: ManagedClone + Clone>(
        &self,
        value: &T,
    ) -> BuildResult<NativeOwned<'a, T>> {
        self.checkpoint()?;
        self.managed(mem::owned_copy(Some(self.arena), value))
    }
    pub(crate) fn vector<T>(&self, capacity: usize) -> BuildResult<NativeVec<'a, T>> {
        self.checkpoint()?;
        self.managed(NativeVec::new(Some(self.arena), capacity))
    }
    pub(crate) fn string(&self, text: &str) -> BuildResult<NativeOwned<'a, String>> {
        self.checkpoint()?;
        Ok(self
            .managed(mem::OwnedString::new(Some(self.arena), text))?
            .into_native_owned())
    }
    pub(crate) fn push<T>(
        &self,
        values: &mut NativeVec<'a, T>,
        value: NativeOwned<'a, T>,
    ) -> BuildResult<()> {
        self.checkpoint()?;
        self.managed(values.push_owned(value))
    }
    pub(crate) fn boxed<T>(
        &self,
        value: NativeOwned<'a, T>,
    ) -> BuildResult<NativeOwned<'a, Box<T>>> {
        self.parent_depth(std::iter::once(value.depth()))?;
        self.allocated(value.boxed())
    }
    pub(crate) fn parent_depth(&self, children: impl Iterator<Item = u16>) -> BuildResult<u16> {
        self.checkpoint()?;
        let child = children.max().unwrap_or(0);
        if let Some(depth) = child.checked_add(1).filter(|depth| *depth <= 128) {
            Ok(depth)
        } else {
            Err(BuildProblem::Resource(self.request.record(
                ResourceFailure {
                    phase: ResourcePhase::Build,
                    cause: ResourceCause::Structure(StructureLimit::Depth),
                },
            )))
        }
    }
    pub(crate) fn scratch<T>(&self, capacity: usize) -> BuildResult<ScratchVec<T>> {
        self.checkpoint()?;
        self.managed(ScratchVec::new(Some(self.arena), capacity))
    }
    pub(crate) fn scratch_push<T>(&self, values: &mut ScratchVec<T>, value: T) -> BuildResult<()> {
        self.checkpoint()?;
        self.managed(values.push(Some(self.arena), value))
    }
    pub(crate) fn scalar<T: Copy>(&self, value: T) -> NativeOwned<'a, T> {
        NativeOwned::inline(Some(self.arena), value)
    }
    pub(crate) fn finish<T>(
        &self,
        values: &mut ScratchVec<NativeOwned<'a, T>>,
    ) -> BuildResult<NativeOwned<'a, Vec<T>>> {
        let mut output = self.vector(values.len())?;
        values.reverse();
        while let Some(value) = values.pop() {
            self.push(&mut output, value)?;
        }
        Ok(output.finish())
    }
    pub(crate) fn expr(
        &self,
        kind: NativeOwned<'a, crate::ir::ExprKind>,
        ty: NativeOwned<'a, crate::QType>,
    ) -> BuildResult<NativeOwned<'a, crate::ir::Expr>> {
        let depth = self.parent_depth(std::iter::once(kind.depth()))?;
        Ok(self
            .allocated(kind.zip(ty))?
            .map(|(kind, ty)| crate::ir::Expr { kind, ty })
            .set_depth(depth))
    }
    pub(crate) fn empty_rel(&self) -> BuildResult<NativeOwned<'a, crate::ir::Rel>> {
        let depth = self.parent_depth(std::iter::empty())?;
        Ok(NativeOwned::leaf(crate::ir::Rel::Empty, Some(self.arena), 0).set_depth(depth))
    }
    pub(crate) fn project(
        &self,
        input: NativeOwned<'a, crate::ir::Rel>,
        exprs: NativeOwned<'a, Vec<crate::ir::Expr>>,
    ) -> BuildResult<NativeOwned<'a, crate::ir::Rel>> {
        let depth = self.parent_depth([input.depth(), exprs.depth()].into_iter())?;
        let input = self.boxed(input)?;
        Ok(self
            .allocated(input.zip(exprs))?
            .map(|(input, exprs)| crate::ir::Rel::Project { input, exprs })
            .set_depth(depth))
    }
    pub(crate) fn subquery(
        &self,
        src: crate::ir::SrcId,
        input: NativeOwned<'a, crate::ir::Rel>,
    ) -> BuildResult<NativeOwned<'a, crate::ir::Rel>> {
        let depth = self.parent_depth(std::iter::once(input.depth()))?;
        Ok(self
            .boxed(input)?
            .map(|input| crate::ir::Rel::Subquery { src, input })
            .set_depth(depth))
    }
    pub(crate) fn filter(
        &self,
        input: NativeOwned<'a, crate::ir::Rel>,
        pred: NativeOwned<'a, crate::ir::Expr>,
    ) -> BuildResult<NativeOwned<'a, crate::ir::Rel>> {
        let depth = self.parent_depth([input.depth(), pred.depth()].into_iter())?;
        let input = self.boxed(input)?;
        Ok(self
            .allocated(input.zip(pred))?
            .map(|(input, pred)| crate::ir::Rel::Filter { input, pred })
            .set_depth(depth))
    }
    pub(crate) fn join(
        &self,
        kind: query_syntax::ast::JoinKind,
        l: NativeOwned<'a, crate::ir::Rel>,
        r: NativeOwned<'a, crate::ir::Rel>,
        on: NativeOwned<'a, crate::ir::Expr>,
    ) -> BuildResult<NativeOwned<'a, crate::ir::Rel>> {
        let depth = self.parent_depth([l.depth(), r.depth(), on.depth()].into_iter())?;
        let l = self.boxed(l)?;
        let r = self.boxed(r)?;
        let pair = self.allocated(l.zip(r))?;
        Ok(self
            .allocated(pair.zip(on))?
            .map(|((l, r), on)| crate::ir::Rel::Join { kind, l, r, on })
            .set_depth(depth))
    }
    pub(crate) fn merge_type(
        &self,
        left: &crate::QType,
        right: &crate::QType,
    ) -> BuildResult<NativeOwned<'a, crate::QType>> {
        self.checkpoint()?;
        let merged = self.allocated(left.try_merge_budgeted(right, &self.arena.budget))?;
        self.copy(&*merged)
    }
    pub(crate) fn sort(
        &self,
        input: NativeOwned<'a, crate::ir::Rel>,
        keys: NativeOwned<'a, Vec<crate::ir::SortKey>>,
        hierarchy: Option<(u16, u16)>,
    ) -> BuildResult<NativeOwned<'a, crate::ir::Rel>> {
        let depth = self.parent_depth([input.depth(), keys.depth()].into_iter())?;
        let input = self.boxed(input)?;
        Ok(self
            .allocated(input.zip(keys))?
            .map(|(input, keys)| match hierarchy {
                None => crate::ir::Rel::Sort { input, keys },
                Some((id, parent)) => crate::ir::Rel::Hierarchy {
                    input,
                    id,
                    parent,
                    keys,
                },
            })
            .set_depth(depth))
    }
    pub(crate) fn aggregate(
        &self,
        input: NativeOwned<'a, crate::ir::Rel>,
        keys: NativeOwned<'a, Vec<crate::ir::Expr>>,
        sets: NativeOwned<'a, Option<Vec<Vec<u16>>>>,
        aggs: NativeOwned<'a, Vec<crate::ir::AggCall>>,
    ) -> BuildResult<NativeOwned<'a, crate::ir::Rel>> {
        let depth = self.parent_depth([input.depth(), keys.depth(), aggs.depth()].into_iter())?;
        let input = self.boxed(input)?;
        let parts = self.allocated(input.zip(keys))?;
        let parts = self.allocated(parts.zip(sets))?;
        Ok(self
            .allocated(parts.zip(aggs))?
            .map(|(((input, keys), sets), aggs)| crate::ir::Rel::Aggregate {
                input,
                keys,
                sets,
                aggs,
            })
            .set_depth(depth))
    }
    pub(crate) fn union(
        &self,
        parts: NativeOwned<'a, Vec<crate::ir::Rel>>,
        all: NativeOwned<'a, Vec<bool>>,
    ) -> BuildResult<NativeOwned<'a, crate::ir::Rel>> {
        let depth = self.parent_depth(std::iter::once(parts.depth()))?;
        Ok(self
            .allocated(parts.zip(all))?
            .map(|(parts, all)| crate::ir::Rel::Union { parts, all })
            .set_depth(depth))
    }
}
