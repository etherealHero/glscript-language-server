use async_lsp::lsp_types::request as R;
use async_lsp::{LanguageServer, lsp_types as lsp};

use crate::builder::Build;
use crate::proxy::{Error, Proxy, ResFut, forward_build_range};
use crate::types::SCRIPT_IDENTIFIER_PREFIX;
use crate::{try_ensure_bundle, try_ensure_transpile};

#[cfg_attr(feature = "profiling", tracing::instrument(skip_all))]
pub fn proxy_document_symbol(
    this: &mut Proxy,
    mut params: lsp::DocumentSymbolParams,
) -> ResFut<R::DocumentSymbolRequest> {
    let mut s = this.server();
    let uri = &params.text_document.uri;
    let transpile = try_ensure_transpile!(this, uri, params, document_symbol);
    let state = this.state.clone();
    let project = state.get_project().clone();

    params.text_document.uri = transpile.uri.clone();

    Box::pin(async move {
        match s.document_symbol(params).await.map_err(Error::internal) {
            Ok(Some(lsp::DocumentSymbolResponse::Nested(symbols))) => {
                let source_symbols = forward_nested_document_symbol(&Some(symbols), &transpile);
                let mut source_symbols = source_symbols.unwrap_or_default();
                source_symbols.sort_unstable_by_key(|s| s.range.start);
                Ok(Some(lsp::DocumentSymbolResponse::Nested(source_symbols)))
            }
            Ok(Some(lsp::DocumentSymbolResponse::Flat(build_symbols))) => Ok(Some({
                use rayon::prelude::*;

                let mut source_symbols = Vec::with_capacity(build_symbols.len());
                for s in build_symbols {
                    if s.name.starts_with(SCRIPT_IDENTIFIER_PREFIX) {
                        continue;
                    }
                    let mut range = s.location.range;
                    let Ok(source) = forward_build_range(&mut range, &transpile) else {
                        continue;
                    };
                    let uri = state.path_to_uri(&project.join(source.as_str())).unwrap();
                    let uri = (*uri).clone();
                    let location = lsp::Location::new(uri, range);
                    source_symbols.push(lsp::SymbolInformation { location, ..s });
                }

                source_symbols = source_symbols
                    .par_iter()
                    .enumerate()
                    .filter(|(idx, candidate)| {
                        let r = candidate.location.range;
                        let mut is_nested = false;
                        let mut has_children = false;
                        for (other_idx, other) in source_symbols.iter().enumerate() {
                            if *idx == other_idx {
                                continue;
                            }

                            let other_r = other.location.range;
                            if other_r.start <= r.start && other_r.end >= r.end {
                                is_nested = true;
                            }

                            if r.start <= other_r.start && r.end >= other_r.end {
                                has_children = true;
                            }

                            if is_nested && has_children {
                                break;
                            }
                        }
                        let lines = r.end.line.saturating_sub(r.start.line);
                        let looks_meaningful = lines > 1;
                        !is_nested || has_children || looks_meaningful
                    })
                    .map(|(_, s)| s.clone())
                    .collect();

                source_symbols.sort_unstable_by_key(|s| s.location.range.start);
                lsp::DocumentSymbolResponse::Flat(source_symbols)
            })),
            Ok(None) => Ok(None),
            Err(err) => Err(err),
        }
    })
}

fn forward_nested_document_symbol(
    build_symbols: &Option<Vec<lsp::DocumentSymbol>>,
    build: &Build,
) -> Option<Vec<lsp::DocumentSymbol>> {
    let build_symbols = match build_symbols {
        Some(build_symbols) => build_symbols,
        None => return None,
    };

    let mut source_symbols = Vec::with_capacity(build_symbols.len());

    for s in build_symbols {
        if s.name.starts_with(SCRIPT_IDENTIFIER_PREFIX) {
            continue;
        }

        let mut range = s.range;
        forward_build_range(&mut range, build).ok()?;
        let mut selection_range = s.selection_range;
        forward_build_range(&mut selection_range, build).ok()?;

        source_symbols.push(lsp::DocumentSymbol {
            children: forward_nested_document_symbol(&s.children, build),
            detail: s.detail.to_owned(),
            name: s.name.to_owned(),
            tags: s.tags.to_owned(),
            selection_range,
            range,
            ..*s
        });
    }

    Some(source_symbols)
}

