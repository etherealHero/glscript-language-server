use std::collections::HashSet;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use async_lsp::lsp_types::{Url as Uri, request as R};
use async_lsp::{LanguageClient, LanguageServer, ResponseError, lsp_types as lsp};
use tokio::time::{Duration, timeout};

use crate::builder::Build;
use crate::proxy::language_server::{DefRes, Error, forward_build_range};
use crate::proxy::language_server::{definition_params, references_params};
use crate::proxy::language_server::{did_close, did_open};
use crate::proxy::{Canonicalize, Proxy, ResFut};
use crate::proxy::{DECL_FILE_EXT, DEFAULT_TIMEOUT_MS, JS_FILE_EXT};
use crate::state::State;
use crate::types::{SourceHash, SourcePattern};
use crate::{try_ensure_bundle, try_forward_text_document_position_params};

pub fn proxy_workspace_references(
    this: &mut Proxy,
    mut p: lsp::ReferenceParams,
) -> ResFut<R::References> {
    if !p.context.include_declaration {
        return Box::pin(async move { Ok(None) });
    }

    let uri = &p.text_document_position.text_document.uri;
    let pos = &p.text_document_position.position;
    let req_bundle = try_ensure_bundle!(this, uri, p, references);
    let definition_request = this.definition(definition_params(uri.clone(), pos.to_owned()));

    let mut s = this.server();
    let mut client = this.client();
    let st = this.state.clone();
    let root = st.get_project().clone();
    let temp_uri = Uri::from_str("file:///.virtual/refs.js").unwrap();

    Box::pin(async move {
        let def_loc = get_definition_location(definition_request).await?;
        if def_loc.target_uri.as_str().ends_with(DECL_FILE_EXT) {
            let doc_pos = &mut p.text_document_position;
            try_forward_text_document_position_params!(st, req_bundle, doc_pos);
            return fetch_with_build_params(&mut s, &st, &root, p, req_bundle, None).await;
        }

        if !def_loc.target_uri.as_str().ends_with(JS_FILE_EXT) {
            return Err(Error::unexpected_source());
        }

        let mut ws_locs = HashSet::new();
        let mut is_sync_doc_failed = false;
        let def_source = st.get_doc(&def_loc.target_uri).unwrap().source;
        let opened_bundles_contains_source = st.get_bundles_contains_source(&def_source); // TODO: if global context ?
        let unopened_docs = get_unopened_documents(&st, &root, &def_loc);

        for (i, doc_uri) in unopened_docs.iter().enumerate() {
            let try_open = |s: &mut async_lsp::ServerSocket| {
                let bundle = st.get_bundle(doc_uri).unwrap();
                did_open(s, &temp_uri, &bundle.content, None)
            };

            if st.cancel_received.load() || try_open(&mut s).is_err() {
                st.remove_bundle(doc_uri);
                continue;
            }

            let doc_path = st.uri_to_path(doc_uri).unwrap();
            let doc_path = doc_path.strip_prefix(&root).unwrap_or(&doc_path);
            let msg = format!("tsserver request {}", doc_path.display());
            let t = Some(temp_uri.clone());

            if traverse(doc_uri, &def_loc, &mut s, &st, &root, &mut ws_locs, t)
                .await
                .is_err()
            {
                is_sync_doc_failed = true;
            };
            let _ = did_close(&mut s, &temp_uri);

            st.send_progress(&mut client, (i, unopened_docs.len()), &msg);
            st.remove_bundle(doc_uri);
            tracing::info!("{msg}");
        }

        for doc_path in opened_bundles_contains_source {
            if st.cancel_received.load() {
                break;
            }
            let doc_uri = st.path_to_uri(&doc_path).unwrap();
            st.commit_changes(&doc_uri, &mut s);
            traverse(&doc_uri, &def_loc, &mut s, &st, &root, &mut ws_locs, None).await?;
        }

        if st.cancel_received.load() {
            return Ok(None);
        }

        if is_sync_doc_failed {
            let _ = client.show_message(lsp::ShowMessageParams {
                typ: lsp::MessageType::WARNING,
                message: "Some script modules failed to sync on build stage.
                            Response of references may be incomplete.
                            See output logs for more details."
                    .into(),
            });
        }

        Ok(Some(ws_locs.into_iter().collect()))
    })
}

async fn traverse(
    doc_uri: &Uri,
    def_loc: &lsp::LocationLink,
    service: &mut async_lsp::ServerSocket,
    st: &Arc<State>,
    root: &Path,
    workspace_locations: &mut HashSet<lsp::Location>,
    temp: Option<Uri>,
) -> Result<(), ResponseError> {
    let bundle = st.get_bundle(doc_uri).unwrap();
    let def_pos = &def_loc.target_selection_range.start;
    let def_source = st.get_doc(&def_loc.target_uri).unwrap().source;
    let position = match bundle.forward_src_position(def_pos, &def_source) {
        Some(pos) => pos,
        None => {
            let doc_path = st.uri_to_path(doc_uri).unwrap();
            let doc_path = doc_path.strip_prefix(root).unwrap_or(&doc_path);
            let err = format!("Sync doc ({}) failed. Request aborted", doc_path.display());
            tracing::error!(err);
            return Err(Error::request_failed(err));
        }
    };

    let req_uri = temp.clone().unwrap_or_else(|| bundle.uri.clone());
    let fwd_params = references_params(req_uri, position);
    let fetch_response = timeout(
        Duration::from_millis(DEFAULT_TIMEOUT_MS),
        fetch_with_build_params(service, st, root, fwd_params, bundle, temp),
    )
    .await
    .unwrap_or(Ok(None));

    if let Ok(Some(locations)) = fetch_response {
        for l in locations.into_iter() {
            workspace_locations.insert(l);
        }
    }

    Ok(())
}

