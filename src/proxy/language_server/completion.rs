use std::sync::Arc;

use async_lsp::lsp_types::request as R;
use async_lsp::{LanguageServer, lsp_types as lsp};

use crate::builder::Build;
use crate::proxy::{Proxy, ResFut, language_server::Error};
use crate::state::State;
use crate::types::SCRIPT_IDENTIFIER_PREFIX;
use crate::{try_ensure_bundle, try_ensure_transpile, try_forward_text_document_position_params};

type Res = lsp::CompletionResponse;

pub fn proxy_completion(this: &mut Proxy, p: lsp::CompletionParams) -> ResFut<R::Completion> {
    let s = this.server();
    let uri = &p.text_document_position.text_document.uri;
    let b = try_ensure_bundle!(this, uri, p, completion);
    let t = try_ensure_transpile!(this, uri, p, completion);
    let st = this.state.clone();
    let doc = this.state.get_doc(uri).unwrap();

    Box::pin(async move {
        let inside_include_path = doc.is_inside_include_path(&p.text_document_position.position);
        get_completions(p, st, s, if inside_include_path { t } else { b }).await
    })
}

fn get_completions(
    mut params: lsp::CompletionParams,
    state: Arc<State>,
    mut s: async_lsp::ServerSocket,
    build: Arc<Build>,
) -> ResFut<R::Completion> {
    Box::pin(async move {
        let doc_pos = &mut params.text_document_position;
        let f = |mut item: lsp::CompletionItem| {
            if item.label.starts_with(SCRIPT_IDENTIFIER_PREFIX) {
                return None;
            };
            match item.kind {
                Some(lsp::CompletionItemKind::FOLDER) => item.sort_text = Some("1".into()),
                Some(lsp::CompletionItemKind::FILE) => item.sort_text = Some("2".into()),
                _ => {}
            };
            forward(&mut item);
            Some(item)
        };

        try_forward_text_document_position_params!(state, build, doc_pos);

        s.completion(params)
            .await
            .map_err(Error::internal)
            .map(|r| r.unwrap())
            .map(|response| {
                use rayon::prelude::*;

                match response {
                    Res::Array(items) => Res::Array(items.into_par_iter().filter_map(f).collect()),
                    Res::List(list) => Res::List(lsp::CompletionList {
                        is_incomplete: list.is_incomplete,
                        items: list.items.into_par_iter().filter_map(f).collect(),
                    }),
                }
                .into()
            })
    })
}

pub fn proxy_completion_item_resolve(
    this: &mut Proxy,
    params: lsp::CompletionItem,
) -> ResFut<R::ResolveCompletionItem> {
    let mut s = this.server();
    Box::pin(async move {
        s.completion_item_resolve(params)
            .await
            .map_err(Error::internal)
            .map(|mut res| {
                forward(&mut res);
                res
            })
    })
}

fn forward(item: &mut lsp::CompletionItem) {
    item.text_edit = None; // can't define context
    item.additional_text_edits = None;
    item.command = None;
}
