use std::collections::HashMap;

use async_lsp::LanguageClient;
use async_lsp::lsp_types::{self as lsp, Url as Uri, request as R};

use crate::proxy::{Error, Proxy, ResFut, forward_build_range};

#[cfg_attr(feature = "profiling", tracing::instrument(skip_all))]
pub fn proxy_apply_edit(
    this: &mut Proxy,
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

    let mut c = this.client();
    let st = this.state.clone();
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
