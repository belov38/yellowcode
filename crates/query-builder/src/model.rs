use query_syntax::{INLINE_DEPTH, ParseMode, ScriptVariant, ast, with_deep_stack};

#[derive(Debug)]
pub struct Schema {
    pub batch: Vec<Stmt>,
    pub mode: ParseMode,
    pub script: ScriptVariant,
    pub max_depth: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    Select(SelectQuery),
    Drop(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SelectQuery {
    pub operators: Vec<Operator>,
    pub allowed: bool,
    pub into: Option<Placement>,
    pub order: Vec<ast::OrderItem>,
    pub autoorder: bool,
    pub totals: Option<Totals>,
    pub index: Option<ast::IndexKind>,
    pub dcs: Vec<ast::DcsBlock>,
    pub names: Vec<Option<String>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    pub kind: ast::IntoKind,
    pub table: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Totals {
    pub items: Vec<ast::TotalsItem>,
    pub points: Vec<ast::ControlPoint>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Operator {
    pub union_all: bool,
    pub distinct: bool,
    pub top: Option<String>,
    pub fields: Vec<ast::SelectItem>,
    pub sources: Vec<Source>,
    pub filter: Vec<ast::Expr>,
    pub having: Vec<ast::Expr>,
    pub group: Option<ast::GroupKind>,
    pub for_update: Option<Vec<ast::Path>>,
    pub dcs: Vec<ast::DcsBlock>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub table: Table,
    pub alias: Option<ast::Alias>,
    pub joins: Vec<Join>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Table {
    Named {
        path: ast::Path,
        args: Option<Vec<Option<ast::VtArg>>>,
    },
    Subquery(Box<SelectQuery>),
    Param(ast::Param),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Join {
    pub kind: ast::JoinKind,
    pub right: Source,
    pub on: Vec<JoinCond>,
    pub optional: bool,
    pub same_block: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct JoinCond {
    pub expr: ast::Expr,
    pub api: bool,
}

impl Drop for Schema {
    fn drop(&mut self) {
        if self.max_depth > INLINE_DEPTH {
            let batch = std::mem::take(&mut self.batch);
            with_deep_stack(self.max_depth, move || drop(batch));
        }
    }
}

impl Schema {
    pub fn unresolved_syntax(&self) -> Vec<ast::DcsUnresolvedTail> {
        query_syntax::unresolved_dcs_tails(&crate::to_ast(self, crate::Conventions::AsWritten))
    }

    pub fn new(mode: ParseMode) -> Schema {
        Schema {
            batch: vec![Stmt::Select(SelectQuery::empty())],
            mode,
            script: ScriptVariant::Russian,
            max_depth: 0,
        }
    }

    pub fn to_text(&self) -> String {
        let script = self.script;
        with_deep_stack(self.max_depth, || {
            let mut b = crate::to_ast::to_ast_marked(self, crate::Conventions::OneC);
            let blocks = crate::to_ast::take_marks(&mut b);
            crate::pretty::pretty_with_blocks(&b, script, blocks)
        })
    }

    pub fn deep_clone(&self) -> Schema {
        let (batch, mode, script, max_depth) =
            (&self.batch, self.mode, self.script, self.max_depth);
        with_deep_stack(max_depth, || Schema {
            batch: batch.clone(),
            mode,
            script,
            max_depth,
        })
    }
}

impl SelectQuery {
    pub fn empty() -> SelectQuery {
        SelectQuery {
            operators: vec![Operator::empty()],
            allowed: false,
            into: None,
            order: Vec::new(),
            autoorder: false,
            totals: None,
            index: None,
            dcs: Vec::new(),
            names: Vec::new(),
        }
    }
}

impl Operator {
    pub fn empty() -> Operator {
        Operator {
            union_all: true,
            distinct: false,
            top: None,
            fields: Vec::new(),
            sources: Vec::new(),
            filter: Vec::new(),
            having: Vec::new(),
            group: None,
            for_update: None,
            dcs: Vec::new(),
        }
    }
}

impl Table {
    pub fn is_temp(&self) -> bool {
        matches!(self, Table::Named { path, args: None } if path.parts.len() == 1)
    }
}
