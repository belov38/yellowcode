use crate::ParseMode;
use crate::kw::Func;
use crate::source::{SourceMap, Span};

#[derive(Clone, Debug, PartialEq)]
pub struct Ident {
    pub text: Box<str>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    pub parts: Vec<Ident>,
}

impl Path {
    pub fn span(&self) -> Span {
        match (self.parts.first(), self.parts.last()) {
            (Some(a), Some(b)) => a.span.to(b.span),
            _ => Span::default(),
        }
    }
    pub fn dotted(&self) -> String {
        let mut out = String::new();
        for (i, p) in self.parts.iter().enumerate() {
            if i > 0 {
                out.push('.');
            }
            out.push_str(&p.text);
        }
        out
    }
}

#[derive(Debug)]
pub struct Batch {
    pub stmts: Vec<Stmt>,
    pub map: SourceMap,
    pub mode: ParseMode,
    pub max_depth: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    Query(Box<QueryStmt>),
    Drop(DropStmt),
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueryStmt {
    pub query: Query,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DropStmt {
    pub kw: Span,
    pub name: Ident,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    pub parts: Vec<Select>,
    pub unions: Vec<UnionOp>,
    pub order_by: Option<OrderBy>,
    pub autoorder: Option<Span>,
    pub totals: Option<Totals>,
    pub dcs: Vec<DcsBlock>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UnionOp {
    pub kw: Span,
    pub all: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Select {
    pub kw: Span,
    pub allowed: Option<Span>,
    pub distinct: Option<Span>,
    pub top: Option<Top>,
    pub items: Vec<SelectItem>,
    pub into: Option<IntoClause>,
    pub from: Option<FromClause>,
    pub where_: Option<Clause>,
    pub group_by: Option<GroupBy>,
    pub having: Option<Clause>,
    pub for_update: Option<ForUpdate>,
    pub index_by: Option<IndexBy>,
    pub dcs: Vec<DcsBlock>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Top {
    pub kw: Span,
    pub count: NumLit,
    pub count_span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Clause {
    pub kw: Span,
    pub expr: Expr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SelectItem {
    pub kind: ItemKind,
    pub alias: Option<Alias>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ItemKind {
    Star,
    PathStar(Path),
    Expr(Expr),
    Nested {
        table: Expr,
        fields: Vec<SelectItem>,
    },
    EmptyTable {
        kw: Span,
        names: Vec<Option<Ident>>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Alias {
    pub name: Ident,
    pub as_kw: Option<Span>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IntoClause {
    pub kind: IntoKind,
    pub kw: Span,
    pub name: Ident,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntoKind {
    Put,
    Add,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FromClause {
    pub kw: Span,
    pub sources: Vec<Source>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub table: TableRef,
    pub alias: Option<Alias>,
    pub joins: Vec<Join>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TableRef {
    Named { path: Path, args: Option<VtArgs> },
    Subquery { query: Box<Query>, span: Span },
    Param(Param),
}

#[derive(Clone, Debug, PartialEq)]
pub struct VtArgs {
    pub args: Vec<Option<VtArg>>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VtArg {
    pub expr: Expr,
    pub dir: Option<(OrderDir, Span)>,
    pub dcs: Vec<DcsList>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Join {
    pub kind: JoinKind,
    pub kw: Span,
    pub right: Source,
    pub on_kw: Span,
    pub on: Expr,
    pub optional: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    Left,
    Right,
    Full,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GroupBy {
    pub kw: Span,
    pub kind: GroupKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GroupKind {
    Exprs(Vec<Expr>),
    Sets(Vec<Vec<Expr>>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ForUpdate {
    pub kw: Span,
    pub tables: Vec<Path>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IndexBy {
    pub kw: Span,
    pub kind: IndexKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum IndexKind {
    Fields {
        fields: Vec<Expr>,
        unique: Option<Span>,
    },
    Sets(Vec<IndexSet>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct IndexSet {
    pub fields: Vec<Expr>,
    pub unique: Option<Span>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OrderBy {
    pub kw: Span,
    pub items: Vec<OrderItem>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OrderItem {
    pub expr: Expr,
    pub dir: OrderDir,
    pub dir_kw: Option<Span>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderDir {
    Asc,
    Desc,
    Hierarchy,
    HierarchyDesc,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Totals {
    pub kw: Span,
    pub items: Vec<TotalsItem>,
    pub by_kw: Span,
    pub points: Vec<ControlPoint>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TotalsItem {
    pub expr: Expr,
    pub alias: Option<Alias>,
}

#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum ControlPoint {
    Overall(Span),
    Field {
        expr: Expr,
        hierarchy: Option<Hierarchy>,
        periods: Option<Periods>,
        alias: Option<Alias>,
        span: Span,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Hierarchy {
    All(Span),
    Only(Span),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Periods {
    pub kw: Span,
    pub unit: Ident,
    pub begin: Option<Expr>,
    pub end: Option<Expr>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Lit(Lit),
    Param(Param),
    Path(Path),
    Deref {
        expr: Box<Expr>,
        deref: Vec<Ident>,
    },
    Chain {
        first: Box<Expr>,
        rest: Vec<ChainOp>,
    },
    Unary {
        op: UnOp,
        op_span: Span,
        expr: Box<Expr>,
    },
    Compare {
        op: CmpOp,
        op_span: Span,
        l: Box<Expr>,
        r: Box<Expr>,
    },
    In {
        l: Box<Expr>,
        not: bool,
        hierarchy: bool,
        kw: Span,
        rhs: InRhs,
    },
    Between {
        l: Box<Expr>,
        not: bool,
        kw: Span,
        lo: Box<Expr>,
        hi: Box<Expr>,
    },
    IsNull {
        l: Box<Expr>,
        not: bool,
        kw: Span,
    },
    Refs {
        l: Box<Expr>,
        kw: Span,
        table: Path,
    },
    Like {
        l: Box<Expr>,
        not: bool,
        kw: Span,
        pattern: Box<Expr>,
        escape: Option<Escape>,
    },
    Tuple(Vec<Expr>),
    Case {
        kw: Span,
        operand: Option<Box<Expr>>,
        whens: Vec<When>,
        else_: Option<Box<Expr>>,
    },
    Cast {
        kw: Span,
        expr: Box<Expr>,
        ty: CastType,
        deref: Vec<Ident>,
    },
    Func {
        func: Func,
        name_span: Span,
        distinct: Option<Span>,
        args: FuncArgs,
    },
    TypeOf {
        kw: Span,
        name: Path,
    },
    Value {
        kw: Span,
        path: Path,
    },
    DateTime {
        kw: Span,
        args: Vec<Expr>,
    },
    Paren(Box<Expr>),
    Dcs(DcsList),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChainOp {
    pub op: BinOp,
    pub op_span: Span,
    pub expr: Expr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    And,
    Or,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Plus,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Lit {
    Number(NumLit),
    String(Box<str>),
    Bool(bool),
    Null,
    Undefined,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NumLit {
    pub digits: Box<str>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: Ident,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InRhs {
    List(Vec<Expr>),
    Query(Box<Query>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Escape {
    pub kw: Span,
    pub ch: Box<str>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct When {
    pub kw: Span,
    pub cond: Expr,
    pub then: Expr,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CastType {
    Boolean,
    Date,
    Number {
        len: Option<NumLit>,
        scale: Option<NumLit>,
    },
    String {
        len: Option<NumLit>,
    },
    Table(Path),
}

#[derive(Clone, Debug, PartialEq)]
pub enum FuncArgs {
    Star(Span),
    List(Vec<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct DcsBlock {
    pub kind: DcsKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DcsKind {
    Select(DcsList),
    Where(DcsList),
    OrderBy(DcsList),
    GroupBy(DcsList),
    Totals(DcsList),
    Characteristics(Box<Characteristics>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct DcsList {
    pub items: Vec<DcsItem>,
    pub unresolved_tail: Option<DcsUnresolvedTail>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DcsUnresolvedTail {
    AdjacentParam(Param),
}

#[derive(Clone, Debug, PartialEq)]
pub struct DcsItem {
    pub expr: Expr,
    pub star: Option<Span>,
    pub nested: Option<DcsList>,
    pub dcs: Vec<DcsList>,
    pub dir: Option<(OrderDir, Span)>,
    pub alias: Option<Alias>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Characteristics {
    pub kw: Span,
    pub type_name: Path,
    pub kinds: CharSource,
    pub key_field: Path,
    pub name_field: Path,
    pub value_type_field: Option<Path>,
    pub values: CharSource,
    pub object_field: Path,
    pub kind_field: Path,
    pub value_field: Option<Path>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CharSource {
    Table(Path),
    Query(Box<Query>),
}

#[derive(Debug)]
pub struct AccessCondition {
    pub form: AccessForm,
    pub map: SourceMap,
    pub max_depth: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AccessForm {
    Short {
        where_kw: Span,
        cond: Expr,
    },
    Full {
        alias: Ident,
        from: FromClause,
        where_: Option<Clause>,
    },
}
