use async_lsp::lsp_types::request as R;
use async_lsp::{LanguageServer, lsp_types as lsp};

use crate::builder::Build;
use crate::proxy::language_server::{Error, forward_build_range};
use crate::proxy::{Proxy, ResFut};
use crate::try_ensure_build;
use crate::types::Source;

// TODO: proxy with virtual script without resole include stmt to save time of ts req
#[tracing::instrument(skip_all)]
pub fn proxy_selection_range(
    this: &mut Proxy,
    mut params: lsp::SelectionRangeParams,
) -> ResFut<R::SelectionRangeRequest> {
    let mut s = this.server();
    let uri = &params.text_document.uri;
    let build = try_ensure_build!(this, uri, params, selection_range);
    let state = this.state.clone();
    let req_uri = params.text_document.uri.clone();
    let req_source = state.get_doc(&req_uri).unwrap().source;

    params.text_document.uri = build.uri.clone();

    Box::pin(async move {
        for source_pos in &mut params.positions {
            match build.forward_src_position(source_pos, &req_source) {
                Some(build_pos) => *source_pos = build_pos,
                None => return Err(Error::forward_failed()),
            }
        }

        let mut res = s.selection_range(params).await.map_err(Error::internal);

        if let Ok(Some(ref mut selections)) = res {
            let mut source_selections = Vec::with_capacity(selections.len());
            for s in selections {
                if forward_build_range(&mut s.range, &build)? == *req_source {
                    source_selections.push(lsp::SelectionRange {
                        range: s.range,
                        parent: forward(&s.parent, &build, &req_source),
                    });
                }
            }
            res = Ok(Some(source_selections));
        }

        res
    })
}

fn forward(
    ps: &Option<Box<lsp::SelectionRange>>,
    build: &Build,
    source: &Source,
) -> Option<Box<lsp::SelectionRange>> {
    if let Some(ps) = ps {
        let mut ps = ps.clone();
        let ps_source = forward_build_range(&mut ps.range, build).ok()?;
        if &ps_source != source {
            return None;
        }
        Some(Box::new(lsp::SelectionRange {
            range: ps.range,
            parent: forward(&ps.parent, build, source),
        }))
    } else {
        None
    }
}
