use async_lsp::lsp_types::request as R;
use async_lsp::{LanguageServer, lsp_types as lsp};

use crate::builder::BUILD_FILE_EXT;
use crate::proxy::Canonicalize;
use crate::proxy::language_server::{did_close, did_open};
use crate::proxy::{JS_LANG_ID, Proxy, ResFut, language_server::NotifyResult};
use crate::try_ensure_bundle;

pub fn proxy_did_open(this: &mut Proxy, params: lsp::DidOpenTextDocumentParams) -> NotifyResult {
    let s = &mut this.server();
    let doc = &params.text_document;
    if doc.language_id == JS_LANG_ID && !doc.uri.as_str().ends_with(BUILD_FILE_EXT) {
        let res = this.state.set_doc(
            &doc.uri,
            &[lsp::TextDocumentContentChangeEvent {
                text: doc.text.clone(),
                range_length: None,
                range: None,
            }],
        );

        if res.is_err() {
            let _ = s.did_open(params);
            return std::ops::ControlFlow::Continue(());
        };

        let b = this.state.set_bundle(&doc.uri).unwrap();
        let t = this.state.set_transpile(&doc.uri).unwrap();

        if this.state.is_diagnostics_enabled() {
            std::fs::write(b.build.uri.to_file_path().unwrap(), "").unwrap();
        }

        let _ = did_open(s, &b.build.uri, &b.build.content, b.version.into());
        let _ = did_open(s, &t.build.uri, &t.build.content, t.version.into());
    } else {
        let _ = s.did_open(params);
    }

    std::ops::ControlFlow::Continue(())
}

#[tracing::instrument(skip_all)]
pub fn proxy_did_change(
    this: &mut Proxy,
    params: lsp::DidChangeTextDocumentParams,
) -> NotifyResult {
    let uri = &params.text_document.uri;
    let st = this.state.clone();
    let mut service = this.server();

    if st.get_bundle(uri).is_none() {
        let _ = service.did_change(params);
        return std::ops::ControlFlow::Continue(());
    }

    let doc = st.get_doc(uri).unwrap();
    let hash_prev = doc.transpile_hash;

    // 1. apply changes to raw document
    st.set_doc(uri, &params.content_changes).unwrap();
    let hash_new = st.get_doc(uri).unwrap().transpile_hash;
    let transpile_changed = hash_prev != hash_new;

    // 2. forward params into language server
    let bundles = st.get_bundles_contains_source(&doc.source);
    for doc_path in bundles {
        let params = params.clone();
        st.add_changes(doc_path, params, transpile_changed);
    }

    // 3. commit req doc
    st.commit_changes(uri, &mut service);
    std::ops::ControlFlow::Continue(())
}

pub fn proxy_did_save(this: &mut Proxy, params: lsp::DidSaveTextDocumentParams) -> NotifyResult {
    if this.state.get_bundle(&params.text_document.uri).is_none() {
        let _ = this.server().did_save(params);
    }
    std::ops::ControlFlow::Continue(())
}

pub fn proxy_did_close(this: &mut Proxy, params: lsp::DidCloseTextDocumentParams) -> NotifyResult {
    let uri = &params.text_document.uri;
    let Some(bundle) = this.state.get_bundle(uri) else {
        this.server().did_close(params).expect("did close");
        return std::ops::ControlFlow::Continue(());
    };

    let _ = did_close(&mut this.server(), &bundle.uri);

    if this.state.is_diagnostics_enabled() {
        let _ = std::fs::remove_file(bundle.uri.to_file_path().unwrap());
    }

    this.state.remove_bundle(uri);

    let Some(doc) = this.state.get_transpile(uri) else {
        return std::ops::ControlFlow::Continue(());
    };

    let _ = did_close(&mut this.server(), &doc.uri);

    this.state.remove_transpile(uri);
    std::ops::ControlFlow::Continue(())
}

pub fn proxy_did_change_watched_files(
    this: &mut Proxy,
    mut params: lsp::DidChangeWatchedFilesParams,
) -> NotifyResult {
    let mut forward_changes = vec![];
    for channge in params.changes {
        let is_build_file = !channge.uri.as_str().ends_with(BUILD_FILE_EXT);
        let is_build_dep = this.state.get_bundle(&channge.uri).is_some(); // TODO: ???

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
    try_ensure_bundle!(this, uri, params, code_lens);
    Box::pin(async move { Ok(Some(vec![])) }) // TODO:
}
