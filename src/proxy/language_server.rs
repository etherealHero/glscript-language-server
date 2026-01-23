use async_lsp::lsp_types::{Url as Uri, notification as N, request as R};
use async_lsp::router::Router;
use async_lsp::{ErrorCode, LanguageServer, ResponseError, lsp_types as lsp};

use crate::builder::Build;
use crate::proxy::{DECL_FILE_EXT, JS_FILE_EXT, Proxy, ResFut};
use crate::types::Source;

mod common_features;
mod completion;
mod definition;
mod doc_sync;
mod hover;
mod lifecycle;
mod references;

pub type NotifyResult = std::ops::ControlFlow<async_lsp::Result<()>>;
pub type DefRes = lsp::GotoDefinitionResponse;

pub struct Error {}

impl Error {
    pub fn internal(e: impl core::fmt::Display) -> ResponseError {
        ResponseError::new(ErrorCode::INTERNAL_ERROR, e)
    }

    pub fn request_failed(e: impl core::fmt::Display) -> ResponseError {
        ResponseError::new(ErrorCode::REQUEST_FAILED, e)
    }

    pub fn unexpected_source() -> ResponseError {
        let reason = "Missmatched source extension";
        let err = format!("{reason}, expect '{JS_FILE_EXT}' or '{DECL_FILE_EXT}'. Request aborted");
        ResponseError::new(ErrorCode::REQUEST_FAILED, err)
    }
}

pub fn forward_build_range(range: &mut lsp::Range, build: &Build) -> Result<Source, ResponseError> {
    let source_range = build.forward_build_range(range);
    if source_range.is_none() {
        let err = format!("Forward back build range `{:?}` failed", range);
        return Err(Error::request_failed(err));
    }
    let source_range = source_range.expect("is some");
    *range = source_range.0;
    Ok(source_range.1)
}

pub fn definition_params(uri: Uri, pos: lsp::Position) -> lsp::GotoDefinitionParams {
    lsp::GotoDefinitionParams {
        text_document_position_params: lsp::TextDocumentPositionParams::new(
            lsp::TextDocumentIdentifier::new(uri),
            pos,
        ),
        work_done_progress_params: lsp::WorkDoneProgressParams::default(),
        partial_result_params: lsp::PartialResultParams::default(),
    }
}

pub fn references_params(uri: Uri, pos: lsp::Position) -> lsp::ReferenceParams {
    lsp::ReferenceParams {
        text_document_position: lsp::TextDocumentPositionParams::new(
            lsp::TextDocumentIdentifier::new(uri),
            pos,
        ),
        work_done_progress_params: lsp::WorkDoneProgressParams::default(),
        partial_result_params: lsp::PartialResultParams::default(),
        context: lsp::ReferenceContext {
            include_declaration: true, // the client sends two requests and the second request with false obscures the first response
        },
    }
}

pub fn init_language_server_router(proxy: Proxy) -> Router<Proxy> {
    let mut router: Router<Proxy> = Router::new(proxy);
    router
        .request::<R::Initialize, _>(lifecycle::initialize)
        .notification::<N::Initialized>(lifecycle::initialized)
        .request::<R::Shutdown, _>(lifecycle::shutdown)
        .notification::<N::Exit>(lifecycle::exit)
        .notification::<N::DidOpenTextDocument>(doc_sync::proxy_did_open)
        .notification::<N::DidChangeTextDocument>(doc_sync::proxy_did_change)
        .notification::<N::DidSaveTextDocument>(doc_sync::proxy_did_save)
        .notification::<N::DidCloseTextDocument>(doc_sync::proxy_did_close)
        .notification::<N::DidChangeWatchedFiles>(doc_sync::proxy_did_change_watched_files)
        .request::<R::CodeLensRequest, _>(doc_sync::proxy_sync_doc_by_code_lens_request)
        .request::<R::SignatureHelpRequest, _>(common_features::proxy_signature_help)
        .notification::<N::Cancel>(common_features::proxy_cancel_request)
        .request::<R::HoverRequest, _>(hover::proxy_hover_with_decl_info)
        .request::<R::GotoDefinition, _>(definition::proxy_definition)
        .request::<R::Completion, _>(completion::proxy_completion)
        .request::<R::ResolveCompletionItem, _>(completion::proxy_completion_item_resolve)
        .request::<R::References, _>(Proxy::references)
        .request::<R::PrepareRenameRequest, _>(common_features::proxy_prepare_rename)
        .request::<R::Rename, _>(common_features::proxy_rename);
    router
}

#[allow(clippy::redundant_async_block)]
impl LanguageServer for Proxy {
    type Error = ResponseError;
    type NotifyResult = std::ops::ControlFlow<async_lsp::Result<()>>;

    fn initialize(&mut self, _: lsp::InitializeParams) -> ResFut<R::Initialize> {
        unreachable!()
    }

    #[tracing::instrument(skip_all)]
    fn definition(&mut self, params: lsp::GotoDefinitionParams) -> ResFut<R::GotoDefinition> {
        let req = definition::proxy_definition(self, params);
        Box::pin(async move { req.await })
    }

    #[tracing::instrument(skip_all)]
    fn references(&mut self, params: lsp::ReferenceParams) -> ResFut<R::References> {
        self.state.cancel_received.store(false);
        let req = references::proxy_workspace_references(self, params);
        let (state, mut client) = (self.state.clone(), self.client());
        Box::pin(async move {
            state.create_progress(&mut client).await;
            state.send_progress(&mut client, (0, 0), "tsserver request declaration"); // for workspace search
            let res = req.await.map(|res| {
                let is_source = |l: &lsp::Location| state.get_build_by_emit_uri(&l.uri).is_none();
                res.map(|locations| locations.into_iter().filter(is_source).collect())
            });
            state.destroy_progress(&mut client);
            res
        })
    }
}
