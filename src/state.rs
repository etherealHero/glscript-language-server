use std::mem::transmute;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock};

use async_lsp::lsp_types::Url as Uri;
use async_lsp::{LanguageServer, ServerSocket, lsp_types as lsp};
use dashmap::DashMap;
use ropey::Rope;

use crate::builder::Build;
use crate::parser::{Token, parse};
use crate::proxy::{Canonicalize, PROXY_WORKSPACE};

use crate::types::{BuildWithVersion, DocumentIdentifier, Source, SourceHash};
use crate::types::{Document, DocumentDeclarationStatement, DocumentLinkStatement};

mod progress;

type UnforwardedDocChanges = DashMap<PathBuf, Vec<(lsp::DidChangeTextDocumentParams, bool)>>; // Vec<(_, dependency_changed)>
type UnforwardedBuildChanges = DashMap<PathBuf, Vec<lsp::DidChangeTextDocumentParams>>;

#[derive(Default, Debug)]
pub struct State {
    pub cancel_received: Arc<crossbeam::atomic::AtomicCell<bool>>,

    work_done_progress_present: Arc<crossbeam::atomic::AtomicCell<bool>>,
    work_done_progress_token: Arc<OnceLock<lsp::NumberOrString>>,

    project_path: Arc<OnceLock<PathBuf>>,
    documents: DashMap<PathBuf, Document>,
    builds: DashMap<PathBuf, BuildWithVersion>,

    unforwarded_doc_changes: UnforwardedDocChanges,
    uncommitted_build_changes: UnforwardedBuildChanges,

    uri_to_path: DashMap<Uri, PathBuf>,
    path_to_uri: DashMap<PathBuf, Uri>,
    path_resolver_cache: DashMap<(PathBuf, String), Arc<PathBuf>>,
}

/// State of client buffers
impl State {
    // FIXME: some script duplicated. Validate uri_to_path fn
    pub fn set_doc(
        &self,
        source_uri: &Uri,
        changes: &[lsp::TextDocumentContentChangeEvent],
    ) -> anyhow::Result<()> {
        type RefMut<'a> = dashmap::mapref::one::RefMut<'a, PathBuf, Document>;
        let path = self.uri_to_path(source_uri)?;
        let mut doc = if let Some(old_doc) = self.documents.get_mut(&path) {
            old_doc
        } else {
            let source = Source::from_path(&path, self.get_project())?;
            let source_ident = DocumentIdentifier::new(&source);
            let build_uri = {
                // TODO: change to <project.join(PROXY_WORKSPACE)>/<source_path>/<source_hash.js>
                let proxy_ws = self.get_project().join(PROXY_WORKSPACE);
                let emit_path = proxy_ws.join(format!("{}.js", source_ident.as_str()));
                Uri::from_file_path(emit_path).map_err(|_| anyhow::anyhow!("build_uri failed"))?
            };

            let doc = Document {
                path: path.clone().into(),
                build_uri: build_uri.into(),

                buffer: Rope::new(),
                tokens: vec![].into(),
                content: String::new().into(),
                dependency_hash: Into::into(&vec![]),

                decl_stmt: DocumentDeclarationStatement::create(&source, &source_ident).into(),
                link_stmt: DocumentLinkStatement::create(&source, &source_ident).into(),

                source_hash: SourceHash::new(&source),
                source: source.into(),
            };

            self.documents.insert(path.clone(), doc);
            self.documents.get_mut(&path).unwrap()
        };

        let patch_doc_content = |doc: &mut RefMut<'_>, content: &str| {
            let content = Arc::new(content.to_string());
            let content_ref = content.clone();
            let tokens = parse(&content_ref);
            let tokens = unsafe { transmute::<Vec<Token<'_>>, Vec<Token<'static>>>(tokens) };

            doc.dependency_hash = Into::into(&tokens);
            doc.tokens = tokens.into();
            doc.content = content;
        };

        if changes.len() == 1 && changes[0].range.is_none() {
            let new_text = changes[0].text.as_str();
            doc.buffer = Rope::from_str(new_text);
            patch_doc_content(&mut doc, new_text);
            return Ok(());
        }

        for change in changes {
            let r = change.range.as_ref().unwrap();
            let text = change.text.as_str();
            let start = doc.buffer.line_to_char(r.start.line as usize) + r.start.character as usize;
            let end = doc.buffer.line_to_char(r.end.line as usize) + r.end.character as usize;

            doc.buffer.remove(start..end);
            doc.buffer.insert(start, text);
        }

