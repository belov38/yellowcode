#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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

    let code = match std::env::args_os()
        .skip(1)
        .map(|arg| arg.into_string())
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(args) => onec_desktop::run_from_args(args),
        Err(_) => {
            eprintln!("yellowcode-desktop: arguments must be valid Unicode");
            2
        }
    };
    std::process::exit(code);
}
