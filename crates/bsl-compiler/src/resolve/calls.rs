use super::expr::{Build, Task, push_args};
use super::{
    PARAMS_NOTENOUGH, PARAMS_TOOMANY, PROC_ASFUNC, PROC_DEFINED, PROC_NOTDEFINED, Resolver,
};
use crate::app::AppMethodSig;
use crate::bytecode::MethodCode;
use crate::ir::Callee;
use crate::split::MethodRole;
use crate::sym::{Interner, Sym};
use bsl_syntax::Span;
use bsl_syntax::ast::{Arg, Ident, Method, MethodKind};
use platform_catalog::GlobalMethod;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Arity {
    pub is_function: Option<bool>,
    pub min: usize,
    pub max: Option<usize>,
}

impl Arity {
    pub(crate) const UNCHECKED: Arity = Arity {
        is_function: None,
        min: 0,
        max: None,
    };

    pub(crate) fn of_method(m: &Method) -> Arity {
        Arity {
            is_function: Some(m.kind == MethodKind::Function),
            min: m
                .params
                .iter()
                .rposition(|p| p.default.is_none())
                .map_or(0, |i| i + 1),
            max: Some(m.params.len()),
        }
    }

    pub(crate) fn of_code(m: &MethodCode) -> Arity {
        Arity {
            is_function: Some(m.kind == MethodKind::Function),
            min: m
                .params
                .iter()
                .rposition(|p| p.default.is_none())
                .map_or(0, |i| i + 1),
            max: Some(m.params.len()),
        }
    }

    pub(crate) fn of_app(sig: &AppMethodSig) -> Arity {
        Arity {
            is_function: Some(sig.is_function),
            min: sig
                .params
                .iter()
                .rposition(|p| !p.has_default)
                .map_or(0, |i| i + 1),
            max: Some(sig.params.len()),
        }
    }

    pub(crate) fn of_platform(g: &GlobalMethod) -> Arity {
        let (min, max) = if g.arity_compile {
            (usize::from(g.min_args), g.max_args.map(usize::from))
        } else {
            (0, None)
        };
        Arity {
            is_function: Some(g.is_function),
            min,
            max,
        }
    }
}

#[derive(Default)]
pub(crate) struct Methods(HashMap<Sym, (u32, Arity)>);

impl Methods {
    pub(crate) fn build<'m>(
        methods: &[(&'m Method, MethodRole)],
        interner: &mut Interner,
    ) -> (Methods, Vec<&'m Ident>) {
        let mut table = HashMap::new();
        let mut dups = Vec::new();
        for (i, (m, _)) in methods.iter().enumerate() {
            let sym = interner.sym(&m.name.name);
            if table.contains_key(&sym) {
                dups.push(&m.name);
                continue;
            }
            let i = u32::try_from(i).unwrap_or(u32::MAX);
            table.insert(sym, (i, Arity::of_method(m)));
        }
        (Methods(table), dups)
    }

    pub(crate) fn get(&self, name: &Sym) -> Option<(u32, Arity)> {
        self.0.get(name).copied()
    }
}

impl Resolver<'_> {
    pub(super) fn report_duplicates(&mut self, dups: &[&Ident]) {
        for id in dups {
            self.check(PROC_DEFINED, id);
        }
    }

    pub(super) fn visit_call<'e>(
        &mut self,
        id: &Ident,
        args: &'e [Arg],
        want: bool,
        span: Span,
        tasks: &mut Vec<Task<'e>>,
    ) {
        if self.is_evaluate(id) {
            match args.first().and_then(|a| a.value.as_ref()) {
                Some(x) => {
                    tasks.extend([Task::Build(Build::Evaluate { span }), Task::Expr(x, true)])
                }
                None => tasks.push(Task::Build(Build::Hole { n: 0, span })),
            }
            return;
        }
        let sym = self.interner.sym(&id.name);
        let found = match self.methods.get(&sym) {
            Some((i, arity)) => Some((Callee::Local(i), arity)),
            None => self.globals.method(id, &sym),
        };
        let n = args.len();
        let build = match found {
            Some((callee, arity)) => {
                self.check_call(id, args, arity, want);
                Build::Call {
                    callee,
                    n,
                    want_value: want,
                    span,
                }
            }
            None => {
                let e = self.error(PROC_NOTDEFINED, id, id.span.start);
                self.unknown.push(e);
                Build::Hole { n, span }
            }
        };
        tasks.push(Task::Build(build));
        push_args(tasks, args);
    }

    fn is_evaluate(&self, id: &Ident) -> bool {
        self.platform
            .catalog
            .builtin(&id.name)
            .is_some_and(|g| g.info.name_ru == "Вычислить")
    }

    fn check_call(&mut self, id: &Ident, args: &[Arg], a: Arity, want_value: bool) {
        match a.max.and_then(|max| args.get(max)) {
            Some(extra) => self.check_at(PARAMS_TOOMANY, id, extra.span.start),
            None if args.len() < a.min => self.check(PARAMS_NOTENOUGH, id),
            None => {}
        }
        if want_value && a.is_function == Some(false) {
            self.check(PROC_ASFUNC, id);
        }
    }
}
