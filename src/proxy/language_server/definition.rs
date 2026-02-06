use async_lsp::lsp_types::request as R;
use async_lsp::{LanguageServer, ResponseError, lsp_types as lsp};

use crate::builder::BUILD_FILE_EXT;
use crate::proxy::language_server::{DefRes, Error, forward_build_range};
use crate::proxy::{Canonicalize, DECL_FILE_EXT, Proxy, ResFut};
use crate::state::State;
use crate::types::Source;
use crate::{try_ensure_bundle, try_forward_text_document_position_params};

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::Path;

pub fn proxy_definition(
    this: &mut Proxy,
    mut params: lsp::GotoDefinitionParams,
) -> ResFut<R::GotoDefinition> {
    let mut service = this.server();
    let uri = &params.text_document_position_params.text_document.uri;
    let req_bundle = try_ensure_bundle!(this, uri, params, definition);
    let req_bundle_sources = req_bundle.sources();
    let state = this.state.clone();

    Box::pin(async move {
        let doc_pos = &mut params.text_document_position_params;
        try_forward_text_document_position_params!(state, req_bundle, doc_pos);

        let res = service.definition(params).await.map_err(Error::internal);
        if res.is_err() || res.as_ref().expect("is some").is_none() {
            return res;
        }

        let project = state.get_project();
        let fwd = |l: _| -> Result<_, _> { forward(l, &state, &req_bundle_sources, project) };
        let ts_definition_response = res?.unwrap();
        let forward_res: DefRes = match ts_definition_response {
            DefRes::Link(location_links) => fwd(location_links)?,
            DefRes::Scalar(location) => fwd(vec![lsp::LocationLink {
                origin_selection_range: None,
                target_uri: location.uri.clone(),
                target_range: location.range,
                target_selection_range: location.range,
            }])?,
            DefRes::Array(locations) => fwd(locations
                .iter()
                .map(|l| lsp::LocationLink {
                    origin_selection_range: None,
                    target_uri: l.uri.clone(),
                    target_range: l.range,
                    target_selection_range: l.range,
                })
                .collect())?,
        };

        Ok(Some(forward_res))
    })
}

/// forward back bundle locacions into client buffer locations
fn forward(
    links: Vec<lsp::LocationLink>,
    state: &State,
    req_bundle_sources: &HashSet<Source>,
    project: &Path,
) -> Result<lsp::GotoDefinitionResponse, ResponseError> {
    let mut forward_links = HashSet::with_capacity(links.len());
    for mut link in links {
        if link.target_uri.as_str().ends_with(DECL_FILE_EXT) {
            forward_links.insert(HashLocationLink(link));
            continue;
        }

        // TODO: emit build file with global doc constant to debug anywhere ?
        if link.target_uri.as_str().ends_with(BUILD_FILE_EXT) {
            continue;
        }

        if let Some(ref any_build) = state.get_build_by_emit_uri(&link.target_uri) {
            let source = forward_build_range(&mut link.target_range, any_build)?;

            if !req_bundle_sources.contains(&source) {
                continue;
            }

            forward_build_range(&mut link.target_selection_range, any_build)?;

            let path = &project.join(source.as_str());
            link.target_uri = state.path_to_uri(path).unwrap();
            link.origin_selection_range = None;
            forward_links.insert(HashLocationLink(link));
            continue;
        }

        if let Ok(doc) = state.get_doc(&link.target_uri) {
            if !req_bundle_sources.contains(&*doc.source) {
                continue;
            }

            link.origin_selection_range = None;
            forward_links.insert(HashLocationLink(link));
        }
    }
    let forward_links = forward_links
        .into_iter()
        .map(|mut h| {
            if let Ok(path) = state.uri_to_path(&h.0.target_uri) {
                h.0.target_uri = state.path_to_uri(&path).unwrap();
            }
            h.0
        })
        .collect();
    Ok(DefRes::Link(forward_links))
}

#[derive(Debug, Eq)]
struct HashLocationLink(lsp::LocationLink);

impl Hash for HashLocationLink {
    fn hash<H: Hasher>(&self, state: &mut H) {
        if let Some(origin_selection_range) = &self.0.origin_selection_range {
            origin_selection_range.hash(state);
        }
        self.0.target_uri.try_canonicalize().hash(state);
        self.0.target_range.hash(state);
        self.0.target_selection_range.hash(state);
    }
}

impl PartialEq for HashLocationLink {
    fn eq(&self, other: &Self) -> bool {
        self.0.origin_selection_range == other.0.origin_selection_range
            && self.0.target_selection_range == other.0.target_selection_range
            && self.0.target_range == other.0.target_range
            && self.0.target_uri.try_canonicalize() == other.0.target_uri.try_canonicalize()
    }
}
