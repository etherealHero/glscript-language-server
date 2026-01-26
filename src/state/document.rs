use std::mem::transmute;
use std::path::PathBuf;
use std::sync::Arc;

use async_lsp::lsp_types as lsp;
use async_lsp::lsp_types::Url as Uri;
use ropey::Rope;

use crate::parser::{Token, parse};
use crate::proxy::{Canonicalize, PROXY_WORKSPACE};
use crate::state::State;
use crate::types::{Document, DocumentDeclarationStatement, DocumentLinkStatement};
use crate::types::{DocumentIdentifier, Source, SourceHash};

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
            text: std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned())?,
            range_length: None,
            range: None,
        }];

        self.set_doc(source_uri, content)?;
        self.get_doc(source_uri)
    }

    pub fn get_current_doc(&self) -> Option<Uri> {
        self.current_document.lock().unwrap().clone()
    }

    pub fn set_current_doc(&self, source_uri: &Uri) {
        let mut guard = self.current_document.lock().unwrap();
        *guard = Some(source_uri.try_canonicalize());
    }
}
