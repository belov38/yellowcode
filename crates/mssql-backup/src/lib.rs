#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]
pub mod config_cf;
pub mod media;
pub mod page_source;
pub mod pages;
pub mod records;
pub mod sql_blob;
pub mod sql_catalog;
pub mod sql_config;
pub mod sql_config_spool;
pub mod sql_dt;
pub mod sql_folder;
pub mod sql_inventory;
pub mod sql_json_row;
pub mod sql_metadata;
pub mod sql_rows;
pub mod sql_schema_wire;
pub mod sql_server;
pub mod sql_support;
pub mod sql_users;
pub mod sql_values;
pub mod sqlcmd;
