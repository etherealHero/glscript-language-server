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

#[tracing::instrument(skip_all)]
pub fn proxy_folding_range(
    this: &mut Proxy,
    mut params: lsp::FoldingRangeParams,
) -> ResFut<R::FoldingRangeRequest> {
    let mut s = this.server();
    let uri = &params.text_document.uri;
    try_ensure_build!(this, uri, params, folding_range);
    let state = this.state.clone();
    let req_uri = params.text_document.uri.clone();
    let get_range = |f: &lsp::FoldingRange, text: &str| {
        let start_ch = || text.lines().next().unwrap_or_default().len() as u32;
        let end_ch = || text.lines().last().unwrap_or_default().len() as u32;
        lsp::Range::new(
            lsp::Position::new(f.start_line, f.start_character.unwrap_or_else(start_ch)),
            lsp::Position::new(f.end_line, f.end_character.unwrap_or_else(end_ch)),
        )
    };

    params.text_document.uri = state.get_active_transpiled_buffer();

    Box::pin(async move {
        let transpiled_doc = &state.transpile_doc(&req_uri).unwrap();
        let changes = state.set_active_transpiled_buffer(&transpiled_doc.content);

        s.did_change(changes).unwrap();

        let mut res = s.folding_range(params).await.map_err(Error::internal);
        if let Ok(Some(ref mut foldings)) = res {
            for f in foldings {
                let mut range = get_range(f, &transpiled_doc.content);
                forward_build_range(&mut range, transpiled_doc).unwrap();
                f.start_line = range.start.line;
                f.start_character = range.start.character.into();
                f.end_line = range.end.line;
                f.end_character = range.end.character.into();
            }
        }
        res
    })
}
