use std::{
    cell::RefCell,
    io::{Read, Seek},
    path::Path,
};

use crate::{
    bak_config_source,
    bak_consistency::{Footprint, LogInput, LogIntegrity, ProofKind, Reason, Verdict, check_pages},
    bak_direct::{
        DirectOptions, DirectReport, Error, GateFacts, GateRefusal, ReadSeek, Scope,
        convert_cf_direct,
    },
    bak_pagemap::OpenOptions,
    bak_read::ReadSet,
    config_cf::CfOptions,
    media::MediaInfo,
};

pub const SERVER_MAJOR: u8 = 16;

pub fn idle_options(cf: CfOptions) -> DirectOptions {
    DirectOptions {
        cf,
        server_major: Some(SERVER_MAJOR),
        refuse_adr: true,
        idle_window_first: true,
        ..DirectOptions::default()
    }
}

pub(crate) fn window_first<R: Read + Seek>(
    container: &mut R,
    media: &MediaInfo,
) -> Result<(), Scope> {
    if let Some(header) = &media.header
        && header.software.major != SERVER_MAJOR
    {
        return Err(Scope::ServerVersion {
            found: Some((header.software.major, header.software.build)),
            expected: SERVER_MAJOR,
        });
    }
    let input = LogInput {
        header: media.header.as_ref(),
        regions: &media.log_regions,
        regions_complete: media.log_regions_complete,
    };
    let footprint = Footprint::new(&ReadSet::new(), &[], &[], &[]);
    let verdict = check_pages(container, &input, &footprint, &mut |_| {
        Err("no page has been read yet".to_owned())
    });
    idle_decision(&verdict)
}

pub fn idle_decision(verdict: &Verdict) -> Result<(), Scope> {
    match verdict {
        Verdict::Proven(proof) => match (proof.kind, proof.window.log_integrity()) {
            (ProofKind::Idle, LogIntegrity::Checksums) => Ok(()),
            (ProofKind::Idle, LogIntegrity::Structure) => Err(Scope::NoLogBlockChecksums),
            (ProofKind::Idle, LogIntegrity::NotRead) => Err(Scope::LogNotProven {
                what: "the log of the backup holds no block".to_owned(),
            }),
            (ProofKind::OutsideTheFootprint, _) => Err(Scope::LogWindowNotIdle {
                what: format!(
                    "{} records, none of them in what the reader read",
                    proof.window.scan.records
                ),
            }),
        },
        Verdict::Refused(refusal) => Err(match &refusal.reason {
            Reason::Records | Reason::BusyWindowWithoutChecksums => Scope::LogWindowNotIdle {
                what: refusal.to_string(),
            },
            _ => Scope::LogNotProven {
                what: refusal.to_string(),
            },
        }),
    }
}

fn gate_verdict(facts: &GateFacts<'_>) -> Verdict {
    let footprint = Footprint::new(
        facts.read_set,
        &facts.map.region2_extents,
        &facts.map.unmarked_region2_extents,
        &facts.map.uncopied_extents,
    )
    .with_structure(facts.map.structural_page_checks.unprotected);
    let input = LogInput {
        header: facts.backup.header.as_ref(),
        regions: &facts.backup.log_regions,
        regions_complete: facts.backup.log_regions_complete,
    };
    let mut container = facts.container.borrow_mut();
    let mut log: &mut dyn ReadSeek = &mut *container;
    check_pages(&mut log, &input, &footprint, &mut |page| {
        (facts.page_lsn)(page)
    })
}

fn idle_gate<'s>(
    refused: &'s RefCell<Option<Scope>>,
) -> impl FnOnce(&GateFacts<'_>) -> Result<(), GateRefusal> + 's {
    move |facts| {
        let verdict = gate_verdict(facts);
        match idle_decision(&verdict) {
            Ok(()) => {
                if let Verdict::Proven(proof) = verdict {
                    facts.outcome.record(proof);
                }
                Ok(())
            }
            Err(scope) => {
                let reason = scope.to_string();
                *refused.borrow_mut() = Some(scope);
                Err(GateRefusal { reason })
            }
        }
    }
}

pub fn convert_cf_idle(source: &Path, output: &Path, cf: CfOptions) -> Result<DirectReport, Error> {
    let refused = RefCell::new(None);
    match convert_cf_direct(source, output, &idle_options(cf), idle_gate(&refused)) {
        Err(Error::Gate(refusal)) => Err(match refused.into_inner() {
            Some(scope) => Error::Scope(scope),
            None => Error::Gate(refusal),
        }),
        other => other,
    }
}

pub const OLDEST_SERVER_MAJOR: u8 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Warn,
    Strict,
}

