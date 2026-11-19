use std::{future::Future, io, net::SocketAddr};

use axum::Router;
use tokio::net::TcpListener;

#[cfg(unix)]
use {
    axum::body::Body,
    holon_system::fd_limit,
    hyper::{body::Incoming, service::service_fn, Request},
    hyper_util::{
        rt::{TokioExecutor, TokioIo},
        server::conn::auto::Builder as HyperBuilder,
    },
    std::convert::Infallible,
    tokio::{
        net::UnixListener,
        sync::watch,
        time::{sleep, Duration},
    },
    tower::ServiceExt,
    tracing::{error, warn},
};

pub async fn serve_tcp(
    listener: TcpListener,
    router: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}

#[cfg(unix)]
pub async fn serve_unix(
    listener: UnixListener,
    router: Router,
    mut shutdown: watch::Receiver<bool>,
) -> std::io::Result<()> {
    loop {
        let (stream, _) = tokio::select! {
            changed = shutdown.changed() => {
                match changed {
                    Ok(()) if *shutdown.borrow() => return Ok(()),
                    Ok(()) => continue,
                    Err(_) => return Ok(()),
                }
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok(accepted) => accepted,
                    Err(err) if fd_limit::is_fd_exhaustion_error(&err) => {
                        warn!(
                            target: "holon::http",
                            error = %err,
                            backoff_ms = 100,
                            "unix control server accept hit file descriptor limit; backing off"
                        );
                        sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                    Err(err) => return Err(err),
                }
            }
        };
        let router = router.clone();
        tokio::spawn(async move {
            let service = service_fn(move |request: Request<Incoming>| {
                let router = router.clone();
                async move {
                    let response = router
                        .oneshot(request.map(Body::new))
                        .await
                        .unwrap_or_else(|err| match err {});
                    Ok::<_, Infallible>(response)
                }
            });
            if let Err(err) = HyperBuilder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(TokioIo::new(stream), service)
                .await
            {
                error!(target: "holon::http", error = %err, "unix control server connection failed");
            }
        });
    }
}

#[cfg(test)]
mod tests;
