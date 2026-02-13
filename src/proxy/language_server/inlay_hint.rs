use std::ops::Deref;

use async_lsp::lsp_types::request as R;
use async_lsp::{LanguageServer, lsp_types as lsp};

use crate::builder::Build;
use crate::proxy::language_server::{Error, forward_build_range};
use crate::proxy::{Proxy, ResFut};
use crate::state::State;
use crate::try_ensure_bundle;

pub fn proxy_inlay_hint(
    this: &mut Proxy,
    mut params: lsp::InlayHintParams,
) -> ResFut<R::InlayHintRequest> {
    let mut s = this.server();
    let uri = &params.text_document.uri;
    let bundle = try_ensure_bundle!(this, uri, params, inlay_hint);
    let doc = this.state.get_doc(uri).unwrap();
    let Some(mut bundle_range) = bundle.forward_src_range(&params.range, &doc.source) else {
        return Box::pin(async move { Err(Error::forward_failed()) });
    };
    let first_non_include_build_pos = doc.first_non_include_build_pos(&bundle);

    if let Some(source_start) = first_non_include_build_pos
        && source_start > bundle_range.end
    {
        return Box::pin(async move { Err(Error::forward_failed()) });
    }

    if let Some(source_start) = first_non_include_build_pos
        && source_start > bundle_range.start
    {
        bundle_range.start = source_start;
    }

    params.text_document.uri = bundle.uri.clone();
    params.range = bundle_range;

    let req = s.inlay_hint(params);
    let st = this.state.clone();

    Box::pin(async move {
        use rayon::prelude::*;

        let doc_source = doc.source.deref();
        let fm = |h: lsp::InlayHint| {
            let (position, source) = bundle.forward_build_position(&h.position)?;
            if &source != doc_source {
                return None;
            }

            Some(lsp::InlayHint {
                label: forward_label(&h, &st)?,
                text_edits: forward_text_edits(&h, &bundle),
                position,
                ..h
            })
        };

        match req.await.map_err(Error::internal) {
            Ok(Some(h)) => Ok(Some(h.into_par_iter().filter_map(fm).collect())),
            Ok(None) => Ok(None),
            Err(err) => Err(err),
        }
    })
}

fn forward_label(h: &lsp::InlayHint, st: &State) -> Option<lsp::InlayHintLabel> {
    let project = st.get_project();
    let forward_location = |location: &Option<lsp::Location>| {
        let mut l = location.clone()?;
        // TODO: refactor like this:
        let Some(build) = st.get_any_build_by_emit_uri(&l.uri) else {
            return l.into();
        };
        let source = forward_build_range(&mut l.range, &build).ok()?;
        l.uri = st.path_to_uri(&project.join(source.as_str())).unwrap();
        l.into()
    };

    let forward_part = |p: &lsp::InlayHintLabelPart| lsp::InlayHintLabelPart {
        location: forward_location(&p.location),
        ..p.clone()
    };

    let should_label_hidden = |l: &str| {
        l.contains(": any")
            || l.contains("...args:")
            || l.contains("...items:")
            || l.contains("separator:")
    };

    let should_parts_hidden = |parts: &Vec<lsp::InlayHintLabelPart>| {
        let label = parts.iter().fold("".to_owned(), |buf, p| buf + &p.value);
        should_label_hidden(&label)
    };

    type L = lsp::InlayHintLabel;
    match &h.label {
        L::String(s) => match should_label_hidden(s) {
            false => L::String(s.clone()).into(),
            true => None,
        },
        L::LabelParts(parts) => match should_parts_hidden(parts) {
            false => Some(L::LabelParts(parts.iter().map(forward_part).collect())),
            true => None,
        },
    }
}

fn forward_text_edits(h: &lsp::InlayHint, bundle: &Build) -> Option<Vec<lsp::TextEdit>> {
    let fm = |mut e: lsp::TextEdit| {
        forward_build_range(&mut e.range, bundle).ok()?;
        Some(e)
    };

    if let Some(edits) = &h.text_edits {
        edits
            .iter()
            .cloned()
            .filter_map(fm)
            .collect::<Vec<lsp::TextEdit>>()
            .into()
    } else {
        None
    }
}