        let full_text = &doc.buffer.to_string();
        patch_doc_content(&mut doc, full_text);
        Ok(())
    }

    pub fn get_doc(&self, source_uri: &Uri) -> anyhow::Result<Document> {
        let path = &self.uri_to_path(source_uri)?;
        let doc = self.documents.get(path);

        if let Some(d) = doc {
            return Ok(d.clone());
        }

        if !path.is_file() {
            return Err(anyhow::anyhow!("doc should be a file"));
        }

        let content = &[lsp::TextDocumentContentChangeEvent {
            text: std::fs::read_to_string(path)?,
            range_length: None,
            range: None,
        }];

        self.set_doc(source_uri, content)?;
        self.get_doc(source_uri)
    }
}

/// Lazy build changes
impl State {
    /// Flow:
    /// 1. client change buffer
    /// 2. changes save in state by [`State::add_client_doc_changes`]
    /// 3. state starts pull changes by [`State::commit_build_changes`] with stack unwinding
    ///    where client call some request
    pub fn add_client_doc_changes(
        &self,
        doc_path: PathBuf,
        client_doc_changes: lsp::DidChangeTextDocumentParams,
        is_doc_dependency_changed: bool,
    ) {
        self.unforwarded_doc_changes
            .entry(doc_path)
            .and_modify(|c| c.push((client_doc_changes.to_owned(), is_doc_dependency_changed)))
            .or_insert(vec![(client_doc_changes, is_doc_dependency_changed)]);
    }

    fn forward_client_doc_changes(&self) {
        use rayon::prelude::*;

        self.unforwarded_doc_changes.par_iter().for_each(|entry| {
            let (path, changes) = (entry.key(), entry.value());
            let doc_uri = &self.path_to_uri(path).unwrap();
            for (client_params, dependency_changed) in changes {
                let client_params_doc = self.get_doc(&client_params.text_document.uri).unwrap();
                let client_params_doc_source = client_params_doc.source;

                let build_of_doc = self.get_build(doc_uri).unwrap();
                let mut forward_changes = Vec::with_capacity(client_params.content_changes.len());

                for change in &client_params.content_changes {
                    if change.range.is_none() {
                        continue;
                    }

                    match build_of_doc
                        .forward_src_range(&change.range.unwrap(), &client_params_doc_source)
                    {
                        Some(r) => forward_changes.push(lsp::TextDocumentContentChangeEvent {
                            range: Some(r),
                            range_length: change.range_length,
                            text: change.text.clone(),
                        }),
                        None => return, // FIXME: sync docs failed
                    };
                }

                let new_build_of_doc_with_version = self.set_build(doc_uri).unwrap();
                let forward_params = lsp::DidChangeTextDocumentParams {
                    text_document: lsp::VersionedTextDocumentIdentifier {
                        uri: new_build_of_doc_with_version.build.uri.clone(),
                        version: new_build_of_doc_with_version.version,
                    },
                    content_changes: if *dependency_changed {
                        vec![lsp::TextDocumentContentChangeEvent {
                            text: new_build_of_doc_with_version.build.content.clone(),
                            range_length: None,
                            range: None,
                        }]
                    } else {
                        forward_changes
                    },
                };

                self.add_build_changes(doc_uri, forward_params);
            }
        });

        self.unforwarded_doc_changes.clear();
    }

    fn add_build_changes(
        &self,
        source_uri: &Uri,
        forward_changes: lsp::DidChangeTextDocumentParams,
    ) {
        let path = self.uri_to_path(source_uri).unwrap();
        self.uncommitted_build_changes
            .entry(path)
            .and_modify(|c| {
                let one_change = forward_changes.content_changes.len() == 1;
                let change = forward_changes.content_changes.first();
                let whole_buffer = change.as_ref().and_then(|c| c.range).is_none();
                if one_change && whole_buffer {
                    c.clear();
                }
                c.push(forward_changes.to_owned())
            })
            .or_insert(vec![forward_changes]);
    }

    pub fn commit_build_changes(&self, source_uri: &Uri, service: &mut ServerSocket) {
        let path = match self.uri_to_path(source_uri) {
            Ok(p) => p,
            Err(_) => return,
        };

        self.forward_client_doc_changes();

        if let Some((_, changes)) = self.uncommitted_build_changes.remove(&path) {
            for change in changes {
                let _ = service.did_change(change);
            }
        }
    }
}

/// State of builds
impl State {
    pub fn set_build(&self, source_uri: &Uri) -> anyhow::Result<BuildWithVersion> {
        let path = &self.uri_to_path(source_uri)?;

        match self.builds.get_mut(path) {
            Some(mut b) => {
                let new_build = Build::create(self, source_uri, Some(b.build.clone()))?;
                b.build = new_build.into();
                b.version += 1;
            }
            None => {
                let new_build = Build::create(self, source_uri, None)?;
                let build_with_version = BuildWithVersion::new(new_build.into(), 1);
                self.builds.insert(path.into(), build_with_version);
            }
        }

        Ok(self.builds.get(path).map(|guard| guard.clone()).unwrap())
    }

