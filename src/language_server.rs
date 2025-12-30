use std::fs;
use std::ops::ControlFlow;
use std::sync::Arc;
use tokio::time::{Duration, timeout};

use async_lsp::lsp_types::{Url as Uri, notification as N, request as R};
use async_lsp::lsp_types::{notification::Notification, request::Request};
use async_lsp::{ErrorCode, ResponseError};
use async_lsp::{LanguageServer, lsp_types as lsp};

use crate::builder::{BUILD_FILE, Build};
use crate::types::{IDENTIFIER_PREFIX, Source};

use crate::proxy::{Canonicalize, DECL_FILE_EXT, JS_FILE_EXT, JS_LANG_ID, PROXY_WORKSPACE};
use crate::proxy::{Proxy, ResFut, ResReq, ResReqProxy};
use crate::{try_ensure_build, try_forward_text_document_position_params};

use std::collections::HashSet;
use std::hash::{Hash, Hasher};

impl LanguageServer for Proxy {
    type Error = ResponseError;
    type NotifyResult = ControlFlow<async_lsp::Result<()>>;

    fn initialize(&mut self, mut params: lsp::InitializeParams) -> ResFut<R::Initialize> {
        const _GLSCRIPT_INDEXING_TOKEN: &str = "glscript_indexing";
        const JSCONFIG: &str = "jsconfig.json";

        // FIXME: if node_modules not installed show user error
        // FIXME: if client not has workspace_folders show client Error message for open Project Folder
        // TODO: if workspace_folders.len() > 2 show error for unsupported
        if let Some([root_ws, ..]) = params.workspace_folders.as_deref_mut() {
            let ws_dir = &root_ws.uri.to_file_path().unwrap();
            let proxy_ws_dir = &mut ws_dir.clone().join(PROXY_WORKSPACE);

            fs::create_dir_all(&proxy_ws_dir).unwrap();
            fs::copy(ws_dir.join(JSCONFIG), proxy_ws_dir.join(JSCONFIG)).unwrap();

            self.state.set_project(&root_ws.uri);

            let _ = fs::File::create_new(self.state.get_default_doc().to_file_path().unwrap());

            self.state.set_build(&self.state.get_default_doc());

            root_ws.uri = Uri::from_directory_path(proxy_ws_dir).unwrap();
        }

        #[allow(deprecated)]
        {
            params.root_path = None;
            params.root_uri = None;
        }

        let mut service = self.server();
        Box::pin(async move {
            let initialize_res = service.initialize(params).await;
            initialize_res.map_err(|e| ResponseError::new(ErrorCode::INTERNAL_ERROR, e))
        })
    }

    fn initialized(&mut self, params: lsp::InitializedParams) -> Self::NotifyResult {
        let _ = self.server().initialized(params);
        ControlFlow::Continue(())
    }

    fn shutdown(&mut self, (): <R::Shutdown as Request>::Params) -> ResFut<R::Shutdown> {
        let mut service = self.server();
        Box::pin(async move {
            let _ = service.shutdown(()).await;
            Ok(())
        })
    }

    fn exit(&mut self, (): <N::Exit as Notification>::Params) -> Self::NotifyResult {
        let _ = self.server().exit(());
        ControlFlow::Break(Ok(()))
    }

    fn did_open(&mut self, params: lsp::DidOpenTextDocumentParams) -> Self::NotifyResult {
        let doc = &params.text_document;
        if doc.language_id == JS_LANG_ID && !doc.uri.as_str().ends_with(BUILD_FILE) {
            self.state.set_doc(
                &doc.uri,
                &[lsp::TextDocumentContentChangeEvent {
                    text: doc.text.clone(),
                    range_length: None,
                    range: None,
                }],
            );
            let build_with_version = self.state.set_build(&doc.uri);
            let _ = self.server().did_open(lsp::DidOpenTextDocumentParams {
                text_document: lsp::TextDocumentItem::new(
                    build_with_version.build.uri.clone(),
                    JS_LANG_ID.into(),
                    build_with_version.version,
                    build_with_version.build.content.clone(),
                ),
            });
        } else {
            let _ = self.server().did_open(params);
        }

        ControlFlow::Continue(())
    }

