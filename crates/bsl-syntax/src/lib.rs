#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
#![forbid(unsafe_code)]

pub mod ast;
pub mod diag;
mod dump;
mod fold;
mod keywords;
pub mod lexer;
pub mod parser;
pub mod preproc;
pub mod source;

pub use diag::{MsgId, SyntaxError};
pub use dump::{dump_expr, dump_module};
pub use fold::fold_ident;
pub use lexer::{Kw, Lexed, Token, TokenKind, tokenize};
pub use parser::{
    BuiltinSig, BuiltinTable, NoBuiltins, ParseMode, ParseOptions, ParseOutput, parse,
};
pub use preproc::{ExecContext, Origin, PreprocSymbol, PreprocSymbols, Preprocessed, preprocess};
pub use source::{Pos, SourceText, Span};