    pub fn get_build(&self, source_uri: &Uri) -> Option<Arc<Build>> {
        let path = match self.uri_to_path(source_uri) {
            Ok(p) => p,
            Err(_) => return None,
        };

        self.builds.get(&path).map(|guard| guard.build.clone())
    }

    pub fn remove_build(&self, source_uri: &Uri) {
        let path = &self.uri_to_path(source_uri).unwrap();
        self.builds.remove(path);
        self.uncommitted_build_changes.remove(path);
        self.unforwarded_doc_changes.remove(path);
    }

    pub fn get_build_by_emit_uri(&self, emit_uri: &Uri) -> Option<Arc<Build>> {
        let emit_uri_canonicalized = emit_uri.canonicalize().unwrap_or_else(|_| emit_uri.clone());
        self.builds
            .iter()
            .find(|e| e.build.uri.canonicalize().unwrap() == emit_uri_canonicalized)
            .map(|e| e.build.clone())
    }

    /// returns SourcePath for canonicalize interface
    pub fn get_builds_contains_source(&self, source: &Source) -> Vec<PathBuf> {
        self.builds
            .iter()
            .filter(|e| e.value().build.sources().contains(source))
            .map(|e| e.key().clone())
            .collect()
    }
}

/// State of configuration
impl State {
    pub fn initialize_project(&self, source_uri: &Uri) {
        let path = self.uri_to_path(source_uri).unwrap();
        let msg = "project initialize once";
        let ident = lsp::NumberOrString::String("glscript".into());
        self.project_path.set(path).expect(msg);
        self.work_done_progress_token.set(ident).expect(msg);
    }

    pub fn get_project(&self) -> &PathBuf {
        self.project_path.get().expect("project initialized")
    }

    pub fn get_default_doc(&self) -> Uri {
        let path = self.project_path.get().unwrap();
        let path = path.join(PROXY_WORKSPACE).join("DEFAULT_INCLUDED.js");
        let default_doc = self.path_to_uri(&path);
        default_doc.unwrap_or(Uri::from_file_path(path).unwrap().canonicalize().unwrap())
    }
}

impl State {
    /// returns canonicalized [`PathBuf`]
    #[inline]
    pub fn uri_to_path(&self, uri: &Uri) -> anyhow::Result<PathBuf> {
        if let Some(canonicalized_path) = self.uri_to_path.get(uri) {
            return Ok(canonicalized_path.clone());
        }

        let path = uri.to_file_path();
        let path = path.map_err(|_| anyhow::anyhow!("uri to file path fail: {uri}"))?;
        let canonicalized_path = dunce::canonicalize(dunce::simplified(&path))?;

        self.uri_to_path
            .insert(uri.clone(), canonicalized_path.clone());

        Ok(canonicalized_path)
    }

    /// returns canonicalized [`Uri`]
    #[inline]
    pub fn path_to_uri(&self, path: &Path) -> anyhow::Result<Uri> {
        if let Some(canonicalized_uri) = self.path_to_uri.get(path) {
            return Ok(canonicalized_uri.clone());
        }

        let canonicalized_path = &dunce::canonicalize(dunce::simplified(path))?;
        let uri = Uri::from_file_path(canonicalized_path);
        let uri = uri.map_err(|_| anyhow::anyhow!("path to uri fail: {path:?}"))?;
        let canonicalized_uri = uri.canonicalize()?;

        self.path_to_uri
            .insert(path.to_path_buf(), canonicalized_uri.clone());

        Ok(canonicalized_uri)
    }

    pub fn path_resolver(&self, path_from: &Path, path_literal: &str) -> Arc<PathBuf> {
        let key = (path_from.into(), path_literal.to_string());
        if let Some(resolved_path) = self.path_resolver_cache.get(&key) {
            return resolved_path.clone();
        }

        let is_relative = |path: &str| {
            path.starts_with("./")
                || path.starts_with(".\\")
                || path.starts_with("../")
                || path.starts_with("..\\")
        };

        #[allow(clippy::unit_arg)]
        let normilize = |path: &Path| {
            let mut buf = PathBuf::new();
            for component in path.components() {
                match component {
                    Component::ParentDir => buf.pop().eq(&false).then(|| buf.push("..")),
                    Component::CurDir => None,
                    _ => buf.push(component.as_os_str()).into(),
                };
            }
            buf
        };

        let path = path_literal.replace("\\\\", "/").replace("\\", "/");
        let resolved_path: Arc<PathBuf> = match is_relative(&path) {
            true => normilize(&path_from.parent().unwrap().join(path)).into(),
            false => normilize(&self.get_project().join(path)).into(),
        };

        self.path_resolver_cache.insert(key, resolved_path.clone());
        resolved_path
    }
}
