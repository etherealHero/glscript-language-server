use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use async_lsp::lsp_types as lsp;
use async_lsp::lsp_types::Url as Uri;
use dashmap::DashMap;

use crate::types::BuildWithVersion;
use crate::types::Document;

mod build;
mod caches;
mod configuration;
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
    diagnostics_compatibility: Arc<OnceLock<bool>>,
    tsserver_initialized: Arc<OnceLock<bool>>,

    documents: DashMap<PathBuf, Document>,
    current_doc: Arc<Mutex<Option<Uri>>>,
    doc_to_bundle: DashMap<PathBuf, BuildWithVersion>,
    doc_to_transpile: DashMap<PathBuf, BuildWithVersion>,

    unforwarded_doc_changes: UnforwardedDocChanges,
    uncommitted_bundle_changes: UnforwardedBuildChanges,
    uncommitted_transpile_changes: UnforwardedBuildChanges,

    path_resolver_cache: DashMap<(PathBuf, String), Arc<PathBuf>>,
    uri_to_canonicalized_path: DashMap<Uri, PathBuf>,
    path_to_canonicalized_uri: DashMap<PathBuf, Uri>,
}
