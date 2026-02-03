use async_lsp::lsp_types::request as R;
use async_lsp::{LanguageServer, lsp_types as lsp};

use crate::builder::Build;
use crate::proxy::language_server::{Error, forward_build_range};
use crate::proxy::{Proxy, ResFut};
use crate::try_ensure_transpile;
use crate::types::SCRIPT_IDENTIFIER_PREFIX;

#[tracing::instrument(skip_all)]
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
                let source_symbols = forward(&Some(symbols), &transpile);
                let source_symbols = source_symbols.unwrap_or_default();
                Ok(Some(lsp::DocumentSymbolResponse::Nested(source_symbols)))
            }
            Ok(Some(lsp::DocumentSymbolResponse::Flat(s))) if !s.is_empty() => {
                Err(Error::forward_failed())
            }
            Ok(res) => Ok(res),
            Err(err) => Err(err),
        }
    })
}

fn forward(
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
            children: forward(&s.children, build),
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