#[cfg_attr(feature = "profiling", tracing::instrument(skip_all))]
pub fn proxy_workspace_symbol(
    this: &mut Proxy,
    params: lsp::WorkspaceSymbolParams,
) -> ResFut<R::WorkspaceSymbolRequest> {
    let query = params.query.trim().to_string();
    if query.is_empty() {
        return Box::pin(async move { Ok(None) });
    }

    let mut s = this.server();
    let state = this.state.clone();
    let uri = match state.get_current_doc() {
        Some(uri) => uri,
        None => return Box::pin(async move { Ok(None) }),
    };
    let bundle = try_ensure_bundle!(this, &uri, params, symbol);
    let project = state.get_project().clone();

    Box::pin(async move {
        let res = s
            .document_symbol(lsp::DocumentSymbolParams {
                text_document: lsp::TextDocumentIdentifier::new(bundle.uri.clone()),
                work_done_progress_params: lsp::WorkDoneProgressParams::default(),
                partial_result_params: lsp::PartialResultParams::default(),
            })
            .await
            .map_err(Error::internal);

        let forward = |build_symbols: Vec<lsp::DocumentSymbol>| {
            let mut buf = vec![];
            let matcher = &mut nucleo_matcher::Matcher::default();
            let pattern = nucleo_matcher::pattern::Pattern::parse(
                &query,
                nucleo_matcher::pattern::CaseMatching::Smart,
                nucleo_matcher::pattern::Normalization::Smart,
            );

            let mut source_symbols = Vec::with_capacity(build_symbols.len());

            for s in build_symbols {
                if s.name.starts_with(SCRIPT_IDENTIFIER_PREFIX) {
                    continue;
                }

                let mut range = s.range;
                let Ok(source) = forward_build_range(&mut range, &bundle) else {
                    continue;
                };

                let uri = state.path_to_uri(&project.join(source.as_str())).unwrap();
                let uri = (*uri).clone();
                let location = lsp::Location::new(uri, range);
                let haystack = nucleo_matcher::Utf32Str::new(&s.name, &mut buf);
                let Some(score) = pattern.score(haystack, matcher) else {
                    continue;
                };

                source_symbols.push((
                    match s.name.starts_with(&query) {
                        true => score + 1000,
                        false => score,
                    },
                    lsp::SymbolInformation {
                        container_name: None,
                        #[allow(deprecated)]
                        deprecated: None,
                        name: s.name,
                        kind: s.kind,
                        tags: s.tags,
                        location,
                    },
                ));
            }

            source_symbols.sort_unstable_by_key(|b| std::cmp::Reverse(b.0));
            source_symbols.truncate(100);
            let source_symbols = source_symbols.into_iter().map(|(_, s)| s).collect();

            Ok(Some(lsp::WorkspaceSymbolResponse::Flat(source_symbols)))
        };

        match res {
            Ok(Some(lsp::DocumentSymbolResponse::Nested(build_symbols))) => forward(build_symbols),
            Ok(Some(lsp::DocumentSymbolResponse::Flat(build_symbols))) => forward(
                build_symbols
                    .into_iter()
                    .map(|s| lsp::DocumentSymbol {
                        name: s.name,
                        kind: s.kind,
                        tags: s.tags,
                        range: s.location.range,
                        selection_range: s.location.range,
                        #[allow(deprecated)]
                        deprecated: s.deprecated,
                        children: None,
                        detail: None,
                    })
                    .collect::<Vec<_>>(),
            ),
            Ok(None) => Ok(None),
            Err(err) => Err(err),
        }
    })
}
