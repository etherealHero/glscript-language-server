use async_lsp::lsp_types::{self as lsp, notification as N, request as R};
use async_lsp::router::Router;
use async_lsp::{LanguageClient, ResponseError};

use crate::proxy::{Error, Proxy, ResFut};

mod apply_edit;
mod publish_diagnostics;

pub fn init_language_client_router(proxy: Proxy) -> Router<Proxy> {
    let mut router: Router<Proxy> = Router::new(proxy);
    router
        .notification::<N::PublishDiagnostics>(publish_diagnostics::proxy_publish_diagnostics)
        .notification::<N::LogMessage>(Proxy::log_message)
        .notification::<N::Progress>(Proxy::progress)
        .request::<R::WorkDoneProgressCreate, _>(Proxy::work_done_progress_create)
        .request::<R::ApplyWorkspaceEdit, _>(apply_edit::proxy_apply_edit);
    router
}

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
