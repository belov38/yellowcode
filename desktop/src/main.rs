#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod console {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{
        Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, INVALID_HANDLE_VALUE},
        System::{
            Console::{
                ATTACH_PARENT_PROCESS, AttachConsole, GetConsoleMode, GetStdHandle,
                STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
            },
            Threading::GetCurrentProcess,
        },
    };

    pub fn attach() -> Vec<OwnedHandle> {
        let mut saved = Vec::new();
        for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let original = unsafe { GetStdHandle(which) };
            if original.is_null() || original == INVALID_HANDLE_VALUE {
                continue;
            }
            let mut mode = 0;
            if unsafe { GetConsoleMode(original, &mut mode) } != 0 {
                continue;
            }
            let mut duplicate: HANDLE = std::ptr::null_mut();
            if unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    original,
                    GetCurrentProcess(),
                    &mut duplicate,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                )
            } != 0
            {
                saved.push((which, unsafe { OwnedHandle::from_raw_handle(duplicate) }));
            } else {
                return Vec::new();
            }
        }
        unsafe {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
        saved
            .into_iter()
            .map(|(which, handle)| {
                unsafe {
                    SetStdHandle(which, handle.as_raw_handle());
                }
                handle
            })
            .collect()
    }
}

fn main() {
    use std::io::IsTerminal;
    let args: Vec<_> = std::env::args_os().collect();
    let code = if yellowcode_cli::is_cli(&args) {
        #[cfg(windows)]
        let _redirects = console::attach();
        let mut out = std::io::stdout();
        let tty = out.is_terminal();
        yellowcode_cli::run(
            args,
            &mut std::io::stdin(),
            &mut out,
            &mut std::io::stderr(),
            tty,
        )
    } else {
        match args
            .into_iter()
            .skip(1)
            .map(|arg| arg.into_string())
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(args) => onec_desktop::run_from_args(args),
            Err(_) => {
                eprintln!("yellowcode: GUI arguments must be valid Unicode");
                2
            }
        }
    };
    std::process::exit(code);
}
