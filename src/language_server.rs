use std::ops::ControlFlow;

use async_lsp::lsp_types as lsp;
use async_lsp::lsp_types::{Url as Uri, notification as N, request as R};
use async_lsp::lsp_types::{notification::Notification, request::Request};
use async_lsp::{ErrorCode, LanguageServer, ResponseError};

use crate::builder::{BUILD_FILE, Build};
use crate::proxy::{JS_LANG_ID, Proxy, ResFut, ResReq};
use crate::state::{ToSource, ToSourcePath};

impl LanguageServer for Proxy {
    type Error = ResponseError;
    type NotifyResult = ControlFlow<async_lsp::Result<()>>;

    fn initialize(&mut self, params: lsp::InitializeParams) -> ResFut<R::Initialize> {
        if let Some([root_ws, ..]) = params.workspace_folders.as_deref() {
            self.state.set_project(&root_ws.uri);
        }

        let mut service = self.server();
        Box::pin(async move {
            let res = service.initialize(params).await;
            res.map_err(|e| ResponseError::new(ErrorCode::INTERNAL_ERROR, e))
        })
    }

    fn initialized(&mut self, params: lsp::InitializedParams) -> Self::NotifyResult {
        let _ = self.server().initialized(params);
        ControlFlow::Continue(())
    }

    fn shutdown(&mut self, (): <R::Shutdown as Request>::Params) -> ResFut<R::Shutdown> {
        let _ = self.server().shutdown(());
        Box::pin(async move { Ok(()) })
    }

    fn exit(&mut self, (): <N::Exit as Notification>::Params) -> Self::NotifyResult {
        let _ = self.server().exit(());
        ControlFlow::Break(Ok(()))
    }

    fn did_open(&mut self, params: lsp::DidOpenTextDocumentParams) -> Self::NotifyResult {
        let mut doc = params.text_document;
        if doc.language_id == JS_LANG_ID && !doc.uri.source_path().ends_with(BUILD_FILE) {
            self.state.set_doc(&doc.uri, &doc.text);
            let build_with_version = self.state.set_build(&doc.uri);
            doc.text = build_with_version.build.text;
            doc.version = build_with_version.version;
        }
        let params = lsp::DidOpenTextDocumentParams { text_document: doc };
        let _ = self.server().did_open(params);
        ControlFlow::Continue(())
    }

    fn did_change(&mut self, params: lsp::DidChangeTextDocumentParams) -> Self::NotifyResult {
        let uri = &params.text_document.uri;

        if self.state.get_build(uri).is_none() {
            let _ = self.server().did_change(params);
            return ControlFlow::Continue(());
        }

        // 1. apply changes to raw document
        if let Some(mut text) = self.state.get_doc(uri) {
            let changes = &params.content_changes;
            if text.is_empty() {
                text = changes.into_iter().fold("".into(), |mut acc: String, c| {
                    acc.push_str(&c.text.replace("\r\n", "\n"));
                    acc
                });
            } else {
                let mut changes = changes.to_vec();
                changes.sort_by(|a, b| {
                    let ra = a.range.unwrap();
                    let rb = b.range.unwrap();
                    (ra.start.line, ra.start.character).cmp(&(rb.start.line, rb.start.character))
                });

                for change in changes.into_iter().rev() {
                    text.ends_with("\n").then(|| text.push_str("\n"));
                    let lsp::Range { start, end } = change.range.unwrap();
                    let mut lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
                    let start_line = &mut lines[start.line as usize];
                    let left = start_line[..start.character as usize].to_string();
                    let end_line = &mut lines[end.line as usize];
                    let right = end_line[end.character as usize..].to_string();
                    let ctext = &change.text.clone().replace("\r\n", "\n");
                    let replacement = format!("{left}{ctext}{right}");
                    lines.splice(start.line as usize..=end.line as usize, [replacement]);
                    text = lines.join("\n");
                }
            }

            self.state.set_doc(uri, &text);
        }

        // 2. forward params into language server
        let mut current_build_traversed = false;

        for ref build_source_path in self.state.get_builds_contains_document(uri) {
            if build_source_path == &uri.source_path() {
                current_build_traversed = true
            }

            let build_uri = &Uri::from_file_path(build_source_path).unwrap();
            let build = self.state.get_build(build_uri).expect("iteration build");
            let build_sources = build.sources().clone();

            let mut forward_params = lsp::DidChangeTextDocumentParams {
                text_document: lsp::VersionedTextDocumentIdentifier {
                    uri: build_uri.clone(),
                    version: 0, // will rewrite later
                },
                content_changes: vec![],
            };

            let forward_changes = &mut forward_params.content_changes;
            let mut has_forward_err = false;

            for change in &params.content_changes {
                if change.range.is_none() {
                    continue;
                }

                match build.forward_src_range(&change.range.unwrap(), uri) {
                    Some(r) => forward_changes.push(lsp::TextDocumentContentChangeEvent {
                        range: Some(r),
                        range_length: change.range_length, // TODO: validate length with escape \r\n
                        text: change.text.replace("\r\n", "\n"),
                    }),
                    None => has_forward_err = true,
                };
            }

            let new_build_with_version = self.state.set_build(build_uri);

            forward_params.text_document.version = new_build_with_version.version;

            if new_build_with_version.build.sources() != build_sources || /* FIXME: */ has_forward_err
            {
                *forward_changes = vec![lsp::TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: new_build_with_version.build.text,
                }];
            }

            let _ = self.server().did_change(forward_params);
        }

