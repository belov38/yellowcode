use query_syntax::QueryError;

#[derive(Debug)]
pub enum OpenError {
    Syntax(QueryError),
    Semantic(QueryError),
    Shape { stmt: usize, what: Shape },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    IntoNotFirst,
    IndexNotLast,
    UnionWidth,
    SubqueryAlias,
    DcsOrder,
}
