use std::fs;
use std::ops::ControlFlow;
use std::path::Path;
use std::process::Stdio;

use futures::channel::oneshot;
use tower::ServiceBuilder;
use tracing::Level;

use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::tracing::TracingLayer;

use async_lsp::lsp_types::{self as lsp, Url, notification as N, request as R};
use async_lsp::router::Router;
use async_lsp::{Error, ErrorCode, LanguageServer};

#[test]
fn service_test() {
    run_service_cases()
}

#[tokio::main(flavor = "current_thread")]
async fn run_service_cases() {
    let root_dir = Path::new(TEST_ROOT).canonicalize().unwrap();
    let (indexed_tx, _indexed_rx) = oneshot::channel();

    let (mainloop, mut server) = async_lsp::MainLoop::new_client(|_server| {
        let mut router = Router::new(ClientState {
            indexed_tx: Some(indexed_tx),
        });
        router
            .notification::<N::Progress>(|this, prog| {
                tracing::info!("{:?} {:?}", prog.token, prog.value);
                if matches!(
                    prog.value,
                    lsp::ProgressParamsValue::WorkDone(lsp::WorkDoneProgress::End(_))
                ) {
                    if let Some(tx) = this.indexed_tx.take() {
                        let _: Result<_, _> = tx.send(());
                    }
                }
                ControlFlow::Continue(())
            })
            .unhandled_notification(|_, params| {
                tracing::info!("Unhandled notif {:?}: {}", params.method, params.params);
                ControlFlow::Continue(())
            })
            .request::<R::WorkDoneProgressCreate, _>(|_, _| async move { Ok(()) })
            .event(|_, _: Stop| ControlFlow::Break(Ok(())));

        ServiceBuilder::new()
            .layer(TracingLayer::default())
            .layer(CatchUnwindLayer::default())
            .layer(ConcurrencyLayer::default())
            .service(router)
    });

    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .init();

    let child = async_process::Command::new(env!("CARGO_BIN_EXE_glscript-language-server"))
        .current_dir(&root_dir)
        // .arg(root_dir.join("./node_modules/.bin/typescript-language-server.cmd"))
        .arg(root_dir.join("./node_modules/.bin/tsgo.cmd"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // .stderr(Stdio::inherit())
        .stderr(Stdio::null()) // hide dbg! output
        .kill_on_drop(true)
        .spawn()
        .expect("Failed run glscript-language-server");
    let stdout = child.stdout.unwrap();
    let stdin = child.stdin.unwrap();

    let mainloop_fut =
        tokio::spawn(async move { mainloop.run_buffered(stdout, stdin).await.unwrap() });

    let _ = server
        .initialize(lsp::InitializeParams {
            workspace_folders: Some(vec![lsp::WorkspaceFolder {
                uri: Url::from_file_path(&root_dir).unwrap(),
                name: "root".into(),
            }]),
            capabilities: lsp::ClientCapabilities {
                window: Some(lsp::WindowClientCapabilities {
                    work_done_progress: Some(true),
                    ..lsp::WindowClientCapabilities::default()
                }),
                ..lsp::ClientCapabilities::default()
            },
            ..lsp::InitializeParams::default()
        })
        .await
        .unwrap();

    server.initialized(lsp::InitializedParams {}).unwrap();

    let main_file_uri = Url::from_file_path(root_dir.join("main.js")).unwrap();
    let main_text = &fs::read_to_string(root_dir.join("main.js")).unwrap();
    server
        .did_open(lsp::DidOpenTextDocumentParams {
            text_document: lsp::TextDocumentItem::new(
                main_file_uri.clone(),
                "javascript".into(),
                0,
                main_text.into(),
            ),
        })
        .unwrap();

    // _indexed_rx.await.unwrap();

    // region:    --- hover

    let hover: lsp::Hover = loop {
        let ret = server
            .hover(lsp::HoverParams {
                text_document_position_params: lsp::TextDocumentPositionParams::new(
                    lsp::TextDocumentIdentifier::new(main_file_uri.clone()),
                    lsp::Position::new(2, 36),
                ),
                work_done_progress_params: lsp::WorkDoneProgressParams::default(),
            })
            .await;

        match ret {
            Ok(resp) => break dbg!(resp).expect("no hover"),
            Err(Error::Response(resp)) if resp.code == ErrorCode::CONTENT_MODIFIED => continue,
            Err(err) => panic!("request failed: {err}"),
        }
    };

    assert!(
        matches!(hover.contents, lsp::HoverContents::Markup(lsp::MarkupContent { value, .. }) if value.contains("Sum two numbers")),
        "should show correct hover contents of dependency symbol",
    );

    // endregion: --- hover

    // region:    --- hover after change dependency

    let utils_file_uri = Url::from_file_path(root_dir.join("utils.js")).unwrap();
    let utils_text = &fs::read_to_string(root_dir.join("utils.js")).unwrap();
    server
        .did_open(lsp::DidOpenTextDocumentParams {
            text_document: lsp::TextDocumentItem::new(
                utils_file_uri.clone(),
                "javascript".into(),
                0,
                utils_text.into(),
            ),
        })
        .unwrap();

    server
        .did_change(lsp::DidChangeTextDocumentParams {
            text_document: lsp::VersionedTextDocumentIdentifier::new(utils_file_uri.clone(), 1),
            content_changes: vec![lsp::TextDocumentContentChangeEvent {
                range: Some(lsp::Range::new(
                    lsp::Position::new(1, 0), // TODO: extract change to tests
                    lsp::Position::new(1, 0),
                )),
                range_length: Some(1),
                text: String::from("\r\n"),
            }],
        })
        .unwrap();

    let hover: lsp::Hover = loop {
        let ret = server
            .hover(lsp::HoverParams {
                text_document_position_params: lsp::TextDocumentPositionParams::new(
                    lsp::TextDocumentIdentifier::new(main_file_uri.clone()),
                    lsp::Position::new(2, 36),
                ),
                work_done_progress_params: lsp::WorkDoneProgressParams::default(),
            })
            .await;

        match ret {
            Ok(resp) => break dbg!(resp).expect("no hover"),
            Err(Error::Response(resp)) if resp.code == ErrorCode::CONTENT_MODIFIED => continue,
            Err(err) => panic!("request failed: {err}"),
        }
    };

    assert!(
        matches!(hover.contents, lsp::HoverContents::Markup(lsp::MarkupContent { value, .. }) if value.contains("Sum two numbers")),
        "should show correct hover contents of dependency symbol (after change dependency)",
    );

    // endregion: --- hover after change dependency

    server.shutdown(()).await.unwrap();
    server.exit(()).unwrap();
    server.emit(Stop).unwrap();
    mainloop_fut.await.unwrap();
}

const TEST_ROOT: &str = "tests/client_test_data";
struct Stop;
struct ClientState {
    indexed_tx: Option<oneshot::Sender<()>>,
}
