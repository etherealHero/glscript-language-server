use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use glscript_language_server::proxy::Proxy;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .init();

    let server = &std::env::args()
        .nth(1)
        .expect("expect argument to the forwarded LSP server");
    let server_arg = server.contains("tsgo").then_some("--lsp").unwrap_or("");
    let mut child = async_process::Command::new(server)
        .arg(server_arg)
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("failed to spawn");

    // tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    let ref_server = Arc::new(OnceLock::new());
    let ref_client = Arc::new(OnceLock::new());

    let (server, client) = Proxy::init(ref_client.clone(), ref_server.clone());

    let (mock_client, server_socket) = async_lsp::MainLoop::new_client(|_| client);
    let (mock_server, client_socket) = async_lsp::MainLoop::new_server(|_| server);

    ref_server.set(client_socket).expect("set client socket");
    ref_client.set(server_socket).expect("set server socket");

    let child_stdin = child.stdin.take().expect("take tsls stdin");
    let child_stdout = child.stdout.take().expect("take tsls stdout");
    let main1 = tokio::spawn(mock_client.run_buffered(child_stdout, child_stdin));

    let stdin = tokio::io::stdin().compat();
    let stdout = tokio::io::stdout().compat_write();
    let main2 = tokio::spawn(mock_server.run_buffered(stdin, stdout));

    let ret = tokio::select! {
        ret = main1 => ret,
        ret = main2 => ret,
    };

    ret.map_err(|e| format!("setup proxy transport error: {e}"))
        .expect("setup proxy transport")
        .map_err(|e| format!("proxy lifecycle error: {e}"))
        .expect("proxy lifecycle");
}
