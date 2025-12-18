use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use crate::builder::Build;
use crate::parser::parse;
use crate::proxy::{Canonicalize, PROXY_WORKSPACE};
use crate::types::{
    BuildWithVersion, Document, DocumentDeclarationStatement, DocumentIdentifier,
    DocumentLinkStatement, Source, SourceHash,
};

use async_lsp::lsp_types::Url as Uri;
use async_lsp::{LanguageServer, ServerSocket, lsp_types as lsp};
use dashmap::DashMap;
use ropey::Rope;

#[derive(Default, Debug)]
pub struct State {
    project_path: Arc<OnceLock<PathBuf>>,
    documents: DashMap<PathBuf, Document>,
    builds: DashMap<PathBuf, BuildWithVersion>,

    uncommitted_build_changes: DashMap<PathBuf, Vec<lsp::DidChangeTextDocumentParams>>,

    uri_to_path: DashMap<Uri, PathBuf>,
    path_resolver_cache: DashMap<(PathBuf, String), Arc<PathBuf>>,
}

/// State of client buffers
impl State {
    pub fn set_doc(&self, source_uri: &Uri, changes: &[lsp::TextDocumentContentChangeEvent]) {
        let path_uri = self.uri_to_path(source_uri).unwrap();
        let (source, ident, source_hash, decl_stmt, link_stmt, path, buffer) = {
            if let Some(d) = self.documents.get(&path_uri) {
                (
                    Some(d.source.clone()),
                    Some(d.source_ident.clone()),
                    Some(d.source_hash.clone()),
                    Some(d.decl_stmt.clone()),
                    Some(d.link_stmt.clone()),
                    Some(d.path.clone()),
                    Some((*d.buffer).clone()),
                )
            } else {
                (None, None, None, None, None, None, None)
            }
        };

        let source = source.unwrap_or_else(|| {
            let source = &path_uri.strip_prefix(self.get_project()).unwrap().to_str();
            let source = source.ok_or(anyhow::anyhow!("existed source of project"));
            let source = source.unwrap().to_lowercase().replace('\\', "/");
            Source::new(source).into()
        });

        let ident = ident.unwrap_or(DocumentIdentifier::new(&source).into());
        let source_hash = source_hash.unwrap_or(SourceHash::new(&source).into());
        let path = path.unwrap_or(path_uri.clone().into());
        let decl_stmt =
            decl_stmt.unwrap_or(DocumentDeclarationStatement::new(&source, &ident).into());
        let link_stmt = link_stmt.unwrap_or(DocumentLinkStatement::from(&*ident).into());
        let mut buffer = buffer.unwrap_or(Rope::new());
        let insert_doc = |p: PathBuf, text: &str, buffer: Rope| {
            let tokens = parse(text);
            let doc = Document {
                dependency_hash: (&tokens).into(),
                tokens: tokens.into(),
                buffer: buffer.into(),
                decl_stmt,
                link_stmt,
                source,
                source_ident: ident,
                source_hash,
                path,
            };
            self.documents.insert(p, doc)
        };

        if changes.len() == 1 && changes[0].range.is_none() {
            let new_text = changes[0].text.replace("\r\n", "\n").replace("\r", "");
            buffer = Rope::from_str(&new_text).into();
            insert_doc(path_uri, &new_text, buffer);
            return;
        }

        for change in changes {
            let r = change.range.as_ref().expect("expected incremental sync");
            let text = change.text.replace("\r\n", "\n").replace("\r", "");
            let start = buffer.line_to_char(r.start.line as usize) + r.start.character as usize;
            let end = buffer.line_to_char(r.end.line as usize) + r.end.character as usize;

            buffer.remove(start..end);
            buffer.insert(start, &text);
        }

        let full_text = buffer.to_string();
        insert_doc(path_uri, &full_text, buffer);
    }

    pub fn get_doc(&self, source_uri: &Uri) -> anyhow::Result<Document> {
        let path = &self.uri_to_path(source_uri)?;
        let doc = self.documents.get(path);

        if doc.is_some() {
            return Ok(doc.unwrap().clone());
        }

        if !path.is_file() {
            return Err(anyhow::anyhow!("doc should be a file"));
        }

        let content = &[lsp::TextDocumentContentChangeEvent {
            text: fs::read_to_string(path).unwrap(),
            range_length: None,
            range: None,
        }];

        self.set_doc(source_uri, content);
        self.get_doc(source_uri)
    }
}

