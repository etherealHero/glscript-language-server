use std::collections::HashMap;

use async_lsp::lsp_types::request as R;
use async_lsp::{LanguageServer, lsp_types as lsp};

use crate::proxy::language_server::{Error, NotifyResult, forward_build_range, references_params};
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

pub fn proxy_cancel_request(this: &mut Proxy, _: lsp::CancelParams) -> NotifyResult {
    this.state.cancel_received.store(true);
    std::ops::ControlFlow::Continue(())
}

pub fn proxy_rename(this: &mut Proxy, params: lsp::RenameParams) -> ResFut<R::Rename> {
    let uri = &params.text_document_position.text_document.uri;
    let pos = params.text_document_position.position;
    try_ensure_build!(this, uri, params, rename);
    let references_request = this.references(references_params(uri.clone(), pos));
    Box::pin(async move {
        let refs = references_request.await;
        if let Ok(Some(locations)) = refs {
            let mut ws_edit = lsp::WorkspaceEdit {
                changes: Some(HashMap::with_capacity(locations.len())),
                document_changes: None,
                change_annotations: None,
            };
            let edits = ws_edit.changes.as_mut().unwrap();
            for loc in locations {
                let edit = || lsp::TextEdit::new(loc.range, params.new_name.clone());
                edits
                    .entry(loc.uri)
                    .and_modify(|e| e.push(edit()))
                    .or_insert(vec![edit()]);
            }
            Ok(Some(ws_edit))
        } else {
            Ok(None)
        }
    })
}

pub fn proxy_prepare_rename(
    this: &mut Proxy,
    mut params: lsp::TextDocumentPositionParams,
) -> ResFut<R::PrepareRenameRequest> {
    let mut s = this.server();
    let uri = &params.text_document.uri;
    let build = try_ensure_build!(this, uri, params, prepare_rename);
    let state = this.state.clone();
    Box::pin(async move {
        try_forward_text_document_position_params!(state, build, params);
        let mut res = s.prepare_rename(params).await.map_err(Error::internal);
        if let Ok(Some(lsp::PrepareRenameResponse::Range(ref mut r))) = res {
            forward_build_range(r, &build)?;
        }
        res
    })
}
