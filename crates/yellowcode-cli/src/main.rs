use std::io::IsTerminal;

#[global_allocator]
static ANALYSIS_HEAP: yellowcode_core::viewer::analysis_worker::Heap =
    yellowcode_core::viewer::analysis_worker::Heap::new();

fn main() {
    let mut internal_args = std::env::args_os().skip(1);
    if internal_args.next().as_deref()
        == Some(std::ffi::OsStr::new(
            yellowcode_core::viewer::analysis_worker::INTERNAL_SWITCH,
        ))
    {
        if internal_args.next().is_some() {
            std::process::exit(74);
        }
        let code =
            unsafe { yellowcode_core::viewer::analysis_worker::run_at_startup(&ANALYSIS_HEAP) };
        std::process::exit(code);
    }

    let mut out = std::io::stdout();
    let tty = out.is_terminal();
    let code = yellowcode_cli::run(
        std::env::args_os(),
        &mut std::io::stdin(),
        &mut out,
        &mut std::io::stderr(),
        tty,
    );
    yellowcode_cli::exit(code);
}
