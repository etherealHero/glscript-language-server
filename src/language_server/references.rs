use async_lsp::lsp_types::{Url as Uri, request as R};
use async_lsp::{ClientSocket, LanguageClient, LanguageServer, ResponseError, lsp_types as lsp};
use std::path::Path;
use std::sync::Arc;
use tokio::time::{Duration, timeout};

use crate::builder::Build;
use crate::language_server::{DefRes, Error, definition_params, forward_build_range};
use crate::proxy::{Canonicalize, Proxy, ResFut};
use crate::proxy::{DECL_FILE_EXT, DEFAULT_TIMEOUT_MS, JS_FILE_EXT, JS_LANG_ID};
use crate::state::State;
use crate::{try_ensure_build, try_forward_text_document_position_params};

// FIXME: proxy d.ts symbols
// FIXME: build unforwarded locations
pub fn proxy_workspace_references(
    this: &mut Proxy,
    mut params: lsp::ReferenceParams,
) -> ResFut<R::References> {
    if !params.context.include_declaration {
        return Box::pin(async move { Ok(None) });
    }

    let uri = &params.text_document_position.text_document.uri;
    let pos = &params.text_document_position.position;
    let req_build = try_ensure_build!(this, uri, params, references);
    let definition_request = this.definition(definition_params(uri.clone(), pos.to_owned()));

    let mut service = this.server();
    let mut client = this.client();
    let state = this.state.clone();
    let project = state.get_project().clone();

    Box::pin(async move {
        let def_loc = get_definition_location(definition_request).await?;

        if def_loc.target_uri.as_str().ends_with(DECL_FILE_EXT) {
            let doc_pos = &mut params.text_document_position;
            try_forward_text_document_position_params!(state, req_build, doc_pos);
            return fetch_references_for_client_from_build_params(
                &mut service,
                &state,
                &project,
                params,
                req_build,
            )
            .await;
        }

        if !def_loc.target_uri.as_str().ends_with(JS_FILE_EXT) {
            return Err(Error::unexpected_source());
        }

        let mut workspace_locations = std::collections::HashSet::new();
        let mut is_sync_doc_failed = false;
        let def_source = state.get_doc(&def_loc.target_uri).unwrap().source;
        let opened_builds_contains_source = state.get_builds_contains_source(&def_source); // TODO: if global context ?
        let unopened_docs = get_unopened_documents(&state, &mut client, &project, &def_loc);

        let mut traverse = async |doc_uri: &Uri, service: &mut async_lsp::ServerSocket| {
            let build = state.get_build(doc_uri).unwrap();
            let def_pos = &def_loc.target_selection_range.start;
            let position = match build.forward_src_position(def_pos, &def_source) {
                Some(pos) => pos,
                None => {
                    let doc_path = state.uri_to_path(doc_uri).unwrap();
                    let doc_path = doc_path.strip_prefix(&project).unwrap_or(&doc_path);
                    let err = format!("Sync doc ({}) failed. Request aborted", doc_path.display());
                    is_sync_doc_failed = true;
                    tracing::error!(err);
                    return Err(Error::request_failed(err)); // FIXME:
                }
            };

            let forwarded_params = lsp::ReferenceParams {
                text_document_position: lsp::TextDocumentPositionParams::new(
                    lsp::TextDocumentIdentifier::new(build.uri.clone()),
                    position,
                ),
                work_done_progress_params: lsp::WorkDoneProgressParams::default(),
                partial_result_params: lsp::PartialResultParams::default(),
                context: lsp::ReferenceContext {
                    include_declaration: true, // the client sends two requests and the second request with false obscures the first response
                },
            };

            let fetch_response = timeout(
                Duration::from_millis(DEFAULT_TIMEOUT_MS),
                fetch_references_for_client_from_build_params(
                    service,
                    &state,
                    &project,
                    forwarded_params,
                    build,
                ),
            )
            .await
            .unwrap_or(Ok(None));

            if let Ok(Some(locations)) = fetch_response {
                for l in locations.into_iter() {
                    workspace_locations.insert(l); // TODO: undistinct links ?
                }
            }

            Ok(())
        };

        // TODO:
        // 1 DEPENDECY tree shaking (via strip common dependencies by def_literal pattern)
        //  1.1 disable tree shaking if def_pos in d.ts
        // impl:
        //  - save tree shaked build in temporary file (!mirate to temporary file)
        //  OR - skip emit if EmitCallback not matched def_literal in there recursive call result
        //     - patch first traverse with pattern matching tree
        //       ,then add second traverse (united SM & content) with exclude non matching dependencies
        //  - impl traverse emit fn without separate sourcemaps & content ctx (inspire by single loop)
        for (i, doc_uri) in unopened_docs.iter().enumerate() {
            let try_open = |service: &mut async_lsp::ServerSocket| {
                let build = state.get_build(doc_uri).unwrap();
                service.did_open(lsp::DidOpenTextDocumentParams {
                    text_document: lsp::TextDocumentItem::new(
                        build.uri.clone(),
                        JS_LANG_ID.into(),
                        1,
                        build.content.clone(),
                    ),
                })
            };

            if state.cancel_received.load() || try_open(&mut service).is_err() {
                state.remove_build(doc_uri);
                continue;
            }

            let doc_path = state.uri_to_path(doc_uri).unwrap();
            let doc_path = doc_path.strip_prefix(&project).unwrap_or(&doc_path);
            let msg = format!("tsserver request {}", doc_path.display());
            let build = state.get_build(doc_uri).unwrap();
            let _ = traverse(doc_uri, &mut service).await;
            let _ = service.did_close(lsp::DidCloseTextDocumentParams {
                text_document: lsp::TextDocumentIdentifier::new(build.uri.clone()),
            });

            state.send_progress(&mut client, (i, unopened_docs.len()), &msg);
            state.remove_build(doc_uri);
            tracing::info!("{msg}");
        }

        for doc_of_build_path in opened_builds_contains_source {
            if state.cancel_received.load() {
                break;
            }
            let doc_uri = state.path_to_uri(&doc_of_build_path).unwrap();
            state.commit_build_changes(&doc_uri, &mut service);
            traverse(&doc_uri, &mut service).await?;
        }

        if state.cancel_received.load() {
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

        Ok(Some(workspace_locations.into_iter().collect()))
    })
}

fn get_unopened_documents(
    state: &Arc<State>,
    client: &mut ClientSocket,
    project: &Path,
    def_loc: &lsp::LocationLink,
) -> Vec<Uri> {
    use ignore::Walk;
    use rayon::prelude::*;

    let def_source = state.get_doc(&def_loc.target_uri).unwrap().source;
    let opened_builds_contains_source = state.get_builds_contains_source(&def_source); // TODO: if global context ?
    let default_sources: Vec<_> = state
        .get_build(&state.get_default_doc())
        .unwrap()
        .sources()
        .iter()
        .map(|s| project.join(s.as_str()))
        .collect();
    tracing::info!("default_sources collected");
    let mut raw_entries = Vec::with_capacity(default_sources.len());
    for entry in Walk::new(project).flatten() {
        if entry.file_type().is_some_and(|ft| ft.is_file()) {
            raw_entries.push(entry.path().to_owned());
        }
    }
    tracing::info!("repository raw_entries walked");
    let doc = |uri| state.get_doc(&uri);
    let (js, decl) = (&JS_FILE_EXT[1..], &DECL_FILE_EXT[1..]);
    let def_lit = get_definition_literal(def_loc, state);
    let matched_docs: Vec<Uri> = raw_entries
        .par_iter()
        .filter(|p| match &state.path_to_uri(p.as_path()).map(doc) {
            Ok(Ok(doc)) => doc.content.contains(&def_lit),
            _ => file_contains_text(p, &def_lit).is_ok_and(|matched| matched),
        })
        .filter(|p| match &state.path_to_uri(p.as_path()).map(doc).is_ok() {
            false => p.extension().is_some_and(|ext| ext == js || ext == decl),
            true => true,
        })
        .filter(|path| !opened_builds_contains_source.contains(&path.to_path_buf()))
        .filter(|path| !default_sources.contains(&path.to_path_buf()))
        .filter_map(|path| Uri::from_file_path(path).ok())
        .collect();
    tracing::info!("matched_docs filled"); // FIXME: too long

    state.send_progress(client, (0, 0), "build project...");
    matched_docs.par_iter().for_each(|doc_uri| {
        let _ = state.set_build(doc_uri);
    });
    tracing::info!("par_iter set_build completed");

    let all_builds_contains_source = state.get_builds_contains_source(&def_source); // TODO: if global context ?

    matched_docs.par_iter().for_each(|d| {
        if !all_builds_contains_source.contains(&state.uri_to_path(d).unwrap()) {
            state.remove_build(d);
        }
    });

    all_builds_contains_source
        .into_par_iter()
        .filter(|p| !opened_builds_contains_source.contains(p))
        .map(|p| state.path_to_uri(&p).unwrap())
        .collect()
}

fn get_definition_literal(def_loc: &lsp::LocationLink, state: &Arc<State>) -> String {
    let def_doc = state.get_doc(&def_loc.target_uri).unwrap();
    let s = def_loc.target_selection_range.start;
    let start_pos = def_doc.buffer.line_to_char(s.line as usize) + s.character as usize;
    let e = def_loc.target_selection_range.end;
    let end_pos = def_doc.buffer.line_to_char(e.line as usize) + e.character as usize;
    def_doc.buffer.slice(start_pos..end_pos).to_string()
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

// TODO: rewrite with config
#[inline]
async fn fetch_references_for_client_from_build_params(
    s: &mut async_lsp::ServerSocket,
    state: &std::sync::Arc<State>,
    project: &std::path::Path,
    build_params: lsp::ReferenceParams,
    build: std::sync::Arc<Build>,
) -> Result<Option<Vec<lsp::Location>>, ResponseError> {
    s.references(build_params)
        .await
        .map_err(Error::internal)
        .map(Option::unwrap)
        .map(|mut r| {
            r.iter_mut().for_each(|l| {
                if build.uri.canonicalize() == l.uri.canonicalize()
                    && let Ok(source) = forward_build_range(&mut l.range, &build)
                {
                    l.uri = state.path_to_uri(&project.join(source.as_str())).unwrap();
                }
            });
            Some(r)
        })
}

fn file_contains_text<P: AsRef<std::path::Path>>(
    filename: P,
    search_term: &str,
) -> anyhow::Result<bool> {
    use std::io::BufRead;

    let file = std::fs::File::open(filename)?;
    let reader = std::io::BufReader::new(file);

    for line_result in reader.lines() {
        let line = line_result?;
        if line.contains(search_term) {
            return Ok(true);
        }
    }

    Ok(false)
}
