#![allow(non_snake_case)]

include!("../.thebe/server/routes.rs");

use axum::{Json, extract::State, routing::post};
use std::sync::{
  Arc,
  atomic::{AtomicI64, Ordering},
};

#[derive(Clone)]
struct AppState {
  counter: Arc<AtomicI64>,
}

impl AppState {
  fn new(initial_count: i64) -> Self {
    Self {
      counter: Arc::new(AtomicI64::new(initial_count)),
    }
  }

  fn count(&self) -> i64 {
    self.counter.load(Ordering::SeqCst)
  }

  fn increment(&self) -> i64 {
    self.counter.fetch_add(1, Ordering::SeqCst) + 1
  }

  fn decrement(&self) -> i64 {
    self.counter.fetch_sub(1, Ordering::SeqCst) - 1
  }

  fn reset(&self) -> i64 {
    self.counter.store(0, Ordering::SeqCst);
    0
  }
}

#[derive(serde::Serialize)]
struct CounterValue {
  count: i64,
}

#[tokio::main]
async fn main() {
  let state = AppState::new(0);
  let app = axum::Router::<AppState>::new()
    .merge(thebe_routes())
    .route("/api/counter/increment", post(increment))
    .route("/api/counter/decrement", post(decrement))
    .route("/api/counter/reset", post(reset))
    .fallback_service(tower_http::services::ServeDir::new("public"))
    .with_state(state);

  let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
    .await
    .expect("failed to bind to port 3000");

  println!("thebe dev listening at http://localhost:3000");
  axum::serve(listener, app).await.expect("server error");
}

async fn increment(State(state): State<AppState>) -> Json<CounterValue> {
  Json(CounterValue {
    count: state.increment(),
  })
}

async fn decrement(State(state): State<AppState>) -> Json<CounterValue> {
  Json(CounterValue {
    count: state.decrement(),
  })
}

async fn reset(State(state): State<AppState>) -> Json<CounterValue> {
  Json(CounterValue {
    count: state.reset(),
  })
}
