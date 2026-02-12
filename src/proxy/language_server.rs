use async_lsp::lsp_types::{Url as Uri, notification as N, request as R};
use async_lsp::router::Router;
use async_lsp::{ErrorCode, LanguageServer, ResponseError, ServerSocket, lsp_types as lsp};

use crate::builder::Build;
use crate::proxy::{JS_LANG_ID, Proxy, ResFut};
use crate::types::Source;

mod code_action;
mod common_features;
mod completion;
mod definition;
mod doc_symbol;
mod doc_sync;
mod formatting;
mod hover;
mod inlay_hint;
mod lifecycle;
mod references;
mod selection_range;
mod semantic_tokens;
mod ws_symbol;

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
        use crate::proxy::{DECL_FILE_EXT, JS_FILE_EXT};

        let reason = "Missmatched source extension";
        let err = format!("{reason}, expect '{JS_FILE_EXT}' or '{DECL_FILE_EXT}'. Request aborted");
        ResponseError::new(ErrorCode::REQUEST_FAILED, err)
    }

    pub fn forward_failed() -> ResponseError {
        ResponseError::new(ErrorCode::REQUEST_FAILED, "Forward failed")
    }

    pub fn unbuild_fallback() -> ResponseError {
        let message = "Build not found, fallback request...";
        ResponseError::new(ErrorCode::INTERNAL_ERROR, message)
    }
}

pub fn forward_build_range(range: &mut lsp::Range, build: &Build) -> Result<Source, ResponseError> {
    let source_range = build.forward_build_range(range);
    if source_range.is_none() {
        return Err(Error::forward_failed());
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

pub fn did_open(
    s: &mut ServerSocket,
    uri: &Uri,
    text: &str,
    version: Option<i32>,
) -> Result<(), ResponseError> {
    let (lang, version, text) = (JS_LANG_ID.into(), version.unwrap_or(1), text.into());
    let text_document = lsp::TextDocumentItem::new(uri.clone(), lang, version, text);
    let open = s.did_open(lsp::DidOpenTextDocumentParams { text_document });
    open.map_err(Error::request_failed)
}

pub fn did_close(s: &mut ServerSocket, uri: &Uri) -> Result<(), ResponseError> {
    let text_document = lsp::TextDocumentIdentifier::new(uri.clone());
    s.did_close(lsp::DidCloseTextDocumentParams { text_document })
        .map_err(Error::request_failed)
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
        .request::<R::Rename, _>(common_features::proxy_rename)
        .request::<R::SelectionRangeRequest, _>(selection_range::proxy_selection_range)
        .request::<R::DocumentSymbolRequest, _>(doc_symbol::proxy_document_symbol)
        .request::<R::WorkspaceSymbolRequest, _>(ws_symbol::proxy_symbol)
        .request::<R::WorkspaceSymbolResolve, _>(ws_symbol::proxy_workspace_symbol_resolve)
        .request::<R::FoldingRangeRequest, _>(common_features::proxy_folding_range)
        .request::<R::SemanticTokensFullRequest, _>(semantic_tokens::proxy_semantic_tokens_full)
        .request::<R::SemanticTokensRangeRequest, _>(semantic_tokens::proxy_semantic_tokens_range)
        .request::<R::Formatting, _>(formatting::proxy_formatting)
        .request::<R::RangeFormatting, _>(formatting::proxy_range_formatting)
        .request::<R::InlayHintRequest, _>(inlay_hint::proxy_inlay_hint)
        .request::<R::CodeActionRequest, _>(code_action::proxy_code_action)
        .request::<R::ExecuteCommand, _>(code_action::proxy_execute_command);
    router
}

// TODO: https://github.com/microsoft/vscode/blob/main/extensions/typescript-language-features/src/languageFeatures/formatting.ts
type _OnTypeFormatting = ResFut<R::OnTypeFormatting>;

#[allow(clippy::redundant_async_block)]
impl LanguageServer for Proxy {
    type Error = ResponseError;
    type NotifyResult = std::ops::ControlFlow<async_lsp::Result<()>>;

    fn initialize(&mut self, _: lsp::InitializeParams) -> ResFut<R::Initialize> {
        unreachable!()
    }

    /// Used in
    /// - [`hover::proxy_hover_with_decl_info`]
    /// - [`references::proxy_workspace_references`]
    fn definition(&mut self, params: lsp::GotoDefinitionParams) -> ResFut<R::GotoDefinition> {
        let req = definition::proxy_definition(self, params);
        Box::pin(async move { req.await })
    }

    /// Used in
    /// - [`common_features::proxy_rename`]
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
