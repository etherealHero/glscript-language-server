use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

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
        let path = path.join(PROXY_WORKSPACE).join(DEFAULT_SCRIPT_FILENAME);
        let default_doc = self.path_to_uri(&path);
        default_doc.unwrap_or(Uri::from_file_path(path).unwrap().canonicalize().unwrap())
    }
}
