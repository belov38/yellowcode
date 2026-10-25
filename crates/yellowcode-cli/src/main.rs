use std::io::IsTerminal;

fn main() {
    let mut out = std::io::stdout();
    let tty = out.is_terminal();
    let code = yellowcode_cli::run(
        std::env::args_os(),
        &mut std::io::stdin(),
        &mut out,
        &mut std::io::stderr(),
        tty,
    );
    std::process::exit(code);
}
