use crate::builtins::CatalogBuiltins;
use crate::bytecode::CompiledModule;
use crate::error::CompileError;
use crate::ir::IrModule;
use crate::lower::lower;
use crate::options::CompileOptions;
use crate::resolve::{Globals, Resolved, resolve, select_error};
use crate::split::{parse_options, view};
use bsl_syntax::{MsgId, SourceText, SyntaxError, ast, parse};
use std::sync::Arc;

pub fn compile(
    src: &SourceText,
    o: &CompileOptions<'_>,
) -> Result<Arc<CompiledModule>, CompileError> {
    compile_ir(src, o).map(|m| Arc::new(lower(m)))
}

pub fn compile_ir(src: &SourceText, o: &CompileOptions<'_>) -> Result<IrModule, CompileError> {
    let builtins = CatalogBuiltins(o.catalog);
    let out = parse(src, &parse_options(o, &builtins));
    let v = view(&out.module, o.kind, o.context);
    let syntax = syntax_phase(src, out.error.as_ref(), v.dup_var);
    let r = match syntax {
        None => resolve(src, o, &v, &mut Globals::new(o)),
        Some(_) => Resolved::skipped(o),
    };
    match select_error(syntax, &r.checks, &r.unknown) {
        Some(e) => Err(e),
        None => Ok(r.module),
    }
}

fn syntax_phase(
    src: &SourceText,
    parser: Option<&SyntaxError>,
    dup_var: Option<&ast::Ident>,
) -> Option<CompileError> {
    let dup = dup_var.map(|n| {
        let p = src.pos(n.span.start);
        CompileError {
            msg: MsgId("IDS_STX_VAR_DEFINED"),
            arg: Some(n.name.clone()),
            line: p.line,
            col: p.col,
        }
    });
    let syntax = parser.map(CompileError::from_syntax);
    match (syntax, dup) {
        (Some(s), Some(d)) if (d.line, d.col) < (s.line, s.col) => Some(d),
        (Some(e), _) | (None, Some(e)) => Some(e),
        (None, None) => None,
    }
}
