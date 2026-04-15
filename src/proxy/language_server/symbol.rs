use async_lsp::lsp_types::request as R;
use async_lsp::{LanguageServer, lsp_types as lsp};

use crate::builder::Build;
use crate::proxy::{Error, Proxy, ResFut, forward_build_range};
use crate::types::SCRIPT_IDENTIFIER_PREFIX;
use crate::{try_ensure_bundle, try_ensure_transpile};

pub fn proxy_document_symbol(
    this: &mut Proxy,
    mut params: lsp::DocumentSymbolParams,
) -> ResFut<R::DocumentSymbolRequest> {
    let mut s = this.server();
    let uri = &params.text_document.uri;
    let transpile = try_ensure_transpile!(this, uri, params, document_symbol);

    params.text_document.uri = transpile.uri.clone();

    Box::pin(async move {
        match s.document_symbol(params).await.map_err(Error::internal) {
            Ok(Some(lsp::DocumentSymbolResponse::Nested(symbols))) => {
                let source_symbols = forward_document_symbol(&Some(symbols), &transpile);
                let source_symbols = source_symbols.unwrap_or_default();
                Ok(Some(lsp::DocumentSymbolResponse::Nested(source_symbols)))
            }
            Ok(Some(lsp::DocumentSymbolResponse::Flat(s))) => match s.is_empty() {
                true => Ok(Some(lsp::DocumentSymbolResponse::Flat(s))),
                false => Err(Error::forward_failed()),
            },
            Ok(None) => Ok(None),
            Err(err) => Err(err),
        }
    })
}

fn forward_document_symbol(
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
            children: forward_document_symbol(&s.children, build),
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

pub fn proxy_workspace_symbol(
    this: &mut Proxy,
    params: lsp::WorkspaceSymbolParams,
) -> ResFut<R::WorkspaceSymbolRequest> {
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

        match res {
            Ok(Some(lsp::DocumentSymbolResponse::Nested(symbols))) => {
                let mut source_symbols = Vec::with_capacity(symbols.len());

                for s in symbols {
                    if s.name.starts_with(SCRIPT_IDENTIFIER_PREFIX) {
                        continue;
                    }

                    let mut range = s.range;
                    let Ok(source) = forward_build_range(&mut range, &bundle) else {
                        continue;
                    };

                    let location = lsp::Location::new(
                        state.path_to_uri(&project.join(source.as_str())).unwrap(),
                        range,
                    );

                    source_symbols.push(lsp::SymbolInformation {
                        container_name: None,
                        #[allow(deprecated)]
                        deprecated: None,
                        name: s.name,
                        kind: s.kind,
                        tags: s.tags,
                        location,
                    });
                }

                Ok(Some(lsp::WorkspaceSymbolResponse::Flat(source_symbols)))
            }
            Ok(Some(lsp::DocumentSymbolResponse::Flat(s))) => match s.is_empty() {
                true => Ok(Some(lsp::WorkspaceSymbolResponse::Flat(vec![]))),
                false => Err(Error::forward_failed()),
            },
            Ok(None) => Ok(None),
            Err(err) => Err(err),
        }
    })
}
