use async_lsp::lsp_types::request as R;
use async_lsp::{LanguageServer, lsp_types as lsp};

use crate::proxy::language_server::{Error, NotifyResult};
use crate::proxy::{Proxy, ResFut};
use crate::{try_ensure_build, try_forward_text_document_position_params};

pub fn proxy_signature_help(
    this: &mut Proxy,
    mut params: lsp::SignatureHelpParams,
) -> ResFut<R::SignatureHelpRequest> {
    let mut s = this.server();
    let uri = &params.text_document_position_params.text_document.uri;
    let build = try_ensure_build!(this, uri, params, signature_help);
    let state = this.state.clone();
    Box::pin(async move {
        let doc_pos = &mut params.text_document_position_params;
        try_forward_text_document_position_params!(state, build, doc_pos);
        s.signature_help(params).await.map_err(Error::internal)
    })
}

pub fn proxy_cancel_request(this: &mut Proxy) -> NotifyResult {
    this.state.cancel_received.store(true);
    std::ops::ControlFlow::Continue(())
}