fn get_unopened_documents(
    state: &Arc<State>,
    project: &Path,
    def_loc: &lsp::LocationLink,
) -> Vec<Uri> {
    use ignore::Walk;
    use rayon::prelude::*;

    let def_source = state.get_doc(&def_loc.target_uri).unwrap().source;
    let opened_bundles_contains_source = state.get_bundles_contains_source(&def_source); // TODO: if global context ?
    let default_sources: Vec<_> = state.get_default_sources();
    tracing::info!("raw_entries scan...");
    let mut raw_entries = Vec::with_capacity(default_sources.len());
    for entry in Walk::new(project).flatten() {
        if entry.file_type().is_some_and(|ft| ft.is_file()) {
            raw_entries.push(entry.path().to_owned());
        }
    }
    tracing::info!("raw_entries scanned; repository indexing...");
    let (js, decl) = (&JS_FILE_EXT[1..], &DECL_FILE_EXT[1..]);
    let (def_lit, source_hash) = get_definition_pattern(def_loc, state);
    let matched_docs: Vec<Uri> = raw_entries
        .par_iter()
        .filter_map(|p| {
            let pat = SourcePattern::new(&def_lit, source_hash);
            let uri = state.path_to_uri(p.as_path()).ok();
            if uri.is_none() || !p.extension().is_some_and(|ext| ext == js || ext == decl) {
                return None;
            }
            let matched = match uri.as_ref().and_then(|u| state.get_doc(u).ok()) {
                Some(doc) => doc.parse_content.contains(&def_lit),
                None => file_contains_text(p, &def_lit).ok()?,
            };
            if !matched
                || opened_bundles_contains_source.contains(&p.to_path_buf())
                || default_sources.contains(&p.to_path_buf())
            {
                return None;
            }

            state
                .set_bundle_with_tree_shaking(uri.as_ref().unwrap(), pat)
                .ok()?;

            uri
        })
        .collect();
    tracing::info!("repository indexed");

    let all_bundles_contains_source = state.get_bundles_contains_source(&def_source); // TODO: if global context ?

    matched_docs.par_iter().for_each(|d| {
        if !all_bundles_contains_source.contains(&state.uri_to_path(d).unwrap()) {
            state.remove_bundle(d);
        }
    });

    all_bundles_contains_source
        .into_par_iter()
        .filter(|p| !opened_bundles_contains_source.contains(p))
        .map(|p| state.path_to_uri(&p).unwrap())
        .collect()
}

fn get_definition_pattern(def_loc: &lsp::LocationLink, state: &Arc<State>) -> (String, SourceHash) {
    let def_doc = state.get_doc(&def_loc.target_uri).unwrap();
    let s = def_loc.target_selection_range.start;
    let start_pos = def_doc.buffer.line_to_char(s.line as usize) + s.character as usize;
    let e = def_loc.target_selection_range.end;
    let end_pos = def_doc.buffer.line_to_char(e.line as usize) + e.character as usize;
    let lit = def_doc.buffer.slice(start_pos..end_pos).to_string();
    (lit, def_doc.source_hash)
}

async fn get_definition_location(
    definition_request: ResFut<R::GotoDefinition>,
) -> Result<lsp::LocationLink, ResponseError> {
    let definition_response = definition_request.await;
    let message = "Definition of references request not found";
    match definition_response {
        Ok(Some(ref definition)) => match definition {
            DefRes::Link(links) => match links.first() {
                Some(def_loc) => Ok(def_loc.to_owned()),
                None => Err(Error::request_failed(message)),
            },
            _ => unreachable!(),
        },
        Ok(None) => Err(Error::request_failed(message)),
        Err(err) => Err(err),
    }
}

async fn fetch_with_build_params(
    s: &mut async_lsp::ServerSocket,
    state: &Arc<State>,
    project: &Path,
    build_params: lsp::ReferenceParams,
    build: Arc<Build>,
    temp: Option<Uri>,
) -> Result<Option<Vec<lsp::Location>>, ResponseError> {
    s.references(build_params)
        .await
        .map_err(Error::internal)
        .map(Option::unwrap)
        .map(|mut r| {
            r.iter_mut().for_each(|l| {
                let req_uri = temp.clone().unwrap_or_else(|| build.uri.try_canonicalize());
                if req_uri == l.uri.try_canonicalize()
                    && let Ok(source) = forward_build_range(&mut l.range, &build)
                {
                    l.uri = state.path_to_uri(&project.join(source.as_str())).unwrap();
                }
            });
            Some(r)
        })
}

fn file_contains_text<P: AsRef<Path>>(filename: P, search_term: &str) -> anyhow::Result<bool> {
    use memmap2::Mmap;
    use std::fs::File;

    let file = File::open(filename)?;
    let mmap = unsafe { Mmap::map(&file)? };

    Ok(mmap
        .windows(search_term.len())
        .any(|window| window == search_term.as_bytes()))
}
