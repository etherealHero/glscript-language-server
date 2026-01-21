use async_lsp::lsp_types::{self as lsp, request as R};
use async_lsp::{LanguageClient, ResponseError};

use crate::proxy::{Proxy, ResFut, language_server::Error};

impl LanguageClient for Proxy {
    type Error = ResponseError;
    type NotifyResult = std::ops::ControlFlow<async_lsp::Result<()>>;

    fn work_done_progress_create(
        &mut self,
        params: lsp::WorkDoneProgressCreateParams,
    ) -> ResFut<R::WorkDoneProgressCreate> {
        let mut c = self.client();
        Box::pin(async move {
            c.work_done_progress_create(params)
                .await
                .map_err(Error::internal)
        })
    }

    fn log_message(&mut self, params: lsp::LogMessageParams) -> Self::NotifyResult {
        let _ = self.client().log_message(params);
        std::ops::ControlFlow::Continue(())
    }

    fn progress(&mut self, params: lsp::ProgressParams) -> Self::NotifyResult {
        let _ = self.client().progress(params);
        std::ops::ControlFlow::Continue(())
    }
}
