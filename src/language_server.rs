use std::ops::ControlFlow;
use tokio::time::{Duration, timeout};

use async_lsp::lsp_types as lsp;
use async_lsp::lsp_types::{Url as Uri, notification as N, request as R};
use async_lsp::lsp_types::{notification::Notification, request::Request};
use async_lsp::{ErrorCode, LanguageServer, ResponseError};

use crate::builder::{BUILD_FILE, Build, MODULE_PREFIX};
use crate::proxy::{DECL_FILE_EXT, JS_LANG_ID, Proxy, ResFut, ResReq, ResReqProxy};
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
                    let ra = a.range.expect("is some");
                    let rb = b.range.expect("is some");
                    (ra.start.line, ra.start.character).cmp(&(rb.start.line, rb.start.character))
                });

                for change in changes.into_iter().rev() {
                    text.ends_with("\n").then(|| text.push_str("\n"));
                    let lsp::Range { start, end } = change.range.expect("is some");
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

            let build_uri = &Uri::from_file_path(build_source_path).expect("valid build entry uri");
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

                match build.forward_src_range(&change.range.expect("is some"), uri) {
                    Some(r) => forward_changes.push(lsp::TextDocumentContentChangeEvent {
                        range: Some(r),
                        range_length: change.range_length,
                        text: change.text.replace("\r\n", "\n"),
                    }),
                    None => has_forward_err = true,
                };
            }

            let new_build_with_version = self.state.set_build(build_uri);

            forward_params.text_document.version = new_build_with_version.version;

            assert!(!has_forward_err);

            if new_build_with_version.build.sources() != build_sources {
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
        let pos = &params.text_document_position_params.position;

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

        let decl_req = self.definition(lsp::GotoDefinitionParams {
            text_document_position_params: lsp::TextDocumentPositionParams::new(
                lsp::TextDocumentIdentifier::new(uri.clone()),
                pos.clone(),
            ),
            work_done_progress_params: lsp::WorkDoneProgressParams::default(),
            partial_result_params: lsp::PartialResultParams::default(),
        });

        Box::pin(async move {
            let uri = &params.text_document_position_params.text_document.uri;
            let uri_clone = uri.clone();
            let pos = &mut params.text_document_position_params.position;

            let forward = build.forward_src_position(pos, uri);

            if forward.is_none() {
                let err = format!("Forward src position `{pos:?}` failed");
                return Err(ResponseError::new(ErrorCode::REQUEST_FAILED, err));
            }

            forward.map(|forward_pos| *pos = forward_pos);

            let hover: ResReq<R::HoverRequest> = service.hover(params).await;
            let hover = hover.map_err(|e| ResponseError::new(ErrorCode::INTERNAL_ERROR, e))?;

            if hover.is_none() {
                return Ok(None);
            }

            let mut hover = strip_module_hash(hover.expect("is some"));

            if let Some(mut r) = hover.range {
                if !forward_build_range(&mut r, &build).is_ok_and(|uri| uri == uri_clone) {
                    hover.range = None
                }
            }

            let decl: ResReqProxy<R::GotoDefinition> =
                timeout(Duration::from_millis(200), decl_req)
                    .await
                    .unwrap_or(Ok(None));

            if matches!(decl, Ok(Some(DefRes::Link(l))) if l.is_empty()) {
                let msg = "⚠ No definiion available for this item.";
                return Ok(Some(prepend_hover(hover, msg)));
            }

            Ok(Some(hover))
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
                let err = format!("Forward src position `{pos:?}` failed");
                return Err(ResponseError::new(ErrorCode::REQUEST_FAILED, err));
            }

            *pos = forward_pos.expect("is some");

            let res: ResReq<R::GotoDefinition> = service.definition(params).await;
            let res = res.map_err(|e| ResponseError::new(ErrorCode::INTERNAL_ERROR, e));

            if res.is_err() || res.as_ref().expect("is some").is_none() {
                return res;
            }

            let forward_location_links = |links: Vec<lsp::LocationLink>| -> Result<DefRes, _> {
                let mut forward_links = vec![];
                for mut link in links {
                    if &link.target_uri == uri {
                        let source_uri = forward_build_range(&mut link.target_range, &build)?;
                        link.target_uri = source_uri;

                        forward_build_range(&mut link.target_selection_range, &build)?;

                        if link.origin_selection_range.is_none() {
                            forward_links.push(link);
                            continue;
                        }

                        let origin = &mut link.origin_selection_range.expect("is some");
                        if forward_build_range(origin, &build).is_err() {
                            link.origin_selection_range = None;
                        }

                        forward_links.push(link);
                        continue;
                    }

                    let link_source = &link.target_uri.source_path().source(&project);
                    let is_build_file = link.target_uri.source_path().ends_with(BUILD_FILE);

                    if build_sources.contains(link_source) || is_build_file {
                        continue;
                    }

                    if link_source.ends_with(DECL_FILE_EXT) {
                        forward_links.push(link.to_owned());
                    }
                }
                Ok(DefRes::Link(forward_links))
            };

            let ts_definition_response = res?.expect("is some");
            let forward_res: DefRes = match ts_definition_response {
                DefRes::Link(location_links) => forward_location_links(location_links)?,
                DefRes::Scalar(location) => forward_location_links(vec![lsp::LocationLink {
                    origin_selection_range: None,
                    target_uri: location.uri.clone(),
                    target_range: location.range,
                    target_selection_range: location.range,
                }])?,
                DefRes::Array(locations) => forward_location_links(
                    locations
                        .iter()
                        .map(|l| lsp::LocationLink {
                            origin_selection_range: None,
                            target_uri: l.uri.clone(),
                            target_range: l.range,
                            target_selection_range: l.range,
                        })
                        .collect(),
                )?,
            };

            Ok(Some(forward_res))
        })
    }
}

