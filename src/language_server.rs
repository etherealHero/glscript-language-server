use async_lsp::lsp_types::{Url as Uri, notification as N, request as R};
use async_lsp::lsp_types::{notification::Notification, request::Request};
use async_lsp::{ErrorCode, LanguageServer, ResponseError, lsp_types as lsp};

use crate::builder::Build;
use crate::proxy::{DECL_FILE_EXT, JS_FILE_EXT, Proxy, ResFut};
use crate::types::Source;

mod common_features;
mod completion;
mod definition;
mod document_synchronization;
mod hover;
mod lifecycle;
mod references;

type FutResCompletionItem = ResFut<R::ResolveCompletionItem>;
type ChangeWatchedParams = lsp::DidChangeWatchedFilesParams;
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

#[allow(clippy::redundant_async_block)]
impl LanguageServer for Proxy {
    type Error = ResponseError;
    type NotifyResult = std::ops::ControlFlow<async_lsp::Result<()>>;

    fn initialize(&mut self, params: lsp::InitializeParams) -> ResFut<R::Initialize> {
        let req = lifecycle::initialize(self, params);
        Box::pin(async move { req.await })
    }

    fn initialized(&mut self, params: lsp::InitializedParams) -> NotifyResult {
        lifecycle::initialized(self, params)
    }

    fn shutdown(&mut self, (): <R::Shutdown as Request>::Params) -> ResFut<R::Shutdown> {
        lifecycle::shutdown(self)
    }

    fn exit(&mut self, (): <N::Exit as Notification>::Params) -> NotifyResult {
        lifecycle::exit(self)
    }

    fn did_open(&mut self, params: lsp::DidOpenTextDocumentParams) -> NotifyResult {
        document_synchronization::proxy_did_open(self, params)
    }

    #[tracing::instrument(skip_all)]
    fn did_change(&mut self, params: lsp::DidChangeTextDocumentParams) -> NotifyResult {
        document_synchronization::proxy_did_change(self, params)
    }

    fn did_save(&mut self, params: lsp::DidSaveTextDocumentParams) -> NotifyResult {
        document_synchronization::proxy_did_save(self, params)
    }

    fn did_close(&mut self, params: lsp::DidCloseTextDocumentParams) -> NotifyResult {
        document_synchronization::proxy_did_close(self, params)
    }

    fn did_change_watched_files(&mut self, p: ChangeWatchedParams) -> NotifyResult {
        document_synchronization::proxy_did_change_watched_files(self, p)
    }

    fn code_lens(&mut self, params: lsp::CodeLensParams) -> ResFut<R::CodeLensRequest> {
        let req = document_synchronization::proxy_sync_doc_by_code_lens_request(self, params);
        Box::pin(async move { req.await })
    }

    fn signature_help(&mut self, p: lsp::SignatureHelpParams) -> ResFut<R::SignatureHelpRequest> {
        let req = common_features::proxy_signature_help(self, p);
        Box::pin(async move { req.await })
    }

    fn cancel_request(&mut self, _: lsp::CancelParams) -> NotifyResult {
        common_features::proxy_cancel_request(self)
    }

    fn hover(&mut self, params: lsp::HoverParams) -> ResFut<R::HoverRequest> {
        let req = hover::proxy_hover_with_decl_info(self, params);
        Box::pin(async move { req.await })
    }

    #[tracing::instrument(skip_all)]
    fn definition(&mut self, params: lsp::GotoDefinitionParams) -> ResFut<R::GotoDefinition> {
        let req = definition::proxy_definition(self, params);
        Box::pin(async move { req.await })
    }

    fn completion(&mut self, params: lsp::CompletionParams) -> ResFut<R::Completion> {
        let req = completion::proxy_completion(self, params);
        Box::pin(async move { req.await })
    }

    fn completion_item_resolve(&mut self, p: lsp::CompletionItem) -> FutResCompletionItem {
        let req = completion::proxy_completion_item_resolve(self, p);
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
            let res = req.await;
            state.destroy_progress(&mut client);
            res
        })
    }
}
