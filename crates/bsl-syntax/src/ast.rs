use crate::source::Span;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Module {
    pub vars: Vec<VarDecl>,
    pub methods: Vec<Method>,
    pub body: Vec<Stmt>,
    pub regions: Vec<Region>,
    pub incomplete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub name: Ident,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Annotation {
    pub name: Ident,
    pub arg: Option<Box<[u16]>>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VarDecl {
    pub annotations: Vec<Annotation>,
    pub items: Vec<VarItem>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VarItem {
    pub name: Ident,
    pub export: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MethodKind {
    Procedure,
    Function,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Method {
    pub annotations: Vec<Annotation>,
    pub async_kw: Option<Span>,
    pub kind: MethodKind,
    pub name: Ident,
    pub params: Vec<Param>,
    pub export: bool,
    pub vars: Vec<VarDecl>,
    pub body: Vec<Stmt>,
    pub span: Span,
    pub end_kw: Option<Span>,
    pub base_tokens: Option<Vec<Span>>,
    pub incomplete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub by_val: bool,
    pub name: Ident,
    pub default: Option<Literal>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Literal {
    Number(String),
    Str(Box<[u16]>),
    Date(DateLit),
    Bool(bool),
    Undefined,
    Null,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DateLit {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StmtKind {
    Assign {
        target: Expr,
        value: Expr,
    },
    Call(Expr),
    If {
        arms: Vec<(Expr, Vec<Stmt>)>,
        else_body: Option<Vec<Stmt>>,
        arm_kw: Vec<u32>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    For {
        var: Ident,
        from: Expr,
        to: Expr,
        body: Vec<Stmt>,
    },
    ForEach {
        var: Ident,
        collection: Expr,
        body: Vec<Stmt>,
    },
    Break,
    Continue,
    Return(Option<Expr>),
    Try {
        body: Vec<Stmt>,
        handler: Vec<Stmt>,
    },
    Raise(RaiseForm),
    Goto(Ident),
    Label(Ident),
    Execute(Expr),
    AddHandler {
        event: Expr,
        handler: Expr,
    },
    RemoveHandler {
        event: Expr,
        handler: Expr,
    },
    Await(Expr),
    Value(Expr),
    Incomplete(Vec<Expr>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RaiseForm {
    Rethrow,
    Expr(Expr),
    Call(Vec<Arg>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExprKind {
    Lit(Literal),
    Name(Ident),
    Member {
        object: Box<Expr>,
        name: Ident,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Arg>,
    },
    New {
        type_name: Ident,
        args: Option<Vec<Arg>>,
    },
    NewDynamic {
        args: Vec<Arg>,
    },
    Unary {
        op: UnOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Ternary {
        cond: Box<Expr>,
        then: Box<Expr>,
        otherwise: Box<Expr>,
    },
    Paren(Box<Expr>),
    Await(Box<Expr>),
}

const HOLE: ExprKind = ExprKind::Lit(Literal::Undefined);

impl Expr {
    pub fn into_kind(mut self) -> ExprKind {
        std::mem::replace(&mut self.kind, HOLE)
    }
}

impl Drop for Expr {
    #[inline]
    fn drop(&mut self) {
        if !self.kind.is_shallow() {
            self.drop_deep();
        }
    }
}

impl Expr {
    #[inline(never)]
    fn drop_deep(&mut self) {
        let mut kind = std::mem::replace(&mut self.kind, HOLE);
        let mut rest: Vec<Expr> = Vec::new();
        loop {
            let mut next: Option<Expr> = None;
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

impl ExprKind {
    #[inline]
    fn is_leaf(&self) -> bool {
        matches!(
            self,
            ExprKind::Lit(_) | ExprKind::Name(_) | ExprKind::New { args: None, .. }
        )
    }

    #[inline]
    fn is_shallow(&self) -> bool {
        let leaf = |e: &Expr| e.kind.is_leaf();
        let leaves = |args: &[Arg]| args.iter().all(|a| a.value.as_ref().is_none_or(leaf));
        match self {
            ExprKind::Lit(_) | ExprKind::Name(_) => true,
            ExprKind::Member { object: e, .. }
            | ExprKind::Unary { operand: e, .. }
            | ExprKind::Paren(e)
            | ExprKind::Await(e) => leaf(e),
            ExprKind::Index {
                object: a,
                index: b,
            }
            | ExprKind::Binary { lhs: a, rhs: b, .. } => leaf(a) && leaf(b),
            ExprKind::Call { callee, args } => leaf(callee) && leaves(args),
            ExprKind::New { args, .. } => args.as_deref().is_none_or(leaves),
            ExprKind::NewDynamic { args } => leaves(args),
            ExprKind::Ternary {
                cond,
                then,
                otherwise,
            } => leaf(cond) && leaf(then) && leaf(otherwise),
        }
    }

    fn take_children(self, mut f: impl FnMut(Expr)) {
        fn values(args: Vec<Arg>, f: impl FnMut(Expr)) {
            args.into_iter().filter_map(|a| a.value).for_each(f);
        }
        match self {
            ExprKind::Lit(_) | ExprKind::Name(_) => {}
            ExprKind::New { args, .. } => values(args.unwrap_or_default(), f),
            ExprKind::NewDynamic { args } => values(args, f),
            ExprKind::Call { callee, args } => {
                f(*callee);
                values(args, f);
            }
            ExprKind::Member { object, .. } => f(*object),
            ExprKind::Index { object, index } => {
                f(*object);
                f(*index);
            }
            ExprKind::Unary { operand, .. } => f(*operand),
            ExprKind::Binary { lhs, rhs, .. } => {
                f(*lhs);
                f(*rhs);
            }
            ExprKind::Ternary {
                cond,
                then,
                otherwise,
            } => {
                f(*cond);
                f(*then);
                f(*otherwise);
            }
            ExprKind::Paren(inner) | ExprKind::Await(inner) => f(*inner),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnOp {
    Neg,
    Pos,
    Not,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arg {
    pub value: Option<Expr>,
    pub span: Span,
}
