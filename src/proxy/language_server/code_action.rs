use std::collections::HashMap;

use async_lsp::lsp_types::request as R;
use async_lsp::{LanguageServer, lsp_types as lsp};

use crate::builder::Build;
use crate::parser::Token;
use crate::proxy::language_server::Error;
use crate::proxy::{Proxy, ResFut};
use crate::state::State;
use crate::try_ensure_bundle;
use crate::types::Document;

type K = lsp::CodeActionKind;

pub fn proxy_code_action(
    this: &mut Proxy,
    mut params: lsp::CodeActionParams,
) -> ResFut<R::CodeActionRequest> {
    if params
        .context
        .trigger_kind
        .is_some_and(|k| lsp::CodeActionTriggerKind::AUTOMATIC == k)
    {
        // client send recoursive req sequence (code_action -> publish_diagnostics -> code_action...)
        return Box::pin(async move { Ok(None) });
    };

    let mut s = this.server();
    let uri = &params.text_document.uri;
    let bundle = try_ensure_bundle!(this, uri, params, code_action);
    let st = this.state.clone();
    let doc = st.get_doc(uri).unwrap();
    let transpile = st.get_transpile(uri).unwrap();
    let Some(mut bundle_range) = bundle.forward_src_range(&params.range, &doc.source) else {
        return Box::pin(async move { Err(Error::forward_failed()) });
    };
    let first_non_include_build_pos = doc.first_non_include_build_pos(&bundle);

    if let Some(source_start) = first_non_include_build_pos
        && source_start > bundle_range.end
    {
        return match get_transpile_action(&doc, &transpile, &st) {
            Some(transpile_action) => Box::pin(async move { Ok(Some(vec![transpile_action])) }),
            None => Box::pin(async move { Ok(None) }),
        };
    }

    if let Some(source_start) = first_non_include_build_pos
        && source_start > bundle_range.start
    {
        bundle_range.start = source_start;
    }

    params.text_document.uri = bundle.uri.clone();
    params.context.diagnostics = vec![]; // unimpl _typescript lsp req on pure clients
    params.range = bundle_range;

    Box::pin(async move {
        match s.code_action(params).await.map_err(Error::internal) {
            Ok(Some(actions)) => Ok(Some({
                let mut actions: Vec<_> = actions
                    .into_iter()
                    .filter_map(|a| match a {
                        lsp::CodeActionOrCommand::Command(c) => {
                            tracing::error!("{}: {c:#?}", Error::forward_failed());
                            None
                        }
                        lsp::CodeActionOrCommand::CodeAction(ca) => {
                            let move_action = ca.kind == K::new("refactor.move").into();
                            match ca.disabled.is_some() || move_action {
                                false => lsp::CodeActionOrCommand::CodeAction(ca).into(),
                                true => None,
                            }
                        }
                    })
                    .collect();

                if let Some(transpile_action) = get_transpile_action(&doc, &transpile, &st) {
                    actions.push(transpile_action);
                };

                actions
            })),
            Ok(None) => Ok(None),
            Err(err) => {
                tracing::warn!("tsserer error: {err}");
                Ok(None)
            }
        }
    })
}

// TODO: send multiply req on inline multi-build variable (use Proxy::references handle)
pub fn proxy_execute_command(
    this: &mut Proxy,
    params: lsp::ExecuteCommandParams,
) -> ResFut<R::ExecuteCommand> {
    let mut s = this.server();
    Box::pin(async move { s.execute_command(params).await.map_err(Error::internal) })
}

fn get_transpile_action(
    doc: &Document,
    transpile: &Build,
    st: &State,
) -> Option<lsp::CodeActionOrCommand> {
    if doc.parse_content.as_str().replace("\r\n", "\n")
        == transpile.content.as_str().replace("\r\n", "\n")
    {
        return None;
    }

    let mut changes = HashMap::new();
    let eof = match doc.parse.compressed_tokens.last().unwrap() {
        Token::Eoi(lc) => lc,
        _ => unreachable!(),
    };
    let whole_file = lsp::Range::new(
        lsp::Position::new(0, 0),
        lsp::Position::new(eof.line, eof.col),
    );

    changes.insert(
        st.path_to_uri(&doc.path).unwrap(),
        vec![lsp::TextEdit::new(
            whole_file,
            transpile.content.as_str().into(),
        )],
    );

    lsp::CodeActionOrCommand::CodeAction(lsp::CodeAction {
        title: "Transpile to ES syntax".into(),
        kind: K::REFACTOR.into(),
        is_preferred: true.into(),
        edit: lsp::WorkspaceEdit::new(changes).into(),
        ..Default::default()
    })
    .into()
}
