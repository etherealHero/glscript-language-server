use std::path::PathBuf;
use std::str::FromStr;
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
type UnforwardedBuildChanges = DashMap<PathBuf, Vec<lsp::DidChangeTextDocumentParams>>;

#[derive(Default)]
pub struct State {
    pub cancel_received: Arc<crossbeam::atomic::AtomicCell<bool>>,

    active_transpiled_buffer_ver: Arc<crossbeam::atomic::AtomicCell<i32>>,
    active_transpiled_buffer: Arc<OnceLock<Uri>>,

    work_done_progress_present: Arc<crossbeam::atomic::AtomicCell<bool>>,
    work_done_progress_token: Arc<OnceLock<lsp::NumberOrString>>,

    project_path: Arc<OnceLock<PathBuf>>,
    documents: DashMap<PathBuf, Document>,
    current_document: Arc<Mutex<Option<Uri>>>,
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
        let atb = Uri::from_str("file:///.virtual/active_transpiled_buffer.js").unwrap();

        self.active_transpiled_buffer_ver.store(1);
        self.active_transpiled_buffer.set(atb).expect(msg);
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

    pub fn get_active_transpiled_buffer(&self) -> Uri {
        self.active_transpiled_buffer.get().unwrap().clone()
    }

    pub fn set_active_transpiled_buffer(&self, text: &str) -> lsp::DidChangeTextDocumentParams {
        let current_ver = self.active_transpiled_buffer_ver.load();
        self.active_transpiled_buffer_ver.store(current_ver + 1);

        lsp::DidChangeTextDocumentParams {
            text_document: lsp::VersionedTextDocumentIdentifier {
                uri: self.get_active_transpiled_buffer(),
                version: current_ver + 1,
            },
            content_changes: vec![lsp::TextDocumentContentChangeEvent {
                text: text.into(),
                range_length: None,
                range: None,
            }],
        }
    }
}