    #[tracing::instrument(skip_all)]
    fn did_change(&mut self, params: lsp::DidChangeTextDocumentParams) -> Self::NotifyResult {
        let uri = &params.text_document.uri;
        let mut service = self.server();

        if self.state.get_build(uri).is_none() {
            let _ = service.did_change(params);
            return ControlFlow::Continue(());
        }

        let doc = self.state.get_doc(uri).unwrap();
        let hash_prev = doc.dependency_hash;

        // 1. apply changes to raw document
        self.state.set_doc(uri, &params.content_changes);
        let hash_new = self.state.get_doc(uri).unwrap().dependency_hash;
        let dep_changed = hash_prev != hash_new;

        // 2. forward params into language server
        let builds_contains_source = self.state.get_builds_contains_source(&doc.source);
        for doc_of_build_path in builds_contains_source {
            let params = params.clone();
            self.state
                .add_client_doc_changes(doc_of_build_path, params, dep_changed);
        }

        // 3. commit req doc
        self.state.commit_build_changes(uri, &mut service);
        ControlFlow::Continue(())
    }

    fn did_save(&mut self, params: lsp::DidSaveTextDocumentParams) -> Self::NotifyResult {
        if self.state.get_build(&params.text_document.uri).is_none() {
            let _ = self.server().did_save(params);
        }
        ControlFlow::Continue(())
    }

    fn did_close(&mut self, params: lsp::DidCloseTextDocumentParams) -> Self::NotifyResult {
        let uri = &params.text_document.uri;
        if let Some(build) = self.state.get_build(uri) {
            let _ = self.server().did_close(lsp::DidCloseTextDocumentParams {
                text_document: lsp::TextDocumentIdentifier::new(build.uri.clone()),
            });
            self.state.remove_build(uri);
        } else {
            self.server().did_close(params).expect("did close")
        }
        ControlFlow::Continue(())
    }