        assert!(
            current_build_traversed,
            "{} should be traversed",
            uri.source_path().source(self.state.get_project())
        );

        ControlFlow::Continue(())
    }

    fn did_save(&mut self, mut params: lsp::DidSaveTextDocumentParams) -> Self::NotifyResult {
        let uri = &params.text_document.uri;
        if let (_, Some(build)) = (params.text.is_some(), self.state.get_build(uri)) {
            params.text = Some(build.text);
        }
        let _ = self.server().did_save(params);
        ControlFlow::Continue(())
    }

    fn did_close(&mut self, params: lsp::DidCloseTextDocumentParams) -> Self::NotifyResult {
        let _ = self.server().did_close(params);
        ControlFlow::Continue(())
    }

    fn hover(&mut self, mut params: lsp::HoverParams) -> ResFut<R::HoverRequest> {
        let mut service = self.server();
        let uri = &params.text_document_position_params.text_document.uri;

        let build: Build;
        match self.state.get_build(uri) {
            None => {
                return Box::pin(async move {
                    let res: ResReq<R::HoverRequest> = service.hover(params).await;
                    res.map_err(|e| ResponseError::new(ErrorCode::INTERNAL_ERROR, e))
                });
            }
            Some(b) => build = b,
        };

        Box::pin(async move {
            let uri = &params.text_document_position_params.text_document.uri;
            let pos = &mut params.text_document_position_params.position;

            if let Some(forwarded_pos) = build.forward_src_position(pos, uri) {
                *pos = forwarded_pos;
                let hover_response: ResReq<R::HoverRequest> = service.hover(params).await;
                // FIXME: deduplicate function signature: tsx```<signature><signature>```
                // ```tsx\nfunction sum(a: number, b: number): number\nfunction sum(a: number, b: number): number\n```
                hover_response.map_err(|e| ResponseError::new(ErrorCode::INTERNAL_ERROR, e))
            } else {
                let err = format!("Forward src position `{pos:?}` failed"); // FIXME: skip include stmt
                Err(ResponseError::new(ErrorCode::REQUEST_FAILED, err))
            }
        })
    }

    fn definition(&mut self, mut params: lsp::GotoDefinitionParams) -> ResFut<R::GotoDefinition> {
        let mut service = self.server();
        let uri = &params.text_document_position_params.text_document.uri;

        let build: Build;
        match self.state.get_build(uri) {
            None => {
                return Box::pin(async move {
                    let res: ResReq<R::GotoDefinition> = service.definition(params).await;
                    res.map_err(|e| ResponseError::new(ErrorCode::INTERNAL_ERROR, e))
                });
            }
            Some(b) => build = b,
        };

        let build_sources = build.sources();
        let project = self.state.get_project().clone();

        Box::pin(async move {
            let doc_pos = &mut params.text_document_position_params;
            let uri = &doc_pos.text_document.uri.clone();
            let pos = &mut doc_pos.position;
            let forward_pos = build.forward_src_position(pos, uri);

            if forward_pos.is_none() {
                let err = format!("Forward src position `{pos:?}` failed"); // FIXME: skip include stmt
                return Err(ResponseError::new(ErrorCode::REQUEST_FAILED, err));
            }

            *pos = forward_pos.unwrap();

            let res: ResReq<R::GotoDefinition> = service.definition(params).await;
            let res = res.map_err(|e| ResponseError::new(ErrorCode::INTERNAL_ERROR, e));

            if res.is_err() {
                return res;
            }

            if res.is_ok() && res.as_ref().unwrap().is_none() {
                return res;
            }

            fn forward_range(range: &mut lsp::Range, build: &Build) -> Result<Uri, ResponseError> {
                let source_range = build.forward_build_range(&range);
                if source_range.is_none() {
                    let err = format!("Forward back build range `{:?}` failed", range); // FIXME: skip include stmt
                    return Err(ResponseError::new(ErrorCode::REQUEST_FAILED, err));
                }
                let source_range = source_range.unwrap();
                *range = source_range.0;
                Ok(source_range.1)
            }

            let ts_definition_response = res.unwrap().unwrap();
            let forward_res: lsp::GotoDefinitionResponse = match ts_definition_response {
                lsp::GotoDefinitionResponse::Scalar(mut location) if &location.uri == uri => {
                    match forward_range(&mut location.range, &build) {
                        Ok(uri) => location.uri = uri,
                        Err(err) => return Err(err),
                    };
                    lsp::GotoDefinitionResponse::Scalar(location)
                }
                lsp::GotoDefinitionResponse::Array(locations) => {
                    let mut forward_locations = vec![];
                    for loc in locations {
                        let mut forward_loc;

                        // emitted reference
                        if &loc.uri == uri {
                            forward_loc = loc.clone();
                            match forward_range(&mut forward_loc.range, &build) {
                                Ok(uri) => forward_loc.uri = uri,
                                Err(err) => return Err(err),
                            };
                            forward_locations.push(forward_loc);
                            continue;
                        }

                        let loc_source = &loc.uri.source_path().source(&project);
                        let is_build_file = loc.uri.source_path().ends_with(BUILD_FILE);
                        if build_sources.contains(loc_source) || is_build_file {
                            continue; // skip duplicated source references
                        }

                        forward_locations.push(loc.to_owned()); // .d.ts reference
                    }
                    lsp::GotoDefinitionResponse::Array(forward_locations)
                }
                lsp::GotoDefinitionResponse::Link(location_links) => {
                    let mut forward_locations = vec![];
                    for loc in location_links {
                        let mut forward_loc;

                        // emitted reference
                        if &loc.target_uri == uri {
                            forward_loc = loc.clone();

                            match forward_range(&mut forward_loc.target_range, &build) {
                                Ok(uri) => forward_loc.target_uri = uri,
                                Err(err) => return Err(err),
                            };
                            match forward_range(&mut forward_loc.target_selection_range, &build) {
                                Ok(_) => {}
                                Err(err) => return Err(err),
                            };
                            if let Some(mut range) = forward_loc.origin_selection_range {
                                match forward_range(&mut range, &build) {
                                    Ok(_) => {}
                                    Err(_) => forward_loc.origin_selection_range = None,
                                    //  ^ fired on include path definition req
                                };
                            }

                            forward_locations.push(forward_loc);
                            continue;
                        }

                        let loc_source = &loc.target_uri.source_path().source(&project);
                        let is_build_file = loc.target_uri.source_path().ends_with(BUILD_FILE);
                        if build_sources.contains(loc_source) || is_build_file {
                            continue; // skip duplicated source references
                        }

                        forward_locations.push(loc.to_owned()); // .d.ts reference
                    }
                    lsp::GotoDefinitionResponse::Link(forward_locations)
                }
                _ => ts_definition_response, // .d.ts reference
            };

            Ok(Some(forward_res))
        })
    }
}
