use std::ops::ControlFlow;
use tokio::time::{Duration, timeout};

use async_lsp::lsp_types::{Url as Uri, notification as N, request as R};
use async_lsp::lsp_types::{notification::Notification, request::Request};
use async_lsp::{ErrorCode, ResponseError};
use async_lsp::{LanguageClient, LanguageServer, lsp_types as lsp};

use crate::builder::{BUILD_FILE, BUILD_SOURCEMAP_FILE, Build, MODULE_PREFIX};
use crate::proxy::{DECL_FILE_EXT, JS_LANG_ID, Proxy, ResFut, ResReq, ResReqProxy};
use crate::state::{Canonicalize, ToSource, ToSourcePath};

use std::collections::HashSet;
use std::hash::{Hash, Hasher};

impl LanguageServer for Proxy {
    type Error = ResponseError;
    type NotifyResult = ControlFlow<async_lsp::Result<()>>;

    fn initialize(&mut self, params: lsp::InitializeParams) -> ResFut<R::Initialize> {
        if let Some([root_ws, ..]) = params.workspace_folders.as_deref() {
            let project_uri = &root_ws.uri;
            self.state.set_project(project_uri);

            let global_script = params
                .initialization_options
                .as_ref()
                .and_then(|opt| opt.as_object())
                .and_then(|m| m.get("proxy"))
                .and_then(|v| v.as_object())
                .and_then(|p| p.get("globalScript"))
                .and_then(|v| v.as_str());

            if let Some(doc) = global_script {
                let global_doc_uri = Uri::from_file_path(&project_uri.source_path().join(doc))
                    .expect("valid global doc uri");
                let _ = global_doc_uri.try_source_path().map_err(|err| {
                    // TODO: set glscript as constant script
                    // where users add some deps
                    let message = format!(
                        "{}: {} ({}) {} {err}. {}",
                        "GLScript Language Server",
                        "step to setup global script",
                        global_doc_uri.path(),
                        "from config options failed:",
                        "Try to reconfigure options, then restart language server"
                    );
                    let _ = self.client().show_message(lsp::ShowMessageParams {
                        typ: lsp::MessageType::WARNING,
                        message,
                    });
                });
                self.state.set_global_doc(global_doc_uri);
            }
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
        let doc = &params.text_document;
        if doc.language_id == JS_LANG_ID && !doc.uri.source_path().ends_with(BUILD_FILE) {
            self.state.set_doc(&doc.uri, &doc.text);
            let build_with_version = self.state.set_build(&doc.uri);
            let _ = self.server().did_open(lsp::DidOpenTextDocumentParams {
                text_document: lsp::TextDocumentItem::new(
                    build_with_version.build.emit_uri,
                    JS_LANG_ID.into(),
                    build_with_version.version,
                    build_with_version.build.emit_text,
                ),
            });
        } else {
            let _ = self.server().did_open(params);
        }

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
        let mut builds_for_changes = self.state.get_builds_contains_document(uri);
        builds_for_changes
            .sort_by(|a, b| (a != &uri.source_path()).cmp(&(b != &uri.source_path())));

        let mut sources_changed = false;

        assert!(builds_for_changes.contains(&uri.source_path()));

        for ref build_source_path in builds_for_changes {
            let build_uri = &Uri::from_file_path(build_source_path).expect("valid build entry uri");
            let build = self.state.get_build(build_uri).expect("iteration build");
            let mut forward_changes = vec![];

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
                    None => panic!("forward_src_range failed on did_change"),
                };
            }

            let new_build_with_version = self.state.set_build(build_uri);

            if new_build_with_version.build.emit_hash != build.emit_hash {
                sources_changed = true;
            }

            let forward_params = lsp::DidChangeTextDocumentParams {
                text_document: lsp::VersionedTextDocumentIdentifier {
                    uri: new_build_with_version.build.emit_uri.clone(),
                    version: new_build_with_version.version,
                },
                content_changes: if sources_changed {
                    vec![lsp::TextDocumentContentChangeEvent {
                        text: new_build_with_version.build.emit_text,
                        range_length: None,
                        range: None,
                    }]
                } else {
                    forward_changes
                },
            };

            let _ = self.server().did_change(forward_params);
        }

