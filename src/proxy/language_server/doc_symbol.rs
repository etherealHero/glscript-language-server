use std::str::FromStr;

use async_lsp::lsp_types::{Url as Uri, request as R};
use async_lsp::{LanguageServer, lsp_types as lsp};

use crate::builder::Build;
use crate::proxy::language_server::{Error, did_close, did_open_once, forward_build_range};
use crate::proxy::{Proxy, ResFut};
use crate::try_ensure_build;
use crate::types::{SCRIPT_IDENTIFIER_PREFIX, Source};

// TODO: client send req twice (ignore second req)
#[tracing::instrument(skip_all)]
pub fn proxy_document_symbol(
    this: &mut Proxy,
    mut params: lsp::DocumentSymbolParams,
) -> ResFut<R::DocumentSymbolRequest> {
    let mut s = this.server();
    let uri = &params.text_document.uri;
    try_ensure_build!(this, uri, params, document_symbol);
    let state = this.state.clone();
    let req_uri = params.text_document.uri.clone();
    let req_source = state.get_doc(&req_uri).unwrap().source;
    let temp_uri = Uri::from_str("file:///.virtual/transpiled.js").unwrap();

    params.text_document.uri = temp_uri.clone();

    Box::pin(async move {
        let transpiled_doc = &state.transpile_doc(&req_uri).unwrap();

        did_open_once(&mut s, &temp_uri, &transpiled_doc.content)?;
        let res = match s.document_symbol(params).await.map_err(Error::internal) {
            Ok(Some(lsp::DocumentSymbolResponse::Nested(symbols))) => {
                let source_symbols = forward(&Some(symbols), transpiled_doc, &req_source);
                let source_symbols = source_symbols.unwrap_or_default();
                Ok(Some(lsp::DocumentSymbolResponse::Nested(source_symbols)))
            }
            Ok(Some(_)) => Err(Error::forward_failed()),
            Ok(res) => Ok(res),
            Err(err) => Err(err),
        };

        did_close(&mut s, &temp_uri)?;
        res
    })
}

fn forward(
    build_symbols: &Option<Vec<lsp::DocumentSymbol>>,
    build: &Build,
    source: &Source,
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
        let rs = forward_build_range(&mut range, build).ok()?;
        let mut selection_range = s.selection_range;
        let srs = forward_build_range(&mut selection_range, build).ok()?;

        if &rs == source && &srs == source {
            source_symbols.push(lsp::DocumentSymbol {
                children: forward(&s.children, build, source),
                detail: s.detail.to_owned(),
                name: s.name.to_owned(),
                tags: s.tags.to_owned(),
                selection_range,
                range,
                ..*s
            });
        }
    }

    Some(source_symbols)
}
