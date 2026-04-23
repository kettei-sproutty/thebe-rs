use axum::Router;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::get;
use serde::Serialize;
use std::convert::Infallible;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tokio::sync::{broadcast, oneshot};
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError};

const BROWSER_EVENTS_PATH: &str = "/.thebe/dev/events";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowserPatchEvent {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) css: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) route_pattern: Option<String>,
}

#[derive(Debug, Clone)]
struct BrowserEventMessage {
  data: String,
  event: &'static str,
}

pub(crate) struct BrowserPatchServer {
  event_tx: broadcast::Sender<BrowserEventMessage>,
  local_addr: SocketAddr,
  shutdown_tx: Option<oneshot::Sender<()>>,
  worker: Option<JoinHandle<()>>,
}

impl BrowserPatchServer {
  pub(crate) fn spawn() -> io::Result<Self> {
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    listener.set_nonblocking(true)?;
    let local_addr = listener.local_addr()?;
    let (event_tx, _) = broadcast::channel(64);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let event_tx_for_worker = event_tx.clone();

    let worker = thread::Builder::new()
      .name(String::from("thebe-hotpatch-browser"))
      .spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
          .enable_all()
          .build()
        {
          Ok(runtime) => runtime,
          Err(error) => {
            eprintln!("thebe: failed to start browser patch runtime: {error}");
            return;
          }
        };

        if let Err(error) = runtime.block_on(run_browser_server(listener, event_tx_for_worker, shutdown_rx)) {
          eprintln!("thebe: browser patch server stopped: {error}");
        }
      })
      .map_err(io::Error::other)?;

    Ok(Self {
      event_tx,
      local_addr,
      shutdown_tx: Some(shutdown_tx),
      worker: Some(worker),
    })
  }

  #[must_use]
  pub(crate) fn local_addr(&self) -> SocketAddr {
    self.local_addr
  }

  pub(crate) fn broadcast_style(&self, route_pattern: Option<&str>, css: String) {
    self.send(
      "style",
      &BrowserPatchEvent {
        css: Some(css),
        route_pattern: route_pattern.map(String::from),
      },
    );
  }

  pub(crate) fn broadcast_template(&self, route_pattern: Option<&str>) {
    self.send(
      "template",
      &BrowserPatchEvent {
        css: None,
        route_pattern: route_pattern.map(String::from),
      },
    );
  }

  pub(crate) fn broadcast_reload(&self) {
    self.send("reload", &BrowserPatchEvent { css: None, route_pattern: None });
  }

  fn send<T>(&self, event: &'static str, payload: &T)
  where
    T: Serialize,
  {
    let Ok(data) = serde_json::to_string(payload) else {
      return;
    };

    let _ = self.event_tx.send(BrowserEventMessage { data, event });
  }
}

impl Drop for BrowserPatchServer {
  fn drop(&mut self) {
    if let Some(shutdown_tx) = self.shutdown_tx.take() {
      let _ = shutdown_tx.send(());
    }

    if let Some(worker) = self.worker.take() {
      let _ = worker.join();
    }
  }
}

async fn run_browser_server(
  listener: TcpListener,
  event_tx: broadcast::Sender<BrowserEventMessage>,
  shutdown_rx: oneshot::Receiver<()>,
) -> io::Result<()> {
  let listener = tokio::net::TcpListener::from_std(listener)?;
  let app = Router::new()
    .route(BROWSER_EVENTS_PATH, get(browser_events))
    .with_state(event_tx);

  axum::serve(listener, app)
    .with_graceful_shutdown(async move {
      let _ = shutdown_rx.await;
    })
    .await
    .map_err(io::Error::other)
}

async fn browser_events(
  State(event_tx): State<broadcast::Sender<BrowserEventMessage>>,
) -> impl IntoResponse {
  let stream = BroadcastStream::new(event_tx.subscribe()).filter_map(|message| {
    match message {
      Ok(message) => Some(Ok::<Event, Infallible>(
        Event::default().event(message.event).data(message.data),
      )),
      Err(BroadcastStreamRecvError::Lagged(_)) => Some(Ok::<Event, Infallible>(
        Event::default().comment("thebe-browser-lagged"),
      )),
    }
  });

  (
    [
      (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
      (header::CACHE_CONTROL, "no-cache"),
    ],
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))),
  )
}
