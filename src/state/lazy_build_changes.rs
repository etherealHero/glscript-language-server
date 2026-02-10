use std::path::PathBuf;

use async_lsp::lsp_types::Url as Uri;
use async_lsp::{LanguageServer, ServerSocket, lsp_types as lsp};

use crate::builder::Build;
use crate::state::{State, UnforwardedBuildChanges};
use crate::types::BuildWithVersion;

type ChangeEvent = lsp::TextDocumentContentChangeEvent;
type Ident = lsp::VersionedTextDocumentIdentifier;

/// Lazy build changes
impl State {
    /// Flow:
    /// 1. client change buffer
    /// 2. changes save in state by [`State::add_changes`]
    /// 3. state starts pull changes by [`State::commit_changes`] with stack unwinding
    ///    where client call some request
    pub fn add_changes(
        &self,
        doc_path: PathBuf,
        doc_changes: lsp::DidChangeTextDocumentParams,
        transpile_changed: bool,
    ) {
        self.unforwarded_doc_changes
            .entry(doc_path)
            .and_modify(|c| c.push((doc_changes.to_owned(), transpile_changed)))
            .or_insert(vec![(doc_changes, transpile_changed)]);
    }

    pub fn commit_changes(&self, source_uri: &Uri, s: &mut ServerSocket) {
        let Ok(path) = self.uri_to_path(source_uri) else {
            return;
        };

        let commit = |s: &mut ServerSocket, storage: &UnforwardedBuildChanges| {
            let Some(changes) = storage.remove(&path).map(|e| e.1) else {
                return;
            };

            for change in changes {
                let _ = s.did_change(change);
            }
        };

        self.forward();

        commit(s, &self.uncommitted_bundle_changes);
        commit(s, &self.uncommitted_transpile_changes);
    }
}

impl State {
    fn forward(&self) {
        use rayon::prelude::*;

        self.unforwarded_doc_changes.par_iter().for_each(|entry| {
            let (path, changes) = (entry.key(), entry.value());
            let doc_uri = &self.path_to_uri(path).unwrap();
            for (doc_changes, transpile_changed) in changes {
                let transpile_changed = *transpile_changed;

                if let Some(t) = self.get_transpile(doc_uri) {
                    let changes = self.forward_changes(&t, doc_changes, transpile_changed);
                    let t_new = self.set_transpile(doc_uri).unwrap();
                    let p = self.forward_params(changes, &t_new, transpile_changed);
                    self.add_forwarded_changes(doc_uri, p, &self.uncommitted_transpile_changes);
                }

                if let Some(b) = self.get_bundle(doc_uri) {
                    let changes = self.forward_changes(&b, doc_changes, transpile_changed);
                    let b_new = self.set_bundle(doc_uri).unwrap();
                    let p = self.forward_params(changes, &b_new, transpile_changed);
                    self.add_forwarded_changes(doc_uri, p, &self.uncommitted_bundle_changes);
                }
            }
        });

        self.unforwarded_doc_changes.clear();
    }

    fn forward_changes(
        &self,
        build: &Build,
        doc_changes: &lsp::DidChangeTextDocumentParams,
        transpile_changed: bool,
    ) -> Vec<lsp::TextDocumentContentChangeEvent> {
        let doc = self.get_doc(&doc_changes.text_document.uri).unwrap();
        let mut build_changes = Vec::with_capacity(doc_changes.content_changes.len());

        for change in &doc_changes.content_changes {
            if transpile_changed {
                break;
            }

            let Some(range) = &change.range else {
                continue;
            };

            match build.forward_src_range(range, &doc.source) {
                Some(r) => build_changes.push(ChangeEvent {
                    range: Some(r),
                    range_length: change.range_length,
                    text: change.text.clone(),
                }),
                None => {
                    let err = format!("Sync doc ({}) failed on forward client changes", doc.source);
                    tracing::error!(err); // FIXME: sync docs failed
                    continue;
                }
            };
        }

        build_changes
    }

    fn forward_params(
        &self,
        fc: Vec<ChangeEvent>,
        b: &BuildWithVersion,
        transpile_changed: bool,
    ) -> lsp::DidChangeTextDocumentParams {
        let full = || ChangeEvent {
            text: b.build.content.clone(),
            range_length: None,
            range: None,
        };
        lsp::DidChangeTextDocumentParams {
            text_document: Ident::new(b.build.uri.clone(), b.version),
            content_changes: if transpile_changed { vec![full()] } else { fc },
        }
    }

    fn add_forwarded_changes(
        &self,
        source_uri: &Uri,
        forward_changes: lsp::DidChangeTextDocumentParams,
        storage: &UnforwardedBuildChanges,
    ) {
        let path = self.uri_to_path(source_uri).unwrap();
        let modify = |c: &mut Vec<lsp::DidChangeTextDocumentParams>| {
            let one_change = forward_changes.content_changes.len() == 1;
            let change = forward_changes.content_changes.first();
            let whole_buffer = change.as_ref().and_then(|c| c.range).is_none();
            if one_change && whole_buffer {
                c.clear();
            }
            c.push(forward_changes.to_owned())
        };

        storage
            .entry(path)
            .and_modify(modify)
            .or_insert(vec![forward_changes]);
    }
}