#[derive(Debug)]
#[non_exhaustive]
pub struct Outcome {
    pub report: DirectReport,
    pub warnings: Vec<Scope>,
    pub log_records: Option<u64>,
    pub proven: bool,
}

pub fn warn_options(cf: CfOptions) -> DirectOptions {
    DirectOptions {
        cf,
        min_server_major: Some(OLDEST_SERVER_MAJOR),
        open: OpenOptions::default()
            .accepting_unmarked_frames()
            .accepting_uncopied_extents(),
        config: bak_config_source::Options {
            accept_unreleased_versions: true,
            ..bak_config_source::Options::default()
        },
        ..DirectOptions::default()
    }
}

fn warnings_of(report: &DirectReport, verdict: Option<&Verdict>) -> Vec<Scope> {
    let mut warnings = Vec::new();
    let configured = report.backup.server_version;
    let logged = report
        .backup
        .header
        .as_ref()
        .map(|header| (header.software.major, header.software.build));
    if configured.map(|(major, _)| major) != Some(SERVER_MAJOR)
        || logged.is_some_and(|(major, _)| major != SERVER_MAJOR)
    {
        let found = configured
            .filter(|(major, _)| *major != SERVER_MAJOR)
            .or(logged.filter(|(major, _)| *major != SERVER_MAJOR))
            .or(configured);
        warnings.push(Scope::ServerVersion {
            found,
            expected: SERVER_MAJOR,
        });
    }
    if report.backup.accelerated_recovery || report.config.unreleased_rows > 0 {
        warnings.push(Scope::Adr);
    }
    match verdict.map(idle_decision) {
        Some(Ok(())) => {
            let unmarked = report.map.unmarked_region2_extents.len();
            let uncopied = report.map.uncopied_extents.len();
            if unmarked > 0 || uncopied > 0 {
                warnings.push(Scope::LogNotProven {
                    what: format!(
                        "the page map took {unmarked} extents copied a second time that only their page headers vouch for and {uncopied} extents the backup lacks"
                    ),
                });
            }
        }
        Some(Err(scope)) => warnings.push(scope),
        None => warnings.push(Scope::LogNotProven {
            what: "the consistency check did not run".to_owned(),
        }),
    }
    warnings
}

fn window_records(verdict: &Verdict) -> Option<u64> {
    let window = match verdict {
        Verdict::Proven(proof) => Some(&proof.window),
        Verdict::Refused(refusal) => refusal.window.as_ref(),
    }?;
    (window.scan.blocks > 0).then_some(window.scan.records)
}

pub fn convert_cf(
    source: &Path,
    output: &Path,
    cf: CfOptions,
    mode: Mode,
) -> Result<Outcome, Error> {
    if mode == Mode::Strict {
        let report = convert_cf_idle(source, output, cf)?;
        let log_records = report.gate.as_ref().map(|proof| proof.window.scan.records);
        return Ok(Outcome {
            report,
            warnings: Vec::new(),
            log_records,
            proven: true,
        });
    }
    let seen: RefCell<Option<Verdict>> = RefCell::new(None);
    let gate = |facts: &GateFacts<'_>| -> Result<(), GateRefusal> {
        let verdict = gate_verdict(facts);
        if let Verdict::Proven(proof) = &verdict {
            facts.outcome.record(proof.clone());
        }
        *seen.borrow_mut() = Some(verdict);
        Ok(())
    };
    let report = convert_cf_direct(source, output, &warn_options(cf), gate)?;
    let verdict = seen.into_inner();
    let warnings = warnings_of(&report, verdict.as_ref());
    let log_records = verdict.as_ref().and_then(window_records);
    Ok(Outcome {
        proven: warnings.is_empty(),
        report,
        warnings,
        log_records,
    })
}

pub fn unapplied_log_records(source: &Path) -> Option<u64> {
    let mut file = std::fs::File::open(source).ok()?;
    let media = crate::media::inspect_checked(&mut file).ok()?;
    let input = LogInput {
        header: media.header.as_ref(),
        regions: &media.log_regions,
        regions_complete: media.log_regions_complete,
    };
    let footprint = Footprint::new(&ReadSet::new(), &[], &[], &[]);
    let verdict = check_pages(&mut file, &input, &footprint, &mut |_| {
        Err("no page is read here".to_owned())
    });
    let idle = matches!(&verdict, Verdict::Proven(proof) if proof.kind == ProofKind::Idle);
    if idle {
        return None;
    }
    window_records(&verdict)
}
