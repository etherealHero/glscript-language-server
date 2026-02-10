use std::sync::Arc;

use async_lsp::lsp_types::request as R;
use async_lsp::{LanguageServer, lsp_types as lsp};

use crate::builder::Build;
use crate::parser::Token;
use crate::proxy::language_server::{Error, forward_build_range};
use crate::proxy::{Proxy, ResFut};
use crate::try_ensure_transpile;
use crate::types::Document;

pub fn formatting(
    this: &mut Proxy,
    mut params: lsp::DocumentFormattingParams,
) -> ResFut<R::Formatting> {
    let mut s = this.server();
    let transpile = try_ensure_transpile!(this, &params.text_document.uri, params, formatting);
    let doc = this.state.get_doc(&params.text_document.uri).unwrap();

    params.text_document.uri = transpile.uri.clone();

    let req = s.formatting(params);

    Box::pin(async move {
        let fm = |e| forward(e, &transpile, &doc);
        match req.await.map_err(Error::internal) {
            Ok(Some(e)) => Ok(Some(e.into_iter().filter_map(fm).collect())),
            Ok(None) => Ok(None),
            Err(err) => Err(err),
        }
    })
}

pub fn range_formatting(
    this: &mut Proxy,
    mut params: lsp::DocumentRangeFormattingParams,
) -> ResFut<R::RangeFormatting> {
    let mut s = this.server();
    let uri = &params.text_document.uri;
    let transpile = try_ensure_transpile!(this, uri, params, range_formatting);
    let doc = this.state.get_doc(uri).unwrap();
    let Some(transpile_range) = transpile.forward_src_range(&params.range, &doc.source) else {
        return Box::pin(async move { Err(Error::forward_failed()) });
    };

    params.text_document.uri = transpile.uri.clone();
    params.range = transpile_range;

    let req = s.range_formatting(params);

    Box::pin(async move {
        let fm = |e| forward(e, &transpile, &doc);
        match req.await.map_err(Error::internal) {
            Ok(Some(e)) => Ok(Some(e.into_iter().filter_map(fm).collect())),
            Ok(None) => Ok(None),
            Err(err) => Err(err),
        }
    })
}

fn forward(
    mut edit: lsp::TextEdit,
    transpile: &Arc<Build>,
    doc: &Document,
) -> Option<lsp::TextEdit> {
    forward_build_range(&mut edit.range, transpile).ok()?;

    let transpile_intersect = |t: &Token<'_>| match &t {
        Token::Include(s) | &Token::RegionOpen(s) | &Token::RegionClose(s) => {
            let start = lsp::Position::new(s.line_col.line, s.line_col.col);
            let end = lsp::Position::new(s.line_col.line, s.line_col.col + s.len);
            let t_range = lsp::Range::new(start, end);
            t_range.start <= edit.range.end && edit.range.start <= t_range.end
        }
        Token::IncludePath(s) => {
            let start = lsp::Position::new(s.line_col.line, s.line_col.col);
            let end = lsp::Position::new(s.line_col.line, s.line_col.col + s.lit.len() as u32 + 2);
            let t_range = lsp::Range::new(start, end);
            t_range.start <= edit.range.end && edit.range.start <= t_range.end
        }
        _ => false,
    };

    if doc.parse.compressed_tokens.iter().any(transpile_intersect) {
        return None;
    }

    edit.into()
}
