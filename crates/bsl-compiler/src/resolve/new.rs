use super::expr::{Build, Task, push_args};
use super::{CTOR_NOTFOUND, NEW_NOTOBJECT, Resolver, TYPE_NOTDEFINED};
use bsl_syntax::Span;
use bsl_syntax::ast::{Arg, Ident};
use platform_catalog::{Signature, TypeIdx, TypeInfo, TypeKind};

fn required(s: &Signature) -> usize {
    s.params
        .iter()
        .rposition(|p| !p.optional)
        .map_or(0, |i| i + 1)
}

fn constructible(info: &TypeInfo, argc: usize, on_server: bool) -> bool {
    if argc == 0 {
        info.server_compile
            .filter(|_| on_server)
            .map(|s| s.new_no_args_compiles)
            .or(info.new_no_args)
            .unwrap_or_else(|| info.constructors.iter().any(|s| required(s) == 0))
    } else {
        info.constructors.iter().any(|s| required(s) <= argc)
    }
}

impl Resolver<'_> {
    pub(super) fn visit_new<'e>(
        &mut self,
        type_name: &Ident,
        args: Option<&'e [Arg]>,
        span: Span,
        tasks: &mut Vec<Task<'e>>,
    ) {
        let n = args.map_or(0, <[Arg]>::len);
        let build = match self.new_type(type_name, n) {
            Some(ty) => Build::New {
                ty,
                n: args.map(<[Arg]>::len),
                span,
            },
            None => Build::Hole { n, span },
        };
        tasks.push(Task::Build(build));
        if let Some(args) = args {
            push_args(tasks, args);
        }
    }

    fn new_type(&mut self, name: &Ident, argc: usize) -> Option<TypeIdx> {
        let Some(t) = self.platform.new_type(&name.name) else {
            self.check(TYPE_NOTDEFINED, name);
            return None;
        };
        let info = self.platform.catalog.type_info(t);
        if info.kind == TypeKind::Primitive {
            self.check_bare(NEW_NOTOBJECT, name.span.start);
            None
        } else if constructible(info, argc, self.platform.on_server()) {
            Some(t)
        } else {
            self.check_bare(CTOR_NOTFOUND, name.span.start);
            None
        }
    }
}