        ControlFlow::Continue(())
    }

    fn did_save(&mut self, params: lsp::DidSaveTextDocumentParams) -> Self::NotifyResult {
        if self.state.get_build(&params.text_document.uri).is_none() {
            let _ = self.server().did_save(params);
        }
        ControlFlow::Continue(())
    }

    fn did_close(&mut self, params: lsp::DidCloseTextDocumentParams) -> Self::NotifyResult {
        match self.state.get_build(&params.text_document.uri) {
            Some(b) => {
                let _ = self.server().did_close(lsp::DidCloseTextDocumentParams {
                    text_document: lsp::TextDocumentIdentifier::new(b.emit_uri),
                });
            }
            None => self.server().did_close(params).expect("did close"),
        }
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

        // TODO: send cancel req on timeout
        let decl_req = self.definition(lsp::GotoDefinitionParams {
            text_document_position_params: lsp::TextDocumentPositionParams::new(
                lsp::TextDocumentIdentifier::new(uri.clone()),
                pos.clone(),
            ),
            work_done_progress_params: lsp::WorkDoneProgressParams::default(),
            partial_result_params: lsp::PartialResultParams::default(),
        });

        Box::pin(async move {
            let uri = &mut params.text_document_position_params.text_document.uri;
            let uri_canonicalized = uri.canonicalize();
            let pos = &mut params.text_document_position_params.position;

            // TODO: create util
            let build_pos = build.forward_src_position(pos, uri);

            if build_pos.is_none() {
                let err = format!("Forward src position `{pos:?}` failed");
                return Err(ResponseError::new(ErrorCode::REQUEST_FAILED, err));
            }

            *pos = build_pos.expect("is some");
            *uri = build.emit_uri.clone();

            let hover: ResReq<R::HoverRequest> = service.hover(params).await;
            let hover = hover.map_err(|e| ResponseError::new(ErrorCode::INTERNAL_ERROR, e))?;

            if hover.is_none() {
                return Ok(None);
            }

            let mut hover = strip_module_hash(hover.expect("is some"));

            if let Some(mut r) = hover.range {
                if !forward_build_range(&mut r, &build)
                    .is_ok_and(|source_uri| source_uri.canonicalize() == uri_canonicalized)
                {
                    hover.range = None
                }
            }

            // TODO: skip awaiting decl on empty hover
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

        let req_build: Build;
        match self.state.get_build(uri) {
            None => {
                return Box::pin(async move {
                    let res: ResReq<R::GotoDefinition> = service.definition(params).await;
                    res.map_err(|e| ResponseError::new(ErrorCode::INTERNAL_ERROR, e))
                });
            }
            Some(b) => req_build = b,
        };

        let req_build_sources = req_build.sources();
        let project = self.state.get_project().clone();
        let state = self.state.clone();

        Box::pin(async move {
            let doc_pos = &mut params.text_document_position_params;
            let uri = &mut doc_pos.text_document.uri;
            let pos = &mut doc_pos.position;

            // TODO: create util
            let req_build_pos = req_build.forward_src_position(pos, uri);

            if req_build_pos.is_none() {
                let err = format!("Forward src position `{pos:?}` failed");
                return Err(ResponseError::new(ErrorCode::REQUEST_FAILED, err));
            }

            *pos = req_build_pos.expect("is some");
            *uri = req_build.emit_uri.clone();

            let res: ResReq<R::GotoDefinition> = service.definition(params).await;
            let res = res.map_err(|e| ResponseError::new(ErrorCode::INTERNAL_ERROR, e));

            if res.is_err() || res.as_ref().expect("is some").is_none() {
                return res;
            }

            let forward_location_links = |links: Vec<lsp::LocationLink>| -> Result<_, _> {
                let mut forward_links = HashSet::with_capacity(links.len());
                for mut link in links {
                    if link.target_uri.as_str().ends_with(DECL_FILE_EXT) {
                        forward_links.insert(HashLocationLink(link));
                        continue;
                    }

                    // TODO: forward build file ?
                    // emit build file with global doc constant to debug anywhere ?
                    if link.target_uri.as_str().ends_with(BUILD_FILE) {
                        continue;
                    }

                    if let Some(ref any_build) = state.get_build_by_emit_uri(&link.target_uri) {
                        let source_uri = forward_build_range(&mut link.target_range, any_build)?;
                        let source = &source_uri.source_path().source(&project);

                        if !req_build_sources.contains(source) {
                            continue;
                        }

                        forward_build_range(&mut link.target_selection_range, any_build)?;

                        link.target_uri = source_uri;
                        link.origin_selection_range = None;
                        forward_links.insert(HashLocationLink(link));
                        continue;
                    }

                    if let Ok(link_source_path) = &link.target_uri.try_source_path() {
                        let link_source = &link_source_path.source(&project);
                        if !req_build_sources.contains(link_source) {
                            continue;
                        }

                        link.origin_selection_range = None;
                        forward_links.insert(HashLocationLink(link));
                    }
                }
                let forward_links = forward_links.into_iter().map(|h| h.0).collect();
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

    fn did_change_watched_files(
        &mut self,
        mut params: lsp::DidChangeWatchedFilesParams,
    ) -> Self::NotifyResult {
        let mut forward_changes = vec![];
        for channge in params.changes {
            let is_build_file = !channge.uri.as_str().ends_with(BUILD_FILE);
            let is_sourcemap_file = !channge.uri.as_str().ends_with(BUILD_SOURCEMAP_FILE);
            let is_build_dep = self.state.get_build(&channge.uri).is_some();

            if !is_build_file && !is_sourcemap_file && !is_build_dep {
                forward_changes.push(channge);
            }
        }

        if forward_changes.is_empty() {
            return ControlFlow::Continue(());
        }

        params.changes = forward_changes;

        let _ = self.server().did_change_watched_files(params);
        ControlFlow::Continue(())
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

#[derive(Debug, Eq)]
struct HashLocationLink(lsp::LocationLink);

impl Hash for HashLocationLink {
    fn hash<H: Hasher>(&self, state: &mut H) {
        if let Some(origin_selection_range) = &self.0.origin_selection_range {
            origin_selection_range.hash(state);
        }
        self.0.target_uri.canonicalize().hash(state);
        self.0.target_range.hash(state);
        self.0.target_selection_range.hash(state);
    }
}

impl PartialEq for HashLocationLink {
    fn eq(&self, other: &Self) -> bool {
        self.0.origin_selection_range == other.0.origin_selection_range
            && self.0.target_selection_range == other.0.target_selection_range
            && self.0.target_range == other.0.target_range
            && self.0.target_uri.canonicalize() == other.0.target_uri.canonicalize()
    }
}
