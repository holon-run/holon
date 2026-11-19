use super::serve_tcp;
use axum::{extract::ConnectInfo, routing::get, Router};
use std::{net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    time::timeout,
};

async fn get_response(mut stream: impl AsyncRead + AsyncWrite + Unpin) -> String {
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
}

#[tokio::test]
async fn tcp_serves_the_router_with_peer_info_and_shuts_down() {
    timeout(Duration::from_secs(10), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, shutdown) = oneshot::channel();
        let router = Router::new().route(
            "/health",
            get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move { peer.ip().to_string() }),
        );
        let server = tokio::spawn(serve_tcp(listener, router, async move {
            shutdown.await.unwrap();
        }));

        let response = get_response(TcpStream::connect(address).await.unwrap()).await;
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.ends_with("\r\n\r\n127.0.0.1"));
        stop.send(()).unwrap();
        server.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn unix_serves_the_router_and_exits_when_shutdown_changes() {
    use super::serve_unix;
    use tokio::{
        net::{UnixListener, UnixStream},
        sync::watch,
    };

    timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (stop, shutdown) = watch::channel(false);
        let router = Router::new().route("/health", get(|| async { "ok" }));
        let server = tokio::spawn(serve_unix(listener, router, shutdown));

        let response = get_response(UnixStream::connect(&path).await.unwrap()).await;
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.ends_with("\r\n\r\nok"));
        stop.send(true).unwrap();
        server.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}
