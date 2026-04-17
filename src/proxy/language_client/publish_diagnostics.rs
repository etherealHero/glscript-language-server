use std::collections::{HashMap, HashSet};

use async_lsp::LanguageClient;
use async_lsp::lsp_types::{self as lsp};
use derive_more::Constructor;

use crate::builder::EMIT_FILE_EXT;
use crate::proxy::{DECL_FILE_EXT, Error, NotifyResult, Proxy, forward_build_range};
use crate::types::{SCRIPT_IDENTIFIER_PREFIX, Source};

#[cfg_attr(feature = "profiling", tracing::instrument(skip_all))]
pub fn proxy_publish_diagnostics(
    this: &mut Proxy,
    params: lsp::PublishDiagnosticsParams,
) -> NotifyResult {
    use rayon::prelude::*;

    let mut client = this.client();
    let state = this.state.clone();

    if state.get_transpile(&params.uri).is_some() {
        return std::ops::ControlFlow::Continue(());
    }

    let Some(doc_build) = state.get_any_build_by_emit_uri(&params.uri) else {
        tracing::info!("{}", Error::unbuild_fallback());
        let _ = client.publish_diagnostics(params);
        return std::ops::ControlFlow::Continue(());
    };

    let doc = state.get_doc_by_emit_uri(&params.uri).unwrap();
    let project = state.get_project();

    let mut source_diagnostics: Vec<lsp::Diagnostic> = Vec::new();
    type NS = lsp::NumberOrString;

    let forwarded_diagnostics: Vec<_> = params.diagnostics.into_par_iter().filter_map(|d| {
            let mut range = d.range;
            let Ok(source_of_diagnostic) = forward_build_range(&mut range, &doc_build) else {
                tracing::warn!("{}", Error::forward_failed());
                return None;
            };

            type DS = lsp::DiagnosticSeverity;
            let severity = if let Some(code) = d.code.as_ref().map(|c| match c {
                NS::Number(id) => id.to_string(),
                NS::String(id) => id.clone(),
            }) {
                // https://typescript.tv/errors/
                if source_of_diagnostic != *doc.source && code.as_str() != "2300" /* duplicate identifier */ {
                    return None;
                }

                match code.as_str() {
                    // "7006" /* any type */ => return None,
                    "2300" /* duplicate identifier */ if d.message.contains(SCRIPT_IDENTIFIER_PREFIX) => return None,
                    "80002" /* recommend class decl */ => return None,
                    "2304" /* cannot find name */ => Some(DS::WARNING),
                    "2364" /* assignment err */ => Some(DS::ERROR),
                    "2551" /* similar ident */ => Some(DS::INFORMATION),
                    c if c.len() == 4 && c.starts_with("1") => Some(DS::ERROR), // syntactic errors
                    _ => Some(DS::HINT),
                }
            } else {
                None
            };

            let related_information = if let Some(related_information) = d.related_information {
                let mut source_related_information = Vec::with_capacity(related_information.len());
                for ri in related_information {
                    if ri.location.uri.as_str().ends_with(EMIT_FILE_EXT) {
                        continue;
                    }

                    if let Some(any_build) = state.get_any_build_by_emit_uri(&ri.location.uri) {

                        if state.get_doc_by_emit_uri(&any_build.uri).is_some_and(|any_doc| any_doc.source != doc.source) {
                            continue; // ignore related information from other builds
                        }

                        let doc_build = any_build;
                        let mut source_ri_range = ri.location.range;
                        let Ok(ri_source) = forward_build_range(&mut source_ri_range, &doc_build) else {
                            tracing::error!("forward build range failed on related_information iteration");
                            continue;
                        };

                        let source_uri = state.path_to_uri(&project.join(ri_source.as_str())).unwrap();

                        source_related_information.push(lsp::DiagnosticRelatedInformation {
                            location: lsp::Location::new(source_uri, source_ri_range),
                            message: ri.message,
                        });

                        continue;
                    }

                    if ri.location.uri.as_str().ends_with(DECL_FILE_EXT) {
                        source_related_information.push(ri);
                        continue;
                    }

                    tracing::warn!("unforwarded related_information `{ri:#?}`"); // TODO:
                }
                Some(source_related_information)
            } else {
                None
            };

            let source_diagnostic = lsp::Diagnostic {
                related_information,
                severity,
                range,
                ..d
            };

            DiagnosticEntry::new(source_of_diagnostic, source_diagnostic).into()
        }).collect();

    let mut duplicates_map: HashMap<String, Vec<DiagnosticEntry>> = HashMap::new();

    for entry in forwarded_diagnostics {
        let is_duplicate = entry.diagnostic.code.as_ref().is_some_and(|c| {
            matches!(c, NS::String(s) if s == "2300") || matches!(c, NS::Number(2300))
        });

        match is_duplicate {
            true => duplicates_map
                .entry(entry.diagnostic.message.clone())
                .or_default()
                .push(entry),
            false => source_diagnostics.push(entry.diagnostic),
        }
    }

    for (message, diagnostic_group) in duplicates_map.into_iter() {
        let mut doc_diagnostics = vec![];
        let duplicates_all_locations = diagnostic_group
            .iter()
            .map(|entry| {
                let source_path = project.join(entry.source.as_str());
                let source_uri = state.path_to_uri(&source_path).unwrap();
                lsp::Location::new(source_uri, entry.diagnostic.range)
            })
            .collect::<Vec<_>>();

        let loc_to_ri = |location| lsp::DiagnosticRelatedInformation {
            message: message.clone(),
            location,
        };

        let dts_ri = {
            let mut dts_rl = HashSet::<lsp::Location>::default();

            for entry in &diagnostic_group {
                if let Some(related_information) = &entry.diagnostic.related_information {
                    for ri in related_information.iter().cloned() {
                        dts_rl.insert(ri.location);
                    }
                }
            }

            dts_rl.iter().cloned().map(loc_to_ri).collect::<Vec<_>>()
        };

        for (i, entry) in diagnostic_group.into_iter().enumerate() {
            if entry.source == *doc.source {
                let duplicates_ri = duplicates_all_locations
                    .iter()
                    .cloned()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .map(|(_, location)| loc_to_ri(location))
                    .collect::<Vec<_>>();

                if !duplicates_ri.is_empty() || !dts_ri.is_empty() {
                    let mut entry_ri = vec![];

                    entry_ri.extend(duplicates_ri);
                    entry_ri.extend(dts_ri.clone());

                    doc_diagnostics.push(lsp::Diagnostic {
                        related_information: Some(entry_ri),
                        ..entry.diagnostic
                    });
                }
            }
        }

        source_diagnostics.extend(doc_diagnostics);
    }

    client
        .publish_diagnostics(lsp::PublishDiagnosticsParams::new(
            state.path_to_uri(&doc.path).unwrap(),
            source_diagnostics,
            None,
        ))
        .unwrap();

    std::ops::ControlFlow::Continue(())
}

#[derive(Constructor, Debug)]
struct DiagnosticEntry {
    source: Source,
    diagnostic: lsp::Diagnostic,
}