    fn hover(&mut self, mut params: lsp::HoverParams) -> ResFut<R::HoverRequest> {
        let mut service = self.server();
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = &params.text_document_position_params.position;
        let build = try_ensure_build!(self, uri, params, hover);

        // TODO: send cancel req on timeout
        let decl_req = self.definition(lsp::GotoDefinitionParams {
            text_document_position_params: lsp::TextDocumentPositionParams::new(
                lsp::TextDocumentIdentifier::new(uri.clone()),
                pos.to_owned(),
            ),
            work_done_progress_params: lsp::WorkDoneProgressParams::default(),
            partial_result_params: lsp::PartialResultParams::default(),
        });
        let state = self.state.clone();
        let req_source = state.get_doc(uri).unwrap().source.clone();

        Box::pin(async move {
            let doc_pos = &mut params.text_document_position_params;
            try_forward_text_document_position_params!(state, build, doc_pos);

            let hover: ResReq<R::HoverRequest> = service.hover(params).await;
            let hover = hover.map_err(|e| ResponseError::new(ErrorCode::INTERNAL_ERROR, e))?;

            if hover.is_none() {
                return Ok(None);
            }

            let mut hover = strip_module_hash(hover.expect("is some"));

            if let Some(mut r) = hover.range
                && !forward_build_range(&mut r, &build).is_ok_and(|source| source == *req_source)
            {
                hover.range = None
            }

            // TODO: skip awaiting decl on empty hover. ^^^ Check hover.is_none()
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
        let req_build = try_ensure_build!(self, uri, params, definition);
        let req_build_sources = req_build.sources();
        let state = self.state.clone();

        Box::pin(async move {
            let doc_pos = &mut params.text_document_position_params;
            try_forward_text_document_position_params!(state, req_build, doc_pos);

            let res: ResReq<R::GotoDefinition> = service.definition(params).await;
            let res = res.map_err(|e| ResponseError::new(ErrorCode::INTERNAL_ERROR, e));

            if res.is_err() || res.as_ref().expect("is some").is_none() {
                return res;
            }

            let project = state.get_project();
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
                        let source = forward_build_range(&mut link.target_range, any_build)?;

                        if !req_build_sources.contains(&source) {
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
                        if !req_build_sources.contains(&*doc.source) {
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
            let is_build_dep = self.state.get_build(&channge.uri).is_some();

            if is_build_file || is_build_dep {
                continue;
            }

            forward_changes.push(channge);
        }

        if forward_changes.is_empty() {
            return ControlFlow::Continue(());
        }

        params.changes = forward_changes;

        let _ = self.server().did_change_watched_files(params);
        ControlFlow::Continue(())
    }

    fn code_lens(&mut self, params: lsp::CodeLensParams) -> ResFut<R::CodeLensRequest> {
        let uri = &params.text_document.uri;
        try_ensure_build!(self, uri, params, code_lens);
        Box::pin(async move { Ok(Some(vec![])) })
    }

    fn completion(&mut self, mut params: lsp::CompletionParams) -> ResFut<R::Completion> {
        let mut service = self.server();
        let uri = &params.text_document_position.text_document.uri;
        let build = try_ensure_build!(self, uri, params, completion);
        let state = self.state.clone();
        Box::pin(async move {
            type Res = lsp::CompletionResponse;
            let forward = forward_build_completion_item;
            let doc_pos = &mut params.text_document_position;
            try_forward_text_document_position_params!(state, build, doc_pos);

            service
                .completion(params)
                .await
                .map_err(|e| ResponseError::new(ErrorCode::INTERNAL_ERROR, e))
                .map(|r| r.unwrap())
                .map(|mut response| {
                    match response {
                        Res::Array(ref mut items) => items.iter_mut().for_each(forward),
                        Res::List(ref mut list) => list.items.iter_mut().for_each(forward),
                    };
                    Some(response)
                })
        })
    }

    fn completion_item_resolve(
        &mut self,
        params: lsp::CompletionItem,
    ) -> ResFut<R::ResolveCompletionItem> {
        let mut service = self.server();
        Box::pin(async move {
            service
                .completion_item_resolve(params)
                .await
                .map_err(|e| ResponseError::new(ErrorCode::INTERNAL_ERROR, e))
                .map(|mut res| {
                    forward_build_completion_item(&mut res);
                    res
                })
        })
    }

    fn signature_help(
        &mut self,
        mut params: lsp::SignatureHelpParams,
    ) -> ResFut<R::SignatureHelpRequest> {
        let mut service = self.server();
        let uri = &params.text_document_position_params.text_document.uri;
        let build = try_ensure_build!(self, uri, params, signature_help);
        let state = self.state.clone();
        Box::pin(async move {
            let doc_pos = &mut params.text_document_position_params;
            try_forward_text_document_position_params!(state, build, doc_pos);
            service
                .signature_help(params)
                .await
                .map_err(|e| ResponseError::new(ErrorCode::INTERNAL_ERROR, e))
        })
    }

    fn references(&mut self, mut params: lsp::ReferenceParams) -> ResFut<R::References> {
        // the client sends two requests
        // and the second request with false obscures the first response
        let context = lsp::ReferenceContext {
            include_declaration: true,
        };

        let uri = &params.text_document_position.text_document.uri;
        let pos = &params.text_document_position.position;
        let req_build = try_ensure_build!(self, uri, params, references);
        let definition_request = self.definition(lsp::GotoDefinitionParams {
            text_document_position_params: lsp::TextDocumentPositionParams::new(
                lsp::TextDocumentIdentifier::new(uri.clone()),
                pos.to_owned(),
            ),
            work_done_progress_params: lsp::WorkDoneProgressParams::default(),
            partial_result_params: lsp::PartialResultParams::default(),
        });

        let mut service = self.server();
        let state = self.state.clone();

        Box::pin(async move {
            let project = state.get_project();
            let response = definition_request.await.map(|r| r.unwrap());
            let definition = match response {
                Ok(ref definition_response) => match definition_response {
                    DefRes::Link(links) => links.first(),
                    _ => unreachable!(),
                },
                Err(err) => return Err(err),
            };

            let fetch = async |service: &mut async_lsp::ServerSocket,
                               build_params: lsp::ReferenceParams,
                               build: Arc<Build>| {
                service
                    .references(build_params)
                    .await
                    .map_err(|e| ResponseError::new(ErrorCode::INTERNAL_ERROR, e))
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

            if let Some(def_loc) = definition {
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
                    return Err(ResponseError::new(ErrorCode::REQUEST_FAILED, err));
                }

                let mut workspace_locations = HashSet::new();
                let def_source = state.get_doc(&def_loc.target_uri).unwrap().source;
                let def_pos = &def_loc.target_selection_range.start;
                let builds_contains_source = state.get_builds_contains_source(&def_source);
                for doc_of_build_path in builds_contains_source {
                    let doc_uri = state.path_to_uri(&doc_of_build_path).unwrap();
                    let build = state.get_build(&doc_uri).unwrap();
                    let forwarded_params = lsp::ReferenceParams {
                        text_document_position: lsp::TextDocumentPositionParams::new(
                            lsp::TextDocumentIdentifier::new(build.uri.clone()),
                            build.forward_src_position(def_pos, &def_source).unwrap(),
                        ),
                        work_done_progress_params: lsp::WorkDoneProgressParams::default(),
                        partial_result_params: lsp::PartialResultParams::default(),
                        context,
                    };

                    let source_references = fetch(&mut service, forwarded_params, build).await;

                    if let Ok(Some(locations)) = source_references {
                        for l in locations.iter() {
                            workspace_locations.insert(l.clone()); // TODO: undistinct links ?
                        }
                    }
                }

                Ok(Some(dbg!(workspace_locations).into_iter().collect()))
            } else {
                let err = "Definition of references request not found".to_string();
                Err(ResponseError::new(ErrorCode::REQUEST_FAILED, err))
            }
        })
    }
}

impl Proxy {
    #[allow(unused)]
    /// plain implementation of references request (without project links, only build context)
    fn local_references(&mut self, mut params: lsp::ReferenceParams) -> ResFut<R::References> {
        let mut service = self.server();
        let uri = &params.text_document_position.text_document.uri;
        let req_build = try_ensure_build!(self, uri, params, references);
        let state = self.state.clone();
        Box::pin(async move {
            let doc_pos = &mut params.text_document_position;
            try_forward_text_document_position_params!(state, req_build, doc_pos);
            let project = state.get_project();
            service
                .references(params)
                .await
                .map_err(|e| ResponseError::new(ErrorCode::INTERNAL_ERROR, e))
                .map(|r| r.unwrap())
                .map(|mut r| {
                    r.iter_mut().for_each(|l| {
                        if req_build.uri.canonicalize() == l.uri.canonicalize()
                            && let Ok(source) = forward_build_range(&mut l.range, &req_build)
                        {
                            l.uri = state.path_to_uri(&project.join(source.as_str())).unwrap();
                        };
                    });
                    Some(r)
                })
        })
    }
}

type DefRes = lsp::GotoDefinitionResponse;

fn forward_build_range(range: &mut lsp::Range, build: &Build) -> Result<Source, ResponseError> {
    let source_range = build.forward_build_range(range);
    if source_range.is_none() {
        let err = format!("Forward back build range `{:?}` failed", range);
        return Err(ResponseError::new(ErrorCode::REQUEST_FAILED, err));
    }
    let source_range = source_range.expect("is some");
    *range = source_range.0;
    Ok(source_range.1)
}

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

fn strip_module_hash(mut hover: lsp::Hover) -> lsp::Hover {
    type H = lsp::HoverContents;
    type S = lsp::MarkedString;

    let re = regex::Regex::new(&format!(r"{}\w+", regex::escape(IDENTIFIER_PREFIX))).unwrap();
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

fn forward_build_completion_item(item: &mut lsp::CompletionItem) {
    item.text_edit = None; // can't define context
    item.additional_text_edits = None;
    item.command = None;
}
