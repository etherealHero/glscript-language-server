use std::mem::transmute;
use std::path::PathBuf;
use std::sync::Arc;

use async_lsp::lsp_types as lsp;
use async_lsp::lsp_types::Url as Uri;
use ropey::Rope;

use crate::parser::{Parse, parse};
use crate::proxy::{Canonicalize, PROXY_WORKSPACE};
use crate::state::State;
use crate::types::{Document, DocumentDeclarationStatement, DocumentLinkStatement};
use crate::types::{DocumentIdentifier, Source, SourceHash};

/// State of client buffers
impl State {
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

            let (bundle_uri, transpiled_doc_uri) = {
                let proxy_ws = self.get_project().join(PROXY_WORKSPACE);
                let uri_fail = |_| anyhow::anyhow!("create uri failed");
                let try_uri = |n: String| Uri::from_file_path(proxy_ws.join(n)).map_err(uri_fail);
                let ident = source_ident.as_str();
                let bundle_uri = try_uri(format!("bundle.{ident}.js"))?;
                let transpiled_doc_uri = try_uri(format!("transpile.{ident}.js"))?;

                (bundle_uri, transpiled_doc_uri)
            };

            let doc = Document {
                path: path.clone().into(),
                bundle_uri: bundle_uri.into(),
                transpile_uri: transpiled_doc_uri.into(),

                buffer: Rope::new(),
                parse: Parse::default().into(),
                parse_content: String::new().into(),
                transpile_hash: (&vec![], None).into(),

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
            let parse = unsafe { transmute::<Parse<'_>, Parse<'static>>(parse(&content_ref)) };

            doc.parse = parse.into();
            doc.parse_content = content;
        };

        if changes.len() == 1 && changes[0].range.is_none() {
            let new_text = changes[0].text.as_str();
            doc.buffer = Rope::from_str(new_text);
            patch_doc_content(&mut doc, new_text);
            doc.transpile_hash = (doc.parse.compressed_tokens.as_ref(), None).into();
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
        doc.transpile_hash = (doc.parse.compressed_tokens.as_ref(), changes.into()).into();
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

    pub fn get_doc_by_emit_uri(&self, emit_uri: &Uri) -> Option<Document> {
        let emit_uri_canonicalized = emit_uri.try_canonicalize();
        self.doc_to_bundle
            .iter()
            .find(|e| e.build.uri.canonicalize().unwrap() == emit_uri_canonicalized)
            .map(|e| self.get_doc(&self.path_to_uri(e.key()).unwrap()).unwrap())
    }

    pub fn get_current_doc(&self) -> Option<Uri> {
        self.current_doc.lock().unwrap().clone()
    }

    pub fn set_current_doc(&self, source_uri: &Uri) {
        let mut guard = self.current_doc.lock().unwrap();
        *guard = Some(source_uri.try_canonicalize());
    }
}
