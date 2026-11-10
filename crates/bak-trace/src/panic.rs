use crate::{notice, trace::Trace};
use std::{io::Write as _, panic::PanicHookInfo, sync::Once};

pub(crate) fn trace_for_panic() -> Option<Trace> {
    Trace::current().or_else(Trace::sole_active)
}

pub fn install_panic_hook() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            previous(info);
            report(info);
        }));
    });
}

fn report(info: &PanicHookInfo<'_>) {
    let Some(trace) = trace_for_panic() else {
        return;
    };
    let message = if let Some(text) = info.payload().downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = info.payload().downcast_ref::<String>() {
        text.clone()
    } else {
        String::new()
    };
    let (file, line, column) = info
        .location()
        .map_or(("", 0, 0), |at| (at.file(), at.line(), at.column()));
    let finished = trace.finish_panic(&message, file, line, column);
    if let Some(path) = finished.path {
        let _ = writeln!(std::io::stderr(), "{}", notice::failure_written(&path));
    }
}
