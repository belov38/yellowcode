use std::time::Instant;

use onecd_testkit::{SampleDb, TORN, all_sample_dbs, qa_sample_dbs};

use crate::collate::IcuCollate;
use crate::format::index_page::{IndexPage, encode_branch, encode_leaf};
use crate::format::key::{KeyCtx, build_key, index_key_len, key_slot_size, split_key};
use crate::index::IndexRead;
use crate::table::TableRead;
use crate::verify::{TreeNode, last_pair, link_ok, tree_levels};
use crate::{Database, Locking, OpenOptions};

#[derive(Default)]
struct Counters {
    files: usize,
    tables: usize,
    indexes: usize,
    empty_indexes: usize,
    leaves: usize,
    branches: usize,
    entries: u64,
    parts: u64,
    string_parts: u64,
    string_entries: u64,
    over_slot: u64,
    keys_equal: u64,
    compared: u64,
    leaf_exact: usize,
    leaf_off: usize,
    dirty_gaps: usize,
    branch_exact: usize,
    branch_off: usize,
    torn_dead_rows: u64,
    torn_separators: u64,
    max_depth: usize,
}

type Level = Vec<(u32, IndexPage)>;

fn ptrs(p: &IndexPage) -> (u32, u32, bool) {
    match p {
        IndexPage::Leaf(l) => (l.prev, l.next, l.root),
        IndexPage::Branch(b) => (b.prev, b.next, b.root),
    }
}

fn levels(ix: &IndexRead) -> std::result::Result<Vec<Level>, String> {
    let (levels, bad) = tree_levels(ix).map_err(|e| e.to_string())?;
    if let Some(e) = bad.first() {
        return Err(e.to_string());
    }
    levels
        .into_iter()
        .map(|level| {
            level
                .into_iter()
                .map(|(no, node)| match node {
                    TreeNode::Branch(b) => Ok((no, IndexPage::Branch(b))),
                    TreeNode::Leaf => ix.decode(no).map(|p| (no, p)).map_err(|e| e.to_string()),
                })
                .collect()
        })
        .collect()
}

