use std::{io::Write, net::SocketAddr, path::PathBuf};
use yellowcode_core::service::memory;
use yellowcode_http::service::serve::{self, Config};

#[derive(clap::Args)]
pub(crate) struct Args {
    #[command(flatten)]
    memory: memory::Settings,
    /// Immutable infobase copy: directory or its 1Cv8.1CD file, opened once read-only.
    base: PathBuf,
    /// HTTP address (infrastructure owns access control and TLS).
    #[arg(long, default_value = "127.0.0.1:8080")]
    listen: SocketAddr,
}

pub(crate) fn run(
    args: Args,
    platform_build: String,
    err: &mut dyn Write,
) -> yellowcode_core::Result<()> {
    serve::run(
        Config {
            platform_build,
            memory: args.memory,
            base: args.base,
            listen: args.listen,
        },
        err,
    )
}
