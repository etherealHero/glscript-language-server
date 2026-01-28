use async_lsp::lsp_types::request as R;
use async_lsp::{LanguageServer, lsp_types as lsp};

use crate::builder::BUILD_FILE_EXT;
use crate::proxy::Canonicalize;
use crate::proxy::language_server::did_close;
use crate::proxy::{JS_LANG_ID, Proxy, ResFut, language_server::NotifyResult};
use crate::try_ensure_build;

pub fn proxy_did_open(this: &mut Proxy, params: lsp::DidOpenTextDocumentParams) -> NotifyResult {
    let doc = &params.text_document;
    if doc.language_id == JS_LANG_ID && !doc.uri.as_str().ends_with(BUILD_FILE_EXT) {
        this.state
            .set_doc(
                &doc.uri,
                &[lsp::TextDocumentContentChangeEvent {
                    text: doc.text.clone(),
                    range_length: None,
                    range: None,
                }],
            )
            .unwrap();
        let build_with_version = this.state.set_build(&doc.uri).unwrap();

        std::fs::write(build_with_version.build.uri.to_file_path().unwrap(), "").unwrap();

        let _ = this.server().did_open(lsp::DidOpenTextDocumentParams {
            text_document: lsp::TextDocumentItem::new(
                build_with_version.build.uri.clone(),
                JS_LANG_ID.into(),
                build_with_version.version,
                build_with_version.build.content.clone(),
            ),
        });
    } else {
        let _ = this.server().did_open(params);
    }

    std::ops::ControlFlow::Continue(())
}

#[tracing::instrument(skip_all)]
pub fn proxy_did_change(
    this: &mut Proxy,
    params: lsp::DidChangeTextDocumentParams,
) -> NotifyResult {
    let uri = &params.text_document.uri;
    let mut service = this.server();

    if this.state.get_build(uri).is_none() {
        let _ = service.did_change(params);
        return std::ops::ControlFlow::Continue(());
    }

    let doc = this.state.get_doc(uri).unwrap();
    let hash_prev = doc.transpile_hash;

    // 1. apply changes to raw document
    this.state.set_doc(uri, &params.content_changes).unwrap();
    let hash_new = this.state.get_doc(uri).unwrap().transpile_hash;
    let transpile_changed = hash_prev != hash_new;

    // 2. forward params into language server
    let builds_contains_source = this.state.get_builds_contains_source(&doc.source);
    for doc_of_build_path in builds_contains_source {
        let params = params.clone();
        this.state
            .add_client_doc_changes(doc_of_build_path, params, transpile_changed);
    }

    // 3. commit req doc
    this.state.commit_build_changes(uri, &mut service);
    std::ops::ControlFlow::Continue(())
}

pub fn proxy_did_save(this: &mut Proxy, params: lsp::DidSaveTextDocumentParams) -> NotifyResult {
    if this.state.get_build(&params.text_document.uri).is_none() {
        let _ = this.server().did_save(params);
    }
    std::ops::ControlFlow::Continue(())
}

pub fn proxy_did_close(this: &mut Proxy, params: lsp::DidCloseTextDocumentParams) -> NotifyResult {
    let uri = &params.text_document.uri;
    if let Some(build) = this.state.get_build(uri) {
        let _ = did_close(&mut this.server(), &build.uri);
        let _ = std::fs::remove_file(build.uri.to_file_path().unwrap());
        this.state.remove_build(uri);
    } else {
        this.server().did_close(params).expect("did close")
    }
    std::ops::ControlFlow::Continue(())
}

pub fn proxy_did_change_watched_files(
    this: &mut Proxy,
    mut params: lsp::DidChangeWatchedFilesParams,
) -> NotifyResult {
    let mut forward_changes = vec![];
    for channge in params.changes {
        let is_build_file = !channge.uri.as_str().ends_with(BUILD_FILE_EXT);
        let is_build_dep = this.state.get_build(&channge.uri).is_some();

        if is_build_file || is_build_dep {
            continue;
        }

        forward_changes.push(channge);
    }

    if forward_changes.is_empty() {
        return std::ops::ControlFlow::Continue(());
    }

    params.changes = forward_changes;

    let _ = this.server().did_change_watched_files(params);
    std::ops::ControlFlow::Continue(())
}

pub fn proxy_sync_doc_by_code_lens_request(
    this: &mut Proxy,
    params: lsp::CodeLensParams,
) -> ResFut<R::CodeLensRequest> {
    let uri = &params.text_document.uri;
    let state = this.state.clone();
    if state.get_current_doc() != Some(uri.try_canonicalize()) {
        state.set_current_doc(uri);
    }
    try_ensure_build!(this, uri, params, code_lens);
    Box::pin(async move { Ok(Some(vec![])) }) // TODO:
}
