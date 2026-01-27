use std::path::PathBuf;

use async_lsp::lsp_types::Url as Uri;
use async_lsp::{LanguageServer, ServerSocket, lsp_types as lsp};

use crate::state::State;

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
        transpile_changed: bool,
    ) {
        self.unforwarded_doc_changes
            .entry(doc_path)
            .and_modify(|c| c.push((client_doc_changes.to_owned(), transpile_changed)))
            .or_insert(vec![(client_doc_changes, transpile_changed)]);
    }

    fn forward_client_doc_changes(&self) {
        use rayon::prelude::*;

        self.unforwarded_doc_changes.par_iter().for_each(|entry| {
            let (path, changes) = (entry.key(), entry.value());
            let doc_uri = &self.path_to_uri(path).unwrap();
            for (client_params, transpile_changed) in changes {
                let client_params_doc = self.get_doc(&client_params.text_document.uri).unwrap();
                let client_params_doc_source = client_params_doc.source;

                let build_of_doc = self.get_build(doc_uri).unwrap();
                let mut forward_changes = Vec::with_capacity(client_params.content_changes.len());
                let transpile_changed = *transpile_changed;

                for change in &client_params.content_changes {
                    if transpile_changed {
                        break;
                    }

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
                    content_changes: if transpile_changed {
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
