use crate::app::AppMethodSig;
use crate::ir::{Const, Directive, GlobalRef, Intercept, IrVar};
use crate::options::ModuleKind;
use crate::sym::Sym;
use bsl_prim::{CompatMode, fold_name};
use bsl_syntax::{ExecContext, ast::MethodKind};
use std::sync::Arc;

#[derive(Debug)]
pub struct CompiledModule {
    pub name: String,
    pub kind: ModuleKind,
    pub context: ExecContext,
    pub compat: CompatMode,
    pub consts: Vec<Const>,
    pub syms: Vec<Sym>,
    pub globals: Vec<GlobalRef>,
    pub app_methods: Vec<AppMethodSig>,
    pub vars: Vec<IrVar>,
    pub methods: Vec<MethodCode>,
    pub body: Option<MethodCode>,
    pub source_lines: Arc<[Arc<[u16]>]>,
    pub outer: Vec<Sym>,
    pub caller_methods: Vec<Sym>,
    pub caller_vars: Vec<Sym>,
}

impl CompiledModule {
    pub fn method(&self, name: &str) -> Option<(u32, &MethodCode)> {
        let folded = fold_name(name);
        let (i, m) = self
            .methods
            .iter()
            .enumerate()
            .find(|(_, m)| *m.name.folded == *folded)?;
        Some((u32::try_from(i).ok()?, m))
    }

    pub fn disassemble(&self) -> String {
        crate::disasm::module(self)
    }
}

#[derive(Debug)]
pub struct MethodCode {
    pub name: Sym,
    pub kind: MethodKind,
    pub export: bool,
    pub is_async: bool,
    pub directive: Option<Directive>,
    pub intercept: Option<Intercept>,
    pub params: Vec<ParamCode>,
    pub slot_names: Vec<Sym>,
    pub code: Vec<Op>,
    pub lines: Vec<(u32, u32)>,
    pub handlers: Vec<Handler>,
    pub callable_only: bool,
}

#[derive(Clone, Debug)]
pub struct ParamCode {
    pub name: Sym,
    pub by_val: bool,
    pub default: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handler {
    pub start: u32,
    pub end: u32,
    pub target: u32,
    pub depth: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Const(u32),
    Undef,
    Null,
    True,
    False,
    Pop,
    Dup,
    LoadLocal(u32),
    StoreLocal(u32),
    LoadModVar(u32),
    StoreModVar(u32),
    LoadOuter(u32),
    StoreOuter(u32),
    LoadGlobal(u32),
    StoreGlobal(u32),
    LoadContext(u32),
    StoreContext(u32),
    GetMember(u32),
    SetMember(u32),
    GetIndex,
    SetIndex,
    RefLocal(u32),
    RefModVar(u32),
    RefOuter(u32),
    RefMember(u32),
    RefIndex,
    Omitted,
    CallLocal { method: u32, argc: u32, want: bool },
    CallApp { method: u32, argc: u32, want: bool },
    CallPlatform { method: u16, argc: u32, want: bool },
    CallContext { name: u32, argc: u32, want: bool },
    CallMember { name: u32, argc: u32, want: bool },
    New { ty: u16, argc: u32 },
    NewNoArgs { ty: u16 },
    NewDynamic { argc: u32 },
    Return,
    ReturnValue,
    Neg,
    Pos,
    Not,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    ToBool,
    Jump(u32),
    JumpIfFalse(u32),
    JumpIfTrue(u32),
    IterStart(u32),
    IterNext { slot: u32, end: u32 },
    Raise,
    RaiseExt(u32),
    Reraise,
    EnterHandler,
    LeaveHandler(u16),
    Execute,
    Evaluate,
    Await,
    AddHandler,
    RemoveHandler,
}
