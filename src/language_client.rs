use std::ops::ControlFlow;

use async_lsp::lsp_types as lsp;
use async_lsp::lsp_types::request as R;
use async_lsp::{ErrorCode, LanguageClient, ResponseError};

use crate::proxy::{Proxy, ResFut};

impl LanguageClient for Proxy {
    type Error = ResponseError;
    type NotifyResult = ControlFlow<async_lsp::Result<()>>;

    fn work_done_progress_create(
        &mut self,
        params: lsp::WorkDoneProgressCreateParams,
    ) -> ResFut<R::WorkDoneProgressCreate> {
        let mut service = self.client();
        Box::pin(async move {
            let res = service.work_done_progress_create(params).await;
            res.map_err(|e| ResponseError::new(ErrorCode::INTERNAL_ERROR, e))
        })
    }

    fn log_message(&mut self, params: lsp::LogMessageParams) -> Self::NotifyResult {
        let _ = self.client().log_message(params);
        ControlFlow::Continue(())
    }

    fn progress(&mut self, params: lsp::ProgressParams) -> Self::NotifyResult {
        let _ = self.client().progress(params);
        ControlFlow::Continue(())
    }
}