fn check_index(t: &TableRead, ix: &IndexRead, torn: bool, c: &mut Counters) -> Vec<String> {
    let mut bad = Vec::new();
    let d = t.descr();
    let idx = ix.descr();
    let kl = ix.key_len();
    match index_key_len(d, idx) {
        Ok(want) if want == kl => {}
        other => bad.push(format!("key length {kl}, computed {other:?}")),
    }
    let levels = match levels(ix) {
        Ok(l) => l,
        Err(e) => return vec![e],
    };
    c.max_depth = c.max_depth.max(levels.len());
    match ix.depth() {
        Ok(depth) if depth as usize + 1 == levels.len() || levels.len() == 1 && depth == 0 => {}
        other => bad.push(format!("depth {other:?}, {} levels", levels.len())),
    }
    let ps = t.format_info().page_size;

    for level in &levels {
        for (i, (no, page)) in level.iter().enumerate() {
            let (prev, next, root) = ptrs(page);
            if root != (*no == ix.root_page()) {
                bad.push(format!("page {no}: root flag {root}"));
            }
            let before = i.checked_sub(1).and_then(|j| level.get(j)).map(|x| x.0);
            let after = level.get(i + 1).map(|x| x.0);
            if !link_ok(ix, *no, prev, before) || !link_ok(ix, *no, next, after) {
                bad.push(format!(
                    "page {no}: links {prev:#x}/{next:#x}, wanted {before:?}/{after:?}"
                ));
            }
        }
    }

    for level in &levels {
        for (no, page) in level {
            let Ok(raw) = ix.raw_page(*no) else {
                bad.push(format!("page {no} unreadable"));
                continue;
            };
            let mut expect = raw.to_vec();
            let encoded = match page {
                IndexPage::Leaf(l) => {
                    c.leaves += 1;
                    let at = |off: usize| {
                        expect
                            .get(off..off + 2)
                            .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])))
                    };
                    let free = at(12).unwrap_or(0);
                    let start = 30 + l.entries.len() * usize::from(l.packing.recbytes);
                    if let Some(gap) = expect.get_mut(start..start + free) {
                        c.dirty_gaps += usize::from(gap.iter().any(|&b| b != 0));
                        gap.fill(0);
                    }
                    encode_leaf(l, kl, ps)
                }
                IndexPage::Branch(b) => {
                    c.branches += 1;
                    encode_branch(b, kl, ps)
                }
            };
            let is_leaf = matches!(page, IndexPage::Leaf(_));
            match encoded {
                Some(enc) if enc == expect => {
                    if is_leaf {
                        c.leaf_exact += 1;
                    } else {
                        c.branch_exact += 1;
                    }
                }
                other => {
                    if is_leaf {
                        c.leaf_off += 1;
                    } else {
                        c.branch_off += 1;
                    }
                    let first = other
                        .as_ref()
                        .and_then(|e| e.iter().zip(&expect).position(|(a, b)| a != b));
                    bad.push(format!(
                        "page {no} ({}): re-encoding {} (first difference at {first:?})",
                        if is_leaf { "leaf" } else { "branch" },
                        if other.is_some() { "differs" } else { "fails" },
                    ));
                }
            }
        }
    }

    for level in &levels {
        for (no, page) in level {
            let IndexPage::Branch(b) = page else { continue };
            for e in &b.entries {
                let child = ix
                    .page_of(e.child, *no)
                    .ok()
                    .and_then(|n| ix.decode(n).ok());
                let last = child.as_ref().and_then(last_pair);
                if last == Some((e.key.as_slice(), e.row)) {
                    continue;
                }
                if torn {
                    c.torn_separators += 1;
                } else {
                    bad.push(format!(
                        "page {no}: separator {} does not close its child {}",
                        e.row, e.child
                    ));
                }
            }
        }
    }

    let cx = KeyCtx {
        collate: Some(&IcuCollate),
        locale: t.key_ctx().locale,
    };
    let Some(leaves) = levels.last() else {
        return bad;
    };
    let mut entries = 0u64;
    for (_, page) in leaves {
        let IndexPage::Leaf(l) = page else { continue };
        for (row, key) in &l.entries {
            entries += 1;
            if torn && !matches!(t.get(*row), Ok(Some(_))) {
                c.torn_dead_rows += 1;
                continue;
            }
            let rec = match t.raw_record(*row) {
                Ok(r) => r,
                Err(e) => {
                    bad.push(format!("row {row}: {e}"));
                    continue;
                }
            };
            let compared = (|| -> std::result::Result<Vec<String>, String> {
                let stored = split_key(d, idx, key).map_err(|e| e.to_string())?;
                let built =
                    build_key(d, t.layout(), idx, &rec, kl, &cx).map_err(|e| e.to_string())?;
                let exp = split_key(d, idx, &built).map_err(|e| e.to_string())?;
                c.compared += 1;
                let mut diffs = Vec::new();
                let mut has_string = false;
                for ((fld, s), e) in idx.fields.iter().zip(&stored).zip(&exp) {
                    let f = d.field(&fld.name).map(|(_, f)| f);
                    let is_string = f.is_some_and(|f| f.ty.is_string());
                    if is_string {
                        has_string = true;
                        c.string_parts += 1;
                    } else {
                        c.parts += 1;
                    }
                    if key.get(s.clone()) != built.get(e.clone()) {
                        diffs.push(format!(
                            "row {row}: field {}: key part {:02x?} != {:02x?}",
                            fld.name,
                            key.get(s.clone()),
                            built.get(e.clone())
                        ));
                    } else if let (true, Some(f)) = (is_string, f) {
                        let slot = key_slot_size(f, fld.prefix_len).map_err(|e| e.to_string())?;
                        let part = key.get(s.clone()).unwrap_or_default();
                        let part = part.get(usize::from(f.nullable)..).unwrap_or_default();
                        c.over_slot += u64::from(
                            part.len() == slot as usize && part.last().is_some_and(|&b| b != 0),
                        );
                    }
                }
                c.string_entries += u64::from(has_string);
                if key == &built {
                    c.keys_equal += 1;
                } else if diffs.is_empty() {
                    diffs.push(format!("row {row}: the keys differ in the padding"));
                }
                Ok(diffs)
            })();
            match compared {
                Ok(diffs) if diffs.is_empty() => {}
                Ok(diffs) => bad.extend(diffs),
                Err(e) => bad.push(format!("row {row}: {e}")),
            }
        }
    }
    c.entries += entries;
    if entries == 0 {
        c.empty_indexes += 1;
    }
    bad
}