/// State of builds
impl State {
    pub fn set_build(&self, source_uri: &Uri) -> BuildWithVersion {
        let path = &self.uri_to_path(source_uri).unwrap();

        match self.builds.get_mut(path) {
            Some(mut b) => {
                let new_build = Build::new(&self, source_uri, Some(b.build.clone())).unwrap();
                b.build = new_build.into();
                b.version += 1;
            }
            None => {
                let new_build = Build::new(&self, source_uri, None).unwrap();
                let b = BuildWithVersion {
                    build: new_build.into(),
                    version: 1,
                };
                self.builds.insert(path.into(), b);
            }
        }

        self.builds.get(path).map(|guard| guard.clone()).unwrap()
    }

    pub fn get_build(&self, source_uri: &Uri) -> Option<Arc<Build>> {
        self.builds
            .get(&self.uri_to_path(source_uri).unwrap())
            .map(|guard| guard.build.clone())
    }

    pub fn remove_build(&self, source_uri: &Uri) {
        let path = &self.uri_to_path(source_uri).unwrap();
        self.builds.remove(path);
        self.uncommitted_build_changes.remove(path);
    }

    pub fn get_build_by_emit_uri(&self, emit_uri: &Uri) -> Option<Arc<Build>> {
        let emit_uri_canonicalized = emit_uri.canonicalize();
        self.builds
            .iter()
            .find(|e| e.build.emit_uri.canonicalize() == emit_uri_canonicalized)
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

    pub fn pending_build_changes(
        &self,
        source_uri: &Uri,
        changes: lsp::DidChangeTextDocumentParams,
    ) {
        let path = self.uri_to_path(source_uri).unwrap();
        self.uncommitted_build_changes
            .entry(path)
            .and_modify(|c| c.push(changes.to_owned()))
            .or_insert(vec![changes]);
    }

    pub fn commit_build_changes(&self, source_uri: &Uri, service: &mut ServerSocket) {
        let path = match self.uri_to_path(source_uri) {
            Ok(p) => p,
            Err(_) => return,
        };

        if let Some((_, changes)) = self.uncommitted_build_changes.remove(&path) {
            tracing::info!(
                "commit_build_changes {}",
                source_uri.as_str().split("/").last().unwrap()
            );
            for change in changes {
                let _ = service.did_change(change);
            }
        }
    }
}

/// State of config options
impl State {
    pub fn set_project(&self, source_uri: &Uri) {
        let sp = self.uri_to_path(source_uri).unwrap();
        self.project_path.set(sp).expect("project set once");
    }

    pub fn get_project(&self) -> &PathBuf {
        self.project_path.get().expect("project installed")
    }

    // FIXME: if global doc invalid or not installed ? change with constant global.js file
    pub fn get_global_doc(&self) -> Uri {
        let path = self.project_path.get().unwrap();
        let path = path.join(PROXY_WORKSPACE).join("global.js");
        Uri::from_file_path(path).unwrap()
    }
}

impl State {
    /// returns caonnicalized path
    #[inline]
    pub fn uri_to_path(&self, uri: &Uri) -> anyhow::Result<PathBuf> {
        if let Some(source_path) = self.uri_to_path.get(uri) {
            return Ok(source_path.clone());
        }

        let sp = uri.to_file_path();
        let sp = sp.map_err(|_| anyhow::anyhow!("uri to file path fail: {uri}"))?;
        let sp = dunce::canonicalize(sp)?;

        self.uri_to_path.insert(uri.clone(), sp.clone());
        Ok(sp)
    }

    #[inline]
    pub fn path_resolver(&self, path_from: &Path, include_literal: &str) -> Arc<PathBuf> {
        if let Some(resolved_path) = self
            .path_resolver_cache
            .get(&(path_from.into(), include_literal.to_string()))
        {
            return resolved_path.clone();
        }

        let project_root: &Path = &self.get_project();
        let path = include_literal.replace("\\\\", "/").replace("\\", "/");
        let resolved_path: Arc<PathBuf>;

        if Self::is_relative_path(&path) {
            let path_from_dir = path_from.parent().unwrap_or(project_root);
            resolved_path = Self::normalize_path(&path_from_dir.join(path)).into();
        } else {
            resolved_path = Self::normalize_path(&project_root.join(path)).into();
        }

        self.path_resolver_cache.insert(
            (path_from.into(), include_literal.to_string()),
            resolved_path.clone(),
        );

        resolved_path
    }

    #[inline]
    fn is_relative_path(path: &str) -> bool {
        path.starts_with("./")
            || path.starts_with(".\\")
            || path.starts_with("../")
            || path.starts_with("..\\")
    }

    #[inline]
    fn normalize_path(path: &Path) -> PathBuf {
        let mut buf = PathBuf::new();
        for component in path.components() {
            match component {
                std::path::Component::ParentDir => {
                    buf.pop().eq(&false).then(|| buf.push(".."));
                }
                std::path::Component::CurDir => {}
                _ => buf.push(component.as_os_str()),
            }
        }
        buf
    }
}
