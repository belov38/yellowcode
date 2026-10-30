use super::{Error, PaidPath};
#[path = "../../../yellowcode-core/tests/support/allocation_accounting.rs"]
mod accounting;
use accounting::{Counting, Observation, Sampling};
use memory_budget::{BudgetError, Category, Limits, MemoryBudget};
use std::{
    ffi::OsString,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
};

#[global_allocator]
static ALLOCATOR: Counting = Counting;
const CAPACITY: u64 = 8 << 20;

struct Case {
    name: &'static str,
    base: PathBuf,
    part: &'static str,
}
fn case(name: &'static str, base: &str, part: &'static str) -> Case {
    Case {
        name,
        base: PathBuf::from(base),
        part,
    }
}
fn cases() -> Vec<Case> {
    let mut deep: Vec<_> = r"\\?\C:\Йё".encode_utf16().collect();
    for _ in 0..128 {
        deep.extend(r"\Йё-0123456789".encode_utf16());
    }
    let mut surrogate_unc: Vec<_> = r"\\?\UNC\server\share\Йё".encode_utf16().collect();
    surrogate_unc.extend([0xd800, 0x41, 92]);
    surrogate_unc.extend("folder".encode_utf16());
    let mut values = vec![
        case("drive", r"\\?\C:\resources", "8.5.1.15290"),
        case("unc", r"\\?\UNC\server\share\resources", "catalog.bin"),
        case("drive-prefix-only", r"\\?\C:", "platform"),
        case("drive-root", r"\\?\C:\", "platform"),
        case("unc-prefix-only", r"\\?\UNC\server\share", "categories.tsv"),
        case("generic-prefix-only", r"\\?\Volume{1234}", "platform"),
        case("base-dots", r"\\?\C:\root\.\middle\..\Йё", "catalog.bin"),
        case("part-dot", r"\\?\C:\root\Йё", "."),
        case("part-parent", r"\\?\C:\root\Йё", ".."),
        case("part-parents", r"\\?\C:\root\Йё", r"..\next\..\catalog.bin"),
        case("root-parent", r"\\?\C:\", ".."),
        case("rooted-part", r"\\?\C:\root\Йё", r"\newroot\..\leaf"),
        case("absolute-part", r"\\?\C:\root\Йё", r"D:\replacement"),
        case("drive-relative-part", r"\\?\C:\root\Йё", r"D:replacement"),
        case("empty-part", r"\\?\C:\root\Йё", ""),
        case(
            "trailing-separators",
            r"\\?\UNC\server\share\Йё\\",
            "resources",
        ),
        case("ordinary-drive-control", r"C:\Йё", "catalog.bin"),
    ];
    values.push(Case {
        name: "deep-unicode-drive",
        base: PathBuf::from(OsString::from_wide(&deep)),
        part: "resources",
    });
    values.push(Case {
        name: "unicode-surrogate-unc",
        base: PathBuf::from(OsString::from_wide(&surrogate_unc)),
        part: "messages.tsv",
    });
    values
}
#[derive(Debug)]
enum Outcome {
    Joined {
        matches: bool,
        capacity: u64,
        paid: u64,
    },
    MetadataRefusal,
    UnexpectedFailure,
}
fn observe(case: &Case, expected: &Path, budget: &MemoryBudget) -> (Outcome, Observation) {
    let baseline = budget.current_bytes();
    let sampling = Sampling::start(budget);
    let outcome = match PaidPath::join(&case.base, case.part, budget) {
        Ok(joined) => {
            let matches = joined.value.as_os_str() == expected.as_os_str();
            let capacity = joined.value.capacity() as u64;
            let paid = budget.current_bytes() - baseline;
            drop(joined);
            Outcome::Joined {
                matches,
                capacity,
                paid,
            }
        }
        Err(error) => {
            let refused = matches!(
                error,
                Error::Memory(BudgetError::Limit(limit)) if limit.category == Category::Metadata
            );
            drop(error);
            if refused {
                Outcome::MetadataRefusal
            } else {
                Outcome::UnexpectedFailure
            }
        }
    };
    let observed = sampling.finish();
    assert_eq!(budget.current_bytes(), baseline, "join admission leaked");
    assert_eq!(
        observed.ending_live_delta, 0,
        "join storage leaked: {observed}"
    );
    assert_eq!(
        observed.ending_paid_delta, 0,
        "join owner leaked: {observed}"
    );
    (outcome, observed)
}
fn matches_and_paid(outcome: &Outcome) -> bool {
    matches!(outcome, Outcome::Joined { matches: true, capacity, paid } if paid >= capacity)
}

fn verbatim_join_has_no_unpaid_peak_and_owns_returned_capacity() {
    let cases = cases();
    let budget = MemoryBudget::try_new(Limits::new(CAPACITY)).unwrap();
    let mut failures = 0;
    for case in &cases {
        let mut expected = case.base.clone();
        expected.push(case.part);
        let (outcome, observed) = observe(case, &expected, &budget);
        eprintln!("join {}: outcome={outcome:?}; [{observed}]", case.name);
        failures += usize::from(observed.unpaid != 0 || !matches_and_paid(&outcome));
    }
    assert_eq!(failures, 0, "direct join accounting/capacity failures");
}

fn verbatim_join_exact_destination_pressure_refuses_or_stays_paid_and_retries() {
    let cases = cases();
    let budget = MemoryBudget::try_new(Limits::new(CAPACITY)).unwrap();
    let baseline = budget.current_bytes();
    let mut failures = 0;
    for case in &cases {
        let mut expected = case.base.clone();
        expected.push(case.part);
        let destination = case
            .base
            .as_os_str()
            .len()
            .checked_add(case.part.len())
            .and_then(|n| n.checked_add(2))
            .unwrap() as u64;
        budget.set_limits(Limits::new(baseline));
        let (refused, empty) = observe(case, &expected, &budget);
        assert!(matches!(refused, Outcome::MetadataRefusal));
        assert_eq!(empty.peak, 0, "zero-cap refusal allocated: {empty}");
        assert_eq!(empty.unpaid, 0);
        budget.set_limits(Limits::new(baseline + destination));
        let (limited, observed) = observe(case, &expected, &budget);
        eprintln!(
            "limited join {}: cap={destination} outcome={limited:?}; [{observed}]",
            case.name
        );
        let valid = matches!(limited, Outcome::MetadataRefusal) || matches_and_paid(&limited);
        failures += usize::from(observed.unpaid != 0 || !valid);
        budget.set_limits(Limits::new(CAPACITY));
        let (retry, observed) = observe(case, &expected, &budget);
        eprintln!("retry join {}: outcome={retry:?}; [{observed}]", case.name);
        failures += usize::from(observed.unpaid != 0 || !matches_and_paid(&retry));
    }
    assert_eq!(failures, 0, "join exact-capacity/refusal/retry failures");
}

const JOIN_CASE: &str = "PLATFORM_CATALOG_JOIN_CASE";
fn in_child(case: &str) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "native::join_memory_tests::join_memory_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(JOIN_CASE, case)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{case}: {stderr}");
    assert!(
        stderr.contains("JOIN CASE DONE"),
        "{case} did not run: {stderr}"
    );
}
#[test]
fn join_memory_child() {
    match std::env::var(JOIN_CASE).as_deref() {
        Ok("capacity") => verbatim_join_has_no_unpaid_peak_and_owns_returned_capacity(),
        Ok("pressure") => {
            verbatim_join_exact_destination_pressure_refuses_or_stays_paid_and_retries()
        }
        _ => return,
    }
    eprintln!("JOIN CASE DONE");
}
#[test]
fn windows_verbatim_join_has_no_unpaid_peak_and_owns_returned_capacity() {
    in_child("capacity");
}
#[test]
fn windows_verbatim_join_exact_destination_pressure_refuses_or_stays_paid_and_retries() {
    in_child("pressure");
}