fn check_db(db: &SampleDb, c: &mut Counters) -> Vec<String> {
    let torn = TORN.contains(&db.rel.as_str());
    let opts = OpenOptions {
        locking: Locking::None,
        ..OpenOptions::read_only()
    };
    let database = match Database::open(&db.path, opts) {
        Ok(d) => d,
        Err(e) => return vec![format!("open: {e}")],
    };
    c.files += 1;
    let txn = database.read();
    let mut bad = Vec::new();
    for name in txn.table_names() {
        let t = match txn.table(&name) {
            Ok(t) => t,
            Err(e) => {
                bad.push(format!("{name}: {e}"));
                continue;
            }
        };
        c.tables += 1;
        for idx in &t.descr().indexes {
            c.indexes += 1;
            let ix = match t.index(&idx.name) {
                Ok(ix) => ix,
                Err(e) => {
                    bad.push(format!("{name}.{}: open: {e}", idx.name));
                    continue;
                }
            };
            let probs = check_index(&t, &ix, torn, c);
            let shown: Vec<String> = probs.iter().take(5).cloned().collect();
            bad.extend(
                shown
                    .into_iter()
                    .map(|p| format!("{name}.{}: {p}", idx.name)),
            );
            if probs.len() > 5 {
                bad.push(format!("{name}.{}: ... {} problems", idx.name, probs.len()));
            }
        }
    }
    bad
}

fn check_all(dbs: &[SampleDb]) -> (Counters, Vec<String>) {
    let mut c = Counters::default();
    let mut failures = Vec::new();
    for db in dbs {
        let bad = check_db(db, &mut c);
        failures.extend(bad.into_iter().map(|m| format!("{}: {m}", db.rel)));
    }
    (c, failures)
}

fn report(what: &str, c: &Counters, started: Instant) {
    eprintln!(
        "{what}: {} files, {} tables, {} indexes ({} empty), {} leaves ({} re-encode exactly, {} do not), \
         {} leaves with a non-zero unused gap; {} branches ({} exactly, {} not), {} entries, \
         deepest tree {} levels; torn copies: {} entries of rows that are not live, {} separators off; \
         keys: {} entries compared, {} equal in every byte; {} entries with string parts, \
         {} non-string and {} string parts compared, {} string parts cut at the slot; {:.1?}",
        c.files,
        c.tables,
        c.indexes,
        c.empty_indexes,
        c.leaves,
        c.leaf_exact,
        c.leaf_off,
        c.dirty_gaps,
        c.branches,
        c.branch_exact,
        c.branch_off,
        c.entries,
        c.max_depth,
        c.torn_dead_rows,
        c.torn_separators,
        c.compared,
        c.keys_equal,
        c.string_entries,
        c.parts,
        c.string_parts,
        c.over_slot,
        started.elapsed()
    );
}

fn assert_clean(c: &Counters, failures: &[String]) {
    assert!(
        failures.is_empty(),
        "{} problems:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert_eq!(c.keys_equal, c.compared);
}

#[test]
fn corpus_index_pages_keys_and_leaf_codec() {
    let dbs = all_sample_dbs();
    if dbs.is_empty() {
        return;
    }
    let started = Instant::now();
    let (c, failures) = check_all(&dbs);
    report("corpus_index", &c, started);
    assert_clean(&c, &failures);
    assert_eq!(c.files, dbs.len());
    assert!(c.leaves > 0 && c.parts > 0);
    assert!(c.string_entries > 0 && c.string_parts > 0);
    assert_eq!(c.compared, c.entries - c.torn_dead_rows);
    assert_eq!(c.over_slot, 0);
}

#[test]
fn qa_samples_keys_with_icu() {
    let dbs = qa_sample_dbs();
    if dbs.is_empty() {
        return;
    }
    for want in ["q1_tx200", "q2_strings"] {
        assert!(dbs.iter().any(|d| d.rel == want), "{want} is missing");
    }
    let started = Instant::now();
    let (c, failures) = check_all(&dbs);
    report("corpus_index (QA samples)", &c, started);
    assert_clean(&c, &failures);
    assert_eq!(c.files, dbs.len());
    assert!(c.string_entries > 0);
    let (q2, failures) = check_all(&[dbs.iter().find(|d| d.rel == "q2_strings").unwrap().clone()]);
    assert!(failures.is_empty(), "{failures:?}");
    assert!(q2.over_slot >= 4, "{} parts cut at the slot", q2.over_slot);
}
