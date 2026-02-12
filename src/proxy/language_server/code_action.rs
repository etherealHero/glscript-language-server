use async_lsp::lsp_types::request as R;
use async_lsp::{LanguageServer, lsp_types as lsp};

use crate::proxy::language_server::Error;
use crate::proxy::{Proxy, ResFut};
use crate::try_ensure_bundle;

pub fn proxy_code_action(
    this: &mut Proxy,
    mut params: lsp::CodeActionParams,
) -> ResFut<R::CodeActionRequest> {
    let mut s = this.server();
    let uri = &params.text_document.uri;
    let bundle = try_ensure_bundle!(this, uri, params, code_action);
    let doc = this.state.get_doc(uri).unwrap();
    let Some(mut bundle_range) = bundle.forward_src_range(&params.range, &doc.source) else {
        return Box::pin(async move { Err(Error::forward_failed()) });
    };
    let first_non_include_build_pos = doc.first_non_include_build_pos(&bundle);

    if let Some(source_start) = first_non_include_build_pos
        && source_start > bundle_range.end
    {
        tracing::warn!("proxy_code_action not supported before import stmt",);
        return Box::pin(async move { Ok(None) });
    }

    if let Some(source_start) = first_non_include_build_pos
        && source_start > bundle_range.start
    {
        bundle_range.start = source_start;
    }

    params.text_document.uri = bundle.uri.clone();
    params.context.diagnostics = vec![];
    params.range = bundle_range;

    Box::pin(async move {
        match s.code_action(params).await.map_err(Error::internal) {
            Ok(Some(actions)) => Ok(Some(
                actions
                    .into_iter()
                    .filter_map(|a| match a {
                        lsp::CodeActionOrCommand::Command(c) => {
                            tracing::info!("{}: {c:#?}", Error::forward_failed());
                            None
                        }
                        lsp::CodeActionOrCommand::CodeAction(ca) => {
                            type K = lsp::CodeActionKind;
                            let move_action = ca.kind == K::new("refactor.move").into();
                            match ca.disabled.is_some() || move_action {
                                false => lsp::CodeActionOrCommand::CodeAction(ca).into(),
                                true => None,
                            }
                        }
                    })
                    .collect(),
            )),
            Ok(None) => Ok(None),
            Err(err) => Err(err),
        }
    })
}

pub fn proxy_execute_command(
    this: &mut Proxy,
    params: lsp::ExecuteCommandParams,
) -> ResFut<R::ExecuteCommand> {
    let mut s = this.server();
    Box::pin(async move { s.execute_command(params).await.map_err(Error::internal) })
}
