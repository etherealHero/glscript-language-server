use async_lsp::lsp_types::request as R;
use async_lsp::{LanguageServer, lsp_types as lsp};

use crate::builder::Build;
use crate::proxy::language_server::{Error, forward_build_range};
use crate::proxy::{Proxy, ResFut};
use crate::try_ensure_transpile;

pub fn proxy_selection_range(
    this: &mut Proxy,
    mut params: lsp::SelectionRangeParams,
) -> ResFut<R::SelectionRangeRequest> {
    let mut s = this.server();
    let uri = &params.text_document.uri;
    let transpile = try_ensure_transpile!(this, uri, params, selection_range);
    let state = this.state.clone();
    let req_uri = params.text_document.uri.clone();
    let req_source = state.get_doc(&req_uri).unwrap().source;

    params.text_document.uri = transpile.uri.clone();

    Box::pin(async move {
        for source_pos in &mut params.positions {
            match transpile.forward_src_position(source_pos, &req_source) {
                Some(build_pos) => *source_pos = build_pos,
                None => return Err(Error::forward_failed()),
            }
        }

        let mut res = s.selection_range(params).await.map_err(Error::internal);

        if let Ok(Some(ref mut selections)) = res {
            let mut source_selections = Vec::with_capacity(selections.len());
            for s in selections {
                forward_build_range(&mut s.range, &transpile)?;
                source_selections.push(lsp::SelectionRange {
                    range: s.range,
                    parent: forward(&s.parent, &transpile),
                });
            }
            res = Ok(Some(source_selections));
        }

        res
    })
}

fn forward(
    ps: &Option<Box<lsp::SelectionRange>>,
    build: &Build,
) -> Option<Box<lsp::SelectionRange>> {
    if let Some(ps) = ps {
        let mut ps = ps.clone();
        forward_build_range(&mut ps.range, build).ok()?;
        Some(Box::new(lsp::SelectionRange {
            range: ps.range,
            parent: forward(&ps.parent, build),
        }))
    } else {
        None
    }
}
