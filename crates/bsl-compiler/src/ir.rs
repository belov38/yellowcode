use crate::app::{AppGlobal, AppMethodSig};
use crate::options::ModuleKind;
use crate::sym::Sym;
use bsl_prim::{BslDate, CompatMode, Number};
use bsl_syntax::{
    ExecContext, Span,
    ast::{BinOp, MethodKind, UnOp},
};
use platform_catalog::TypeIdx;
use std::sync::Arc;

#[derive(Debug)]
pub struct IrModule {
    pub name: String,
    pub kind: ModuleKind,
    pub context: ExecContext,
    pub compat: CompatMode,
    pub vars: Vec<IrVar>,
    pub methods: Vec<IrMethod>,
    pub body: Option<IrBody>,
    pub source_lines: Arc<[Arc<[u16]>]>,
    pub outer: Vec<Sym>,
    pub caller_methods: Vec<Sym>,
    pub caller_vars: Vec<Sym>,
}

#[derive(Debug)]
pub struct IrVar {
    pub name: Sym,
    pub export: bool,
    pub directive: Option<Directive>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Directive {
    AtClient,
    AtServer,
    AtServerNoContext,
    AtClientAtServerNoContext,
    AtClientAtServer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intercept {
    Before(String),
    After(String),
    Around(String),
    ChangeAndValidate(String),
}

#[derive(Debug)]
pub struct IrMethod {
    pub name: Sym,
    pub kind: MethodKind,
    pub export: bool,
    pub is_async: bool,
    pub directive: Option<Directive>,
    pub intercept: Option<Intercept>,
    pub params: Vec<IrParam>,
    pub body: IrBody,
    pub line: u32,
    pub callable_only: bool,
}

#[derive(Debug)]
pub struct IrParam {
    pub name: Sym,
    pub by_val: bool,
    pub default: Option<Const>,
}

#[derive(Debug)]
pub struct IrBody {
    pub slots: Vec<Sym>,
    pub stmts: Vec<IrStmt>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Const {
    Number(Number),
    String(Arc<[u16]>),
    Date(BslDate),
    Bool(bool),
    Undefined,
    Null,
}

#[derive(Debug)]
pub struct IrStmt {
    pub kind: IrStmtKind,
    pub line: u32,
    pub span: Span,
}

#[derive(Debug)]
pub struct IrArm {
    pub line: u32,
    pub cond: IrExpr,
    pub body: Vec<IrStmt>,
}

#[derive(Debug)]
pub enum IrStmtKind {
    Assign {
        target: Place,
        value: IrExpr,
    },
    Eval(IrExpr),
    If {
        arms: Vec<IrArm>,
        otherwise: Vec<IrStmt>,
    },
    While {
        cond: IrExpr,
        body: Vec<IrStmt>,
    },
    For {
        var: Place,
        from: IrExpr,
        to: IrExpr,
        bound_slot: u32,
        body: Vec<IrStmt>,
    },
    ForEach {
        var: Place,
        collection: IrExpr,
        iter_slot: u32,
        body: Vec<IrStmt>,
    },
    Break,
    Continue,
    Return(Option<IrExpr>),
    Try {
        body: Vec<IrStmt>,
        handler: Vec<IrStmt>,
    },
    Raise(IrExpr),
    RaiseCall(Vec<IrArg>),
    Reraise,
    Goto(u32),
    Label(u32),
    Execute(IrExpr),
    AddHandler {
        event: IrExpr,
        handler: IrExpr,
    },
    RemoveHandler {
        event: IrExpr,
        handler: IrExpr,
    },
    Await(IrExpr),
}

#[derive(Debug)]
pub enum Place {
    Local(u32),
    ModVar(u32),
    Outer(u32),
    Global(GlobalRef),
    Context(Sym),
    Member {
        object: Box<IrExpr>,
        name: Sym,
    },
    Index {
        object: Box<IrExpr>,
        index: Box<IrExpr>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GlobalRef {
    App(AppGlobal),
    PlatformProperty(u16),
    SystemEnum(TypeIdx),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Callee {
    Local(u32),
    App(AppMethodSig),
    Platform(u16),
    Context(Sym),
}

#[derive(Debug)]
pub struct IrExpr {
    pub kind: IrExprKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum IrExprKind {
    Const(Const),
    Read(Place),
    Call {
        callee: Callee,
        args: Vec<IrArg>,
        want_value: bool,
    },
    CallMember {
        object: Box<IrExpr>,
        name: Sym,
        args: Vec<IrArg>,
        want_value: bool,
    },
    New {
        ty: TypeIdx,
        args: Option<Vec<IrArg>>,
    },
    NewDynamic {
        args: Vec<IrArg>,
    },
    Unary {
        op: UnOp,
        operand: Box<IrExpr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<IrExpr>,
        rhs: Box<IrExpr>,
    },
    Ternary {
        cond: Box<IrExpr>,
        then: Box<IrExpr>,
        otherwise: Box<IrExpr>,
    },
    Evaluate(Box<IrExpr>),
    Await(Box<IrExpr>),
}

#[derive(Debug)]
pub enum IrArg {
    Omitted,
    Value(IrExpr),
    ByRef(Place),
}

const HOLE: IrExprKind = IrExprKind::Const(Const::Undefined);

impl IrExpr {
    pub fn into_kind(mut self) -> IrExprKind {
        std::mem::replace(&mut self.kind, HOLE)
    }
}

impl Drop for IrExpr {
    #[inline]
    fn drop(&mut self) {
        if !self.kind.is_shallow() {
            self.drop_deep();
        }
    }
}

impl IrExpr {
    #[inline(never)]
    fn drop_deep(&mut self) {
        let mut kind = std::mem::replace(&mut self.kind, HOLE);
        let mut rest: Vec<IrExpr> = Vec::new();
        loop {
            let mut next: Option<IrExpr> = None;
            kind.take_children(|c| {
                if !c.kind.is_shallow()
                    && let Some(prev) = next.replace(c)
                {
                    rest.push(prev);
                }
            });
            match next.or_else(|| rest.pop()) {
                Some(e) => kind = e.into_kind(),
                None => return,
            }
        }
    }
}

impl Place {
    fn is_leaf(&self) -> bool {
        !matches!(self, Place::Member { .. } | Place::Index { .. })
    }

    fn is_shallow(&self) -> bool {
        match self {
            Place::Member { object, .. } => object.kind.is_leaf(),
            Place::Index { object, index } => object.kind.is_leaf() && index.kind.is_leaf(),
            _ => true,
        }
    }

    fn take_children(self, mut f: impl FnMut(IrExpr)) {
        match self {
            Place::Member { object, .. } => f(*object),
            Place::Index { object, index } => {
                f(*object);
                f(*index);
            }
            _ => {}
        }
    }
}

impl IrArg {
    fn is_shallow(&self) -> bool {
        match self {
            IrArg::Omitted => true,
            IrArg::Value(e) => e.kind.is_leaf(),
            IrArg::ByRef(p) => p.is_shallow(),
        }
    }
}

impl IrExprKind {
    #[inline]
    fn is_leaf(&self) -> bool {
        match self {
            IrExprKind::Const(_) | IrExprKind::New { args: None, .. } => true,
            IrExprKind::Read(p) => p.is_leaf(),
            _ => false,
        }
    }

    #[inline]
    fn is_shallow(&self) -> bool {
        let leaf = |e: &IrExpr| e.kind.is_leaf();
        let args = |a: &[IrArg]| a.iter().all(IrArg::is_shallow);
        match self {
            IrExprKind::Const(_) => true,
            IrExprKind::Read(p) => p.is_shallow(),
            IrExprKind::Call { args: a, .. } | IrExprKind::NewDynamic { args: a } => args(a),
            IrExprKind::CallMember {
                object, args: a, ..
            } => leaf(object) && args(a),
            IrExprKind::New { args: a, .. } => a.as_deref().is_none_or(args),
            IrExprKind::Unary { operand: e, .. }
            | IrExprKind::Evaluate(e)
            | IrExprKind::Await(e) => leaf(e),
            IrExprKind::Binary { lhs, rhs, .. } => leaf(lhs) && leaf(rhs),
            IrExprKind::Ternary {
                cond,
                then,
                otherwise,
            } => leaf(cond) && leaf(then) && leaf(otherwise),
        }
    }

    fn take_children(self, mut f: impl FnMut(IrExpr)) {
        fn args(list: Vec<IrArg>, mut f: impl FnMut(IrExpr)) {
            for a in list {
                match a {
                    IrArg::Omitted => {}
                    IrArg::Value(e) => f(e),
                    IrArg::ByRef(p) => p.take_children(&mut f),
                }
            }
        }
        match self {
            IrExprKind::Const(_) => {}
            IrExprKind::Read(p) => p.take_children(f),
            IrExprKind::Call { args: a, .. } | IrExprKind::NewDynamic { args: a } => args(a, f),
            IrExprKind::CallMember {
                object, args: a, ..
            } => {
                f(*object);
                args(a, f);
            }
            IrExprKind::New { args: a, .. } => args(a.unwrap_or_default(), f),
            IrExprKind::Unary { operand: e, .. }
            | IrExprKind::Evaluate(e)
            | IrExprKind::Await(e) => f(*e),
            IrExprKind::Binary { lhs, rhs, .. } => {
                f(*lhs);
                f(*rhs);
            }
            IrExprKind::Ternary {
                cond,
                then,
                otherwise,
            } => {
                f(*cond);
                f(*then);
                f(*otherwise);
            }
        }
    }
}
