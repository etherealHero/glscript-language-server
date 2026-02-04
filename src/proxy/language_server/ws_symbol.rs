use async_lsp::lsp_types::request as R;
use async_lsp::{LanguageServer, lsp_types as lsp};

use crate::proxy::language_server::{Error, forward_build_range};
use crate::proxy::{Proxy, ResFut};
use crate::try_ensure_bundle;

#[tracing::instrument(skip_all)]
pub fn proxy_symbol(
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

    Box::pin(async move {
        let project = state.get_project();
        let res = s.symbol(params).await.map_err(Error::internal);

        match res {
            Ok(Some(lsp::WorkspaceSymbolResponse::Flat(symbols))) => {
                let mut source_symbols = Vec::with_capacity(symbols.len());
                for s in symbols {
                    let mut source_range = s.location.range;
                    if let Ok(source) = forward_build_range(&mut source_range, &bundle) {
                        let mut source_symbol = s.clone();
                        let path = &project.join(source.as_str());
                        source_symbol.location.uri = state.path_to_uri(path).unwrap();
                        source_symbol.location.range = source_range;
                        source_symbols.push(source_symbol);
                    }
                }
                Ok(Some(lsp::WorkspaceSymbolResponse::Flat(source_symbols)))
            }
            Ok(Some(_)) => Err(Error::forward_failed()),
            Ok(res) => Ok(res),
            Err(err) => Err(err),
        }
    })
}

#[tracing::instrument(skip_all)]
pub fn proxy_workspace_symbol_resolve(
    _this: &mut Proxy,
    params: lsp::WorkspaceSymbol,
) -> ResFut<R::WorkspaceSymbolResolve> {
    Box::pin(async move { Ok(params) })
}
