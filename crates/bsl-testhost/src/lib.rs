#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
#![forbid(unsafe_code)]

mod env;
mod http;
mod kinds;
mod repr;
mod runner;
mod scan;
mod stand;
pub mod wrapper;
mod xml;

pub use env::StandEnv;
pub use http::{new_request, register_http, register_no_transactions, response_parts};
pub use kinds::module_kind_by_path;
pub use repr::repr;
pub use runner::{EVAL_MODULE, RunResult, RunnerError, VmRunner, stand_object};
pub use scan::{ConfigScan, ModuleFile, ScanError};
pub use stand::StandHost;
pub use wrapper::STAND_GLOBAL;
