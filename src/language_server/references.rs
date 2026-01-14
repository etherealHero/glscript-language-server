use async_lsp::lsp_types::{Url as Uri, request as R};
use async_lsp::{LanguageClient, LanguageServer, lsp_types as lsp};
use tokio::time::{Duration, timeout};

use crate::builder::Build;
use crate::language_server::{DefRes, Error, definition_params, forward_build_range};
use crate::proxy::{Canonicalize, Proxy, ResFut};
use crate::proxy::{DECL_FILE_EXT, DEFAULT_TIMEOUT_MS, JS_FILE_EXT, JS_LANG_ID};
use crate::types::Source;
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
        let definition_response = definition_request.await;
        let def_loc = {
            let message = "Definition of references request not found";
            match definition_response {
                Ok(Some(ref definition)) => match definition {
                    DefRes::Link(links) => match links.first() {
                        Some(def_loc) => def_loc,
                        None => return Err(Error::request_failed(message)),
                    },
                    _ => unreachable!(),
                },
                Ok(None) => return Err(Error::request_failed(message)),
                Err(err) => return Err(err),
            }
        };

        let fetch = async |service: &mut async_lsp::ServerSocket,
                           build_params: lsp::ReferenceParams,
                           build: std::sync::Arc<Build>| {
            service
                .references(build_params)
                .await
                .map_err(Error::internal)
                .map(|r| r.unwrap())
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
        };

        let definition_source_path = def_loc.target_uri.as_str();
        if definition_source_path.ends_with(DECL_FILE_EXT) {
            let doc_pos = &mut params.text_document_position;
            try_forward_text_document_position_params!(state, req_build, doc_pos);
            return fetch(&mut service, params, req_build).await;
        }

        if !definition_source_path.ends_with(JS_FILE_EXT) {
            let err = format!(
                "Missmatched definition source extension,
                expect '{JS_FILE_EXT}' or '{DECL_FILE_EXT}'.
                References request aborted"
            );
            let err = err.split_whitespace().collect::<Vec<_>>().join(" ");
            return Err(Error::request_failed(err));
        }

        let mut workspace_locations = std::collections::HashSet::new();
        let mut is_sync_doc_failed = false;
        let def_doc = state.get_doc(&def_loc.target_uri).unwrap();
        let opened_builds_contains_source = state.get_builds_contains_source(&def_doc.source); // TODO: if global context ?
        let def_pos = &def_loc.target_selection_range.start;
        let def_literal = {
            let s = def_loc.target_selection_range.start;
            let start_pos = def_doc.buffer.line_to_char(s.line as usize) + s.character as usize;
            let e = def_loc.target_selection_range.end;
            let end_pos = def_doc.buffer.line_to_char(e.line as usize) + e.character as usize;
            def_doc.buffer.slice(start_pos..end_pos).to_string()
        };

        let mut traverse = async |doc_uri: &Uri, service: &mut async_lsp::ServerSocket| {
            let build = state.get_build(doc_uri).unwrap();
            let position = match build.forward_src_position(def_pos, &def_doc.source) {
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
                fetch(service, forwarded_params, build),
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

        let unopened_docs: Vec<Uri> = {
            use ignore::Walk;
            use rayon::prelude::*;

            let mut matched_docs = vec![];
            let default_sources = state.get_build(&state.get_default_doc()).unwrap().sources();
            let sources_len = Walk::new(&project).count();
            for (i, entry) in Walk::new(&project).flatten().enumerate() {
                if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                    continue;
                }

                let path = entry.path();

                if i % 10 == 0 {
                    let msg = format!("scan {}", path.display());
                    state.send_progress(&mut client, (i, sources_len), &msg);
                }

                if !file_contains_text(path, &def_literal).is_ok_and(|matched| matched) {
                    continue;
                };

                let source = Source::from_path(path, &project);
                if !source
                    .as_ref()
                    .is_ok_and(|s| s.ends_with(JS_FILE_EXT) || s.ends_with(DECL_FILE_EXT))
                {
                    continue;
                }

                if opened_builds_contains_source.contains(&path.to_path_buf()) {
                    continue;
                }

                if source.as_ref().is_ok_and(|s| default_sources.contains(s)) {
                    continue;
                }

                matched_docs.push(Uri::from_file_path(path).unwrap());
            }

            state.send_progress(&mut client, (0, 0), "build project...");
            matched_docs.par_iter().for_each(|doc_uri| {
                state.set_build(doc_uri);
            });

            let all_builds_contains_source = state.get_builds_contains_source(&def_doc.source); // TODO: if global context ?

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
        };

        // TODO: check (build def_literal slice) == (req def_literal)
        //
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
