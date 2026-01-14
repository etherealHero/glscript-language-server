use async_lsp::lsp_types::request as R;
use async_lsp::{LanguageServer, lsp_types as lsp};

use crate::language_server::Error;
use crate::proxy::{Proxy, ResFut};
use crate::{try_ensure_build, try_forward_text_document_position_params};

pub fn proxy_completion(
    this: &mut Proxy,
    mut params: lsp::CompletionParams,
) -> ResFut<R::Completion> {
    let mut s = this.server();
    let uri = &params.text_document_position.text_document.uri;
    let build = try_ensure_build!(this, uri, params, completion);
    let state = this.state.clone();
    Box::pin(async move {
        type Res = lsp::CompletionResponse;
        let forward = forward_build_completion_item;
        let doc_pos = &mut params.text_document_position;
        try_forward_text_document_position_params!(state, build, doc_pos);

        s.completion(params)
            .await
            .map_err(Error::internal)
            .map(|r| r.unwrap())
            .map(|mut response| {
                match response {
                    Res::Array(ref mut items) => items.iter_mut().for_each(forward),
                    Res::List(ref mut list) => list.items.iter_mut().for_each(forward),
                };
                Some(response)
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
                forward_build_completion_item(&mut res);
                res
            })
    })
}

fn forward_build_completion_item(item: &mut lsp::CompletionItem) {
    item.text_edit = None; // can't define context
    item.additional_text_edits = None;
    item.command = None;
}
