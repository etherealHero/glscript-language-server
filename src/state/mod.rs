use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use async_lsp::lsp_types as lsp;
use async_lsp::lsp_types::Url as Uri;
use dashmap::DashMap;

use crate::proxy::{Canonicalize, DEFAULT_SCRIPT_FILENAME, PROXY_WORKSPACE};
use crate::types::BuildWithVersion;
use crate::types::Document;

mod build;
mod caches;
mod document;
mod lazy_build_changes;
mod progress;

type UnforwardedDocChanges = DashMap<PathBuf, Vec<(lsp::DidChangeTextDocumentParams, bool)>>; // Vec<(_, dependency_changed)>
pub type UnforwardedBuildChanges = DashMap<PathBuf, Vec<lsp::DidChangeTextDocumentParams>>;

#[derive(Default)]
pub struct State {
    pub cancel_received: Arc<crossbeam::atomic::AtomicCell<bool>>,

    work_done_progress_present: Arc<crossbeam::atomic::AtomicCell<bool>>,
    work_done_progress_token: Arc<OnceLock<lsp::NumberOrString>>,

    project: Arc<OnceLock<PathBuf>>,
    token_types_capabilities: Arc<OnceLock<Vec<lsp::SemanticTokenType>>>,
    documents: DashMap<PathBuf, Document>,
    current_doc: Arc<Mutex<Option<Uri>>>,
    doc_to_bundle: DashMap<PathBuf, BuildWithVersion>,
    doc_to_transpile: DashMap<PathBuf, BuildWithVersion>,
    diagnostics_compatibility: Arc<OnceLock<bool>>,

    unforwarded_doc_changes: UnforwardedDocChanges,
    uncommitted_bundle_changes: UnforwardedBuildChanges,
    uncommitted_transpile_changes: UnforwardedBuildChanges,

    path_resolver_cache: DashMap<(PathBuf, String), Arc<PathBuf>>,
    uri_to_canonicalized_path: DashMap<Uri, PathBuf>,
    path_to_canonicalized_uri: DashMap<PathBuf, Uri>,
}

/// State of configuration
impl State {
    pub fn initialize_project(
        &self,
        source_uri: &Uri,
        token_types: Option<Vec<lsp::SemanticTokenType>>,
    ) {
        let path = self.uri_to_path(source_uri).unwrap();
        let msg = "project initialize once";
        let ident = lsp::NumberOrString::String("glscript".into());

        if let Some(types) = token_types {
            self.token_types_capabilities.set(types).expect(msg);
        }

        self.project.set(path).expect(msg);
        self.work_done_progress_token.set(ident).expect(msg);

        // TODO: configure in client on release
        self.diagnostics_compatibility.set(false).expect(msg);
    }

    pub fn get_project(&self) -> &PathBuf {
        self.project.get().expect("project initialized")
    }

    pub fn get_default_doc(&self) -> Uri {
        let path = self.project.get().unwrap();
        let path = path.join(PROXY_WORKSPACE).join(DEFAULT_SCRIPT_FILENAME);
        let default_doc = self.path_to_uri(&path);

        default_doc.unwrap_or(Uri::from_file_path(path).unwrap().canonicalize().unwrap())
    }

    pub fn get_token_types_capabilities(&self) -> Option<&Vec<lsp::SemanticTokenType>> {
        self.token_types_capabilities.get()
    }

    pub fn is_diagnostics_enabled(&self) -> bool {
        *(self.diagnostics_compatibility.get().unwrap_or(&false))
    }
}
