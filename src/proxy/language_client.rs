use std::collections::HashMap;

use async_lsp::lsp_types::{self as lsp, Url as Uri, request as R};
use async_lsp::{LanguageClient, ResponseError};

use crate::proxy::language_server::{Error, forward_build_range};
use crate::proxy::{Proxy, ResFut};

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

    fn apply_edit(
        &mut self,
        mut params: lsp::ApplyWorkspaceEditParams,
    ) -> ResFut<R::ApplyWorkspaceEdit> {
        if !matches!(
            params.edit,
            lsp::WorkspaceEdit {
                changes: Some(_),
                document_changes: None,
                change_annotations: None
            }
        ) {
            return Box::pin(async move {
                Ok(lsp::ApplyWorkspaceEditResponse {
                    applied: false,
                    failure_reason: Some("unimplemented".into()),
                    failed_change: None,
                })
            });
        }

        let mut c = self.client();
        let st = self.state.clone();
        let project = st.get_project();
        let mut source_changes = HashMap::<Uri, Vec<lsp::TextEdit>>::new();
        let changes = params.edit.changes.unwrap();

        // TODO: if the request intersects more then one build
        // (ex.: multiply build references rename req)
        changes.into_iter().for_each(|(uri, edits)| {
            let Some(any_build) = st.get_any_build_by_emit_uri(&uri) else {
                // TODO: tsserver maybe return intersects edits
                // by any_build & source file (which included in this any_build)
                source_changes.insert(uri, edits);
                return;
            };

            for e in edits {
                let mut source_range = e.range;
                let Ok(source) = forward_build_range(&mut source_range, &any_build) else {
                    continue;
                };
                let Ok(source_uri) = st.path_to_uri(&project.join(source.as_str())) else {
                    continue;
                };
                let edit = || lsp::TextEdit::new(source_range, e.new_text.to_owned());
                source_changes
                    .entry(source_uri)
                    .and_modify(|source_edits| source_edits.push(edit()))
                    .or_insert(vec![edit()]);
            }
        });

        params.edit.changes = source_changes.into();
        Box::pin(async move { c.apply_edit(params).await.map_err(Error::internal) })
    }

    fn publish_diagnostics(&mut self, params: lsp::PublishDiagnosticsParams) -> Self::NotifyResult {
        use rayon::prelude::*;

        let mut client = self.client();
        let state = self.state.clone();

        if state.get_transpile(&params.uri).is_some() {
            return std::ops::ControlFlow::Continue(());
        }

        let Some(any_build) = state.get_any_build_by_emit_uri(&params.uri) else {
            tracing::warn!("{}", Error::unbuild_fallback());
            let _ = client.publish_diagnostics(params);
            return std::ops::ControlFlow::Continue(());
        };

        let doc = state.get_doc_by_emit_uri(&params.uri).unwrap();
        let project = state.get_project();

        let source_diagnostics = params.diagnostics.into_par_iter().filter_map(|d| {
            let mut range = d.range;
            let Ok(source) = forward_build_range(&mut range, &any_build) else {
                tracing::warn!("{}", Error::forward_failed());
                return None;
            };

            if source != *doc.source {
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
                    c if c.len() == 4 && c.starts_with("1") => Some(DS::ERROR), // syntactic errors
                    _ => Some(DS::HINT),
                }
            } else {
                None
            };

            let related_information = if let Some(related_information) = d.related_information {
                let mut source_related_information = Vec::with_capacity(related_information.len());
                for ri in related_information {
                    let Some(any_build) = state.get_any_build_by_emit_uri(&ri.location.uri) else {
                        continue;
                    };
                    let mut source_ri_range = ri.location.range;
                    let Ok(source) = forward_build_range(&mut source_ri_range, &any_build) else {
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
                state.path_to_uri(&doc.path).unwrap(),
                source_diagnostics.collect(),
                None,
            ))
            .unwrap();

        std::ops::ControlFlow::Continue(())
    }
}
