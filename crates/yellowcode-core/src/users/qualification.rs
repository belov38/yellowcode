use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

pub(super) fn source_binding() -> String {
    let sources: &[&[u8]] = &[
        include_bytes!("mutation.rs"),
        include_bytes!("snapshot.rs"),
        include_bytes!("source.rs"),
        include_bytes!("qualification.rs"),
        include_bytes!("../users.rs"),
        include_bytes!("du2u.rs"),
        include_bytes!("metadata.rs"),
        include_bytes!("write.rs"),
        include_bytes!("../../../onecd/src/lock.rs"),
        include_bytes!("../../../onecd/src/pager/storage.rs"),
        include_bytes!("../../../v8users/src/data.rs"),
        include_bytes!("../../../v8users/src/user.rs"),
        include_bytes!("../../../v8users/src/edit.rs"),
        include_bytes!("../../../v8users/src/roles.rs"),
    ];
    let mut digest = Sha256::new();
    for source in sources {
        digest.update((source.len() as u64).to_le_bytes());
        digest.update(source);
    }
    format!("{:x}", digest.finalize())
}

fn hex(value: Option<&str>, length: usize) -> bool {
    value.is_some_and(|value| {
        value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}
fn accepted(receipt: &Value, platform: &str, binding: &str) -> bool {
    let Some(row) = receipt.get(platform) else {
        return false;
    };
    if receipt["schema"] != 2
        || row["status"] != "accepted"
        || row["platform"] != platform
        || row["scope"] != serde_json::json!(["clear_password"])
    {
        return false;
    }
    if platform == "windows" {
        return row["stage"] == "DU2a"
            && row["observations"] == 76
            && row["run_id"] == "users-clear-windows-r15-fix-1"
            && row["report_sha256"]
                == "9eced8c1505e4e9b71f66d8bb8f5ddcfc55c663228b1f8d208d6c9794206487b";
    }
    matches!(platform, "linux" | "macos")
        && row["stage"] == "DU2u"
        && (row["source_binding"].as_str() == Some(binding)
            || row["exported_source_binding"].as_str() == Some(binding))
        && hex(row["request_sha256"].as_str(), 64)
        && row["native_logins"] == 22
        && row["native_integrity"] == 4
        && row["opened_audit"] == "PASS"
        && row["run_id"] == "users-clear-unix-r2"
        && row["report_sha256"]
            == "94aee83ae9a8299315f5f6aca07477bfc7f64e26bf64881f1c3ce64d126249c8"
}
pub(super) fn public_clear() -> bool {
    static ADMITTED: OnceLock<bool> = OnceLock::new();
    *ADMITTED.get_or_init(|| {
        let Ok(receipt) = serde_json::from_str(include_str!("clear_password_qualification.json"))
        else {
            return false;
        };
        accepted(&receipt, std::env::consts::OS, &source_binding())
    })
}