type DefRes = lsp::GotoDefinitionResponse;

#[inline]
fn forward_build_range(range: &mut lsp::Range, build: &Build) -> Result<Uri, ResponseError> {
    let source_range = build.forward_build_range(&range);
    if source_range.is_none() {
        let err = format!("Forward back build range `{:?}` failed", range);
        return Err(ResponseError::new(ErrorCode::REQUEST_FAILED, err));
    }
    let source_range = source_range.expect("is some");
    *range = source_range.0;
    Ok(source_range.1)
}

#[inline]
fn prepend_hover(mut hover: lsp::Hover, msg: &str) -> lsp::Hover {
    type H = lsp::HoverContents;
    type S = lsp::MarkedString;

    match &mut hover.contents {
        H::Scalar(S::String(s)) => {
            let mut new = msg.to_string();
            new.push_str("\n\n");
            new.push_str(s.clone().as_str());
            *s = new;
        }
        H::Scalar(S::LanguageString(s)) => s.value = format!("{msg}\n\n{}", s.value),
        H::Array(ms) => ms.insert(0, S::String(msg.to_string())),
        H::Markup(m) => m.value = format!("{msg}\n\n{}", m.value),
    };

    hover
}

#[inline]
fn strip_module_hash(mut hover: lsp::Hover) -> lsp::Hover {
    type H = lsp::HoverContents;
    type S = lsp::MarkedString;

    let re = regex::Regex::new(&format!(r"{}\w+", regex::escape(MODULE_PREFIX))).expect("valid re");
    let patch = |s: &str| re.replace_all(s, "MODULE").to_string();

    match &mut hover.contents {
        H::Scalar(S::String(s)) => *s = patch(s),
        H::Scalar(S::LanguageString(s)) => s.value = patch(&s.value),
        H::Array(items) => {
            for item in items {
                match item {
                    S::String(s) => *s = patch(s),
                    S::LanguageString(ls) => ls.value = patch(&ls.value),
                }
            }
        }
        H::Markup(m) => m.value = patch(&m.value),
    }

    hover
}
