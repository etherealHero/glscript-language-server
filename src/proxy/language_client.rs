use async_lsp::lsp_types::{self as lsp, request as R};
use async_lsp::{LanguageClient, ResponseError};

use crate::proxy::language_server::forward_build_range;
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

    #[tracing::instrument(skip_all, name = "proxy_publish_diagnostics")]
    fn publish_diagnostics(&mut self, params: lsp::PublishDiagnosticsParams) -> Self::NotifyResult {
        use rayon::prelude::*;

        let mut client = self.client();
        let state = self.state.clone();
        let Some(build) = state.get_build_by_emit_uri(&params.uri) else {
            tracing::warn!("build not found, fallback request...",);
            let _ = client.publish_diagnostics(params);
            return std::ops::ControlFlow::Continue(());
        };

        let doc_of_build = state.get_doc_by_emit_uri(&params.uri).unwrap();
        let project = state.get_project();

        let source_diagnostics = params.diagnostics.into_par_iter().filter_map(|d| {
            let mut range = d.range;
            let Ok(source) = forward_build_range(&mut range, &build) else {
                tracing::warn!("forward_build_range failed");
                return None;
            };

            if source != *doc_of_build.source {
                return None;
            }

            type NS = lsp::NumberOrString;
            type DS = lsp::DiagnosticSeverity;
            let severity = if let Some(code) = d.code.as_ref().map(|c| match c {
                NS::Number(id) => id.to_string(),
                NS::String(id) => id.clone(),
            }) {
                match code.as_str() { // https://typescript.tv/errors/
                    "7006" /* any type */ => return None,
                    "80002" /* recommend class decl */ => return None,
                    "2304" /* cannot find name */ => Some(DS::WARNING),
                    "2364" /* assignment err */ => Some(DS::ERROR),
                    "2551" /* similar ident */ => Some(DS::INFORMATION),
                    _ => Some(DS::HINT),
                }
            } else {
                None
            };

            let related_information = if let Some(related_information) = d.related_information {
                let mut source_related_information = Vec::with_capacity(related_information.len());
                for ri in related_information {
                    let Some(build) = state.get_build_by_emit_uri(&ri.location.uri) else {
                        continue;
                    };
                    let mut source_ri_range = ri.location.range;
                    let Ok(source) = forward_build_range(&mut source_ri_range, &build) else {
                        continue;
                    };

                    let source_uri = state.path_to_uri(&project.join(source.as_str())).unwrap();
                    source_related_information.push(lsp::DiagnosticRelatedInformation {
                        location: lsp::Location::new(source_uri, source_ri_range),
                        message: ri.message,
                    });
                }
                Some(source_related_information)
            } else {
                None
            };

            let source_diagnostic = lsp::Diagnostic {
                related_information,
                severity,
                range,
                ..d
            };

            Some(source_diagnostic)
        });

        client
            .publish_diagnostics(lsp::PublishDiagnosticsParams::new(
                state.path_to_uri(&doc_of_build.path).unwrap(),
                source_diagnostics.collect(),
                None,
            ))
            .unwrap();

        std::ops::ControlFlow::Continue(())
    }
}
