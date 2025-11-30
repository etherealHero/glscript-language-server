mod forward;

mod builder;
mod proxy;
mod state;

mod language_client;
mod language_server;

use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::proxy::Proxy;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(std::io::stderr)
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

    let ref_server = Arc::new(OnceLock::new());
    let ref_client = Arc::new(OnceLock::new());

    let (server, client) = Proxy::init(ref_client.clone(), ref_server.clone());

    let (mock_client, server_socket) = async_lsp::MainLoop::new_client(|_| client);
    let (mock_server, client_socket) = async_lsp::MainLoop::new_server(|_| server);

    ref_server.set(client_socket).unwrap();
    ref_client.set(server_socket).unwrap();

    let child_stdin = child.stdin.take().unwrap();
    let child_stdout = child.stdout.take().unwrap();
    let main1 = tokio::spawn(mock_client.run_buffered(child_stdout, child_stdin));

    let stdin = tokio::io::stdin().compat();
    let stdout = tokio::io::stdout().compat_write();
    let main2 = tokio::spawn(mock_server.run_buffered(stdin, stdout));

    let ret = tokio::select! {
        ret = main1 => ret,
        ret = main2 => ret,
    };
    ret.expect("join error").unwrap();
}
