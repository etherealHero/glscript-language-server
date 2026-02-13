mod builder;
mod parser;
mod proxy;
mod state;
mod types;

use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use tracing::level_filters::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::proxy::Proxy;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::registry()
        .with(LevelFilter::INFO)
        .with(tracing_subscriber::filter::filter_fn(|m| {
            m.name() != "service_ready"
        }))
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(std::io::stderr)
                .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
                .with_line_number(false)
                .with_target(true)
                .event_format(proxy::Formatter),
        )
        .init();

    let server = &std::env::args()
        .nth(1)
        .expect("expect argument to the forwarded LSP server");

    let mut child = async_process::Command::new(server)
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

    ref_server.set(client_socket).expect("set client socket");
    ref_client.set(server_socket).expect("set server socket");

    let child_stdin = child.stdin.take().expect("take tsls stdin");
    let child_stdout = child.stdout.take().expect("take tsls stdout");
    let main1 = tokio::spawn(mock_client.run_buffered(child_stdout, child_stdin));

    let stdin = tokio::io::stdin().compat();
    let stdout = tokio::io::stdout().compat_write();
    let main2 = tokio::spawn(mock_server.run_buffered(stdin, stdout));

    let res = tokio::select! {
        ret = main1 => ret,
        ret = main2 => ret,
    };

    if let Ok(Err(async_lsp::Error::Io(err))) = res {
        let _ = child.kill();
        tracing::error!("{err:#?}");
        std::process::exit(1);
    }
}
