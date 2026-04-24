use std::fs;
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[test]
fn style_only_route_edit_should_emit_style_event_without_runtime_restart() {
  let _guard = hotpatch_test_guard();
  let fixture_port = reserve_port();
  let fixture = TestProject::new("style-event");
  fixture.write("Cargo.toml", &fixture_cargo_toml());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/index.trs", initial_route_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready(fixture_port);
  let browser_addr = wait_for_browser_addr(fixture.root());

  let client = reqwest::blocking::Client::builder()
    .timeout(None)
    .build()
    .expect("blocking client should build");
  let page = client
    .get(format!("http://127.0.0.1:{fixture_port}/"))
    .send()
    .expect("fixture page should respond")
    .text()
    .expect("fixture page body should read");
  assert!(page.contains("Hello hotpatch"));

  let (event_tx, event_rx) = mpsc::channel();
  let sse_handle = thread::spawn(move || {
    let response = client
      .get(format!("http://{browser_addr}/.thebe/dev/events"))
      .send()
      .expect("browser event stream should connect");
    let mut reader = BufReader::new(response);
    let mut line = String::new();

    loop {
      line.clear();
      let bytes_read = reader
        .read_line(&mut line)
        .expect("event stream line should read");
      if bytes_read == 0 {
        break;
      }

      let trimmed = line.trim().to_owned();
      if trimmed.starts_with("event:") || trimmed.starts_with("data:") {
        let _ = event_tx.send(trimmed);
      }
    }
  });

  thread::sleep(Duration::from_millis(150));
  fixture.write("src/routes/index.trs", updated_route_source());

  let event_name = wait_for_line(&event_rx, |line| line == "event: style", Duration::from_secs(20))
    .unwrap_or_else(|error| {
      panic!(
        "{error}\nprocess output:\n{}",
        collected_lines(&output).join("\n")
      )
    });
  assert_eq!(event_name, "event: style");

  let data_line = wait_for_line(&event_rx, |line| line.starts_with("data:"), Duration::from_secs(5))
    .unwrap_or_else(|error| {
      panic!(
        "{error}\nprocess output:\n{}",
        collected_lines(&output).join("\n")
      )
    });
  assert!(
    data_line.contains("#00f") || data_line.contains("blue"),
    "unexpected style payload: {data_line}"
  );
  assert!(data_line.contains(r#""routePattern":"/""#));

  thread::sleep(Duration::from_millis(800));

  let connected_lines = collected_lines(&output)
    .into_iter()
    .filter(|line| line.contains("thebe: hotpatch runtime connected"))
    .collect::<Vec<_>>();
  assert_eq!(connected_lines.len(), 1, "expected one runtime handshake, got {connected_lines:?}");
  assert!(child.try_wait().expect("child wait should succeed").is_none());

  child.terminate();
  let _ = sse_handle.join();
  for handle in output_threads {
    let _ = handle.join();
  }
}

#[test]
fn template_only_route_edit_should_emit_template_event_without_runtime_restart() {
  let _guard = hotpatch_test_guard();
  let fixture_port = reserve_port();
  let fixture = TestProject::new("template-event");
  fixture.write("Cargo.toml", &fixture_cargo_toml());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/index.trs", initial_template_route_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready(fixture_port);
  let browser_addr = wait_for_browser_addr(fixture.root());

  let client = reqwest::blocking::Client::builder()
    .timeout(None)
    .build()
    .expect("blocking client should build");
  let page = client
    .get(format!("http://127.0.0.1:{fixture_port}/"))
    .send()
    .expect("fixture page should respond")
    .text()
    .expect("fixture page body should read");
  assert!(page.contains("Template before"));

  let (event_tx, event_rx) = mpsc::channel();
  let sse_handle = thread::spawn(move || {
    let response = client
      .get(format!("http://{browser_addr}/.thebe/dev/events"))
      .send()
      .expect("browser event stream should connect");
    let mut reader = BufReader::new(response);
    let mut line = String::new();

    loop {
      line.clear();
      let bytes_read = reader
        .read_line(&mut line)
        .expect("event stream line should read");
      if bytes_read == 0 {
        break;
      }

      let trimmed = line.trim().to_owned();
      if trimmed.starts_with("event:") || trimmed.starts_with("data:") {
        let _ = event_tx.send(trimmed);
      }
    }
  });

  thread::sleep(Duration::from_millis(150));
  fixture.write("src/routes/index.trs", updated_template_route_source());

  let event_name = wait_for_line(&event_rx, |line| line == "event: template", Duration::from_secs(20))
    .unwrap_or_else(|error| {
      panic!(
        "{error}\nprocess output:\n{}",
        collected_lines(&output).join("\n")
      )
    });
  assert_eq!(event_name, "event: template");

  let data_line = wait_for_line(&event_rx, |line| line.starts_with("data:"), Duration::from_secs(5))
    .unwrap_or_else(|error| {
      panic!(
        "{error}\nprocess output:\n{}",
        collected_lines(&output).join("\n")
      )
    });
  assert!(data_line.contains(r#""routePattern":"/""#));
  assert!(!data_line.contains("css"), "unexpected template payload: {data_line}");

  thread::sleep(Duration::from_millis(800));

  let connected_lines = collected_lines(&output)
    .into_iter()
    .filter(|line| line.contains("thebe: hotpatch runtime connected"))
    .collect::<Vec<_>>();
  assert_eq!(connected_lines.len(), 1, "expected one runtime handshake, got {connected_lines:?}");
  assert!(child.try_wait().expect("child wait should succeed").is_none());

  child.terminate();
  let _ = sse_handle.join();
  for handle in output_threads {
    let _ = handle.join();
  }
}

#[test]
fn style_only_component_edit_should_emit_style_event_for_affected_route_without_runtime_restart() {
  let _guard = hotpatch_test_guard();
  let fixture_port = reserve_port();
  let fixture = TestProject::new("component-style-event");
  fixture.write("Cargo.toml", &fixture_cargo_toml());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/index.trs", route_using_card_source());
  fixture.write("src/routes/about.trs", plain_about_route_source());
  fixture.write("src/components/Card.trs", initial_card_component_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready_with_output(fixture_port, &output);
  let browser_addr = wait_for_browser_addr(fixture.root());

  assert!(fetch_page(fixture_port, "/").contains("Component before"));
  assert!(fetch_page(fixture_port, "/about").contains("About page"));

  let (event_rx, sse_handle) = open_event_stream(browser_addr);

  thread::sleep(Duration::from_millis(150));
  fixture.write("src/components/Card.trs", updated_card_component_style_source());

  let event_name = wait_for_line(&event_rx, |line| line == "event: style", Duration::from_secs(20))
    .unwrap_or_else(|error| {
      panic!(
        "{error}\nprocess output:\n{}",
        collected_lines(&output).join("\n")
      )
    });
  assert_eq!(event_name, "event: style");

  let data_line = wait_for_line(&event_rx, |line| line.starts_with("data:"), Duration::from_secs(5))
    .unwrap_or_else(|error| {
      panic!(
        "{error}\nprocess output:\n{}",
        collected_lines(&output).join("\n")
      )
    });
  assert!(data_line.contains(r#""routePattern":"/""#));
  assert!(data_line.contains("blue") || data_line.contains("#00f"));

  wait_for_page_matching(
    fixture_port,
    "/",
    |page| page.contains("blue") || page.contains("#00f"),
    "blue or #00f",
    Duration::from_secs(20),
  );
  let about_page = fetch_page(fixture_port, "/about");
  assert!(about_page.contains("About page"));
  assert!(!about_page.contains(".card"));
  assert!(!about_page.contains("blue"));
  wait_for_runtime_handshake_count(&output, 1, Duration::from_secs(5)).unwrap();
  assert!(child.try_wait().expect("child wait should succeed").is_none());

  child.terminate();
  let _ = sse_handle.join();
  for handle in output_threads {
    let _ = handle.join();
  }
}

#[test]
fn template_only_component_edit_should_emit_template_event_for_affected_route_without_runtime_restart() {
  let _guard = hotpatch_test_guard();
  let fixture_port = reserve_port();
  let fixture = TestProject::new("component-template-event");
  fixture.write("Cargo.toml", &fixture_cargo_toml());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/index.trs", route_using_card_source());
  fixture.write("src/routes/about.trs", plain_about_route_source());
  fixture.write("src/components/Card.trs", initial_card_component_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready_with_output(fixture_port, &output);
  let browser_addr = wait_for_browser_addr(fixture.root());

  assert!(fetch_page(fixture_port, "/").contains("Component before"));

  let (event_rx, sse_handle) = open_event_stream(browser_addr);

  thread::sleep(Duration::from_millis(150));
  fixture.write("src/components/Card.trs", updated_card_component_template_source());

  let event_name = wait_for_line(&event_rx, |line| line == "event: template", Duration::from_secs(20))
    .unwrap_or_else(|error| {
      panic!(
        "{error}\nprocess output:\n{}",
        collected_lines(&output).join("\n")
      )
    });
  assert_eq!(event_name, "event: template");

  let data_line = wait_for_line(&event_rx, |line| line.starts_with("data:"), Duration::from_secs(5))
    .unwrap_or_else(|error| {
      panic!(
        "{error}\nprocess output:\n{}",
        collected_lines(&output).join("\n")
      )
    });
  assert!(data_line.contains(r#""routePattern":"/""#));
  assert!(!data_line.contains("css"));

  wait_for_page_contains(fixture_port, "/", "Component after", Duration::from_secs(20));
  let about_page = fetch_page(fixture_port, "/about");
  assert!(about_page.contains("About page"));
  assert!(!about_page.contains("Component after"));
  wait_for_runtime_handshake_count(&output, 1, Duration::from_secs(5)).unwrap();
  assert!(child.try_wait().expect("child wait should succeed").is_none());

  child.terminate();
  let _ = sse_handle.join();
  for handle in output_threads {
    let _ = handle.join();
  }
}

#[test]
fn head_only_layout_edit_should_emit_template_event_for_affected_route_without_runtime_restart() {
  let _guard = hotpatch_test_guard();
  let fixture_port = reserve_port();
  let fixture = TestProject::new("layout-head-event");
  fixture.write("Cargo.toml", &fixture_cargo_toml());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/_layout.trs", initial_layout_source());
  fixture.write("src/routes/index.trs", layout_body_route_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready_with_output(fixture_port, &output);
  let browser_addr = wait_for_browser_addr(fixture.root());

  assert!(fetch_page(fixture_port, "/").contains("Layout before"));

  let (event_rx, sse_handle) = open_event_stream(browser_addr);

  thread::sleep(Duration::from_millis(150));
  fixture.write("src/routes/_layout.trs", updated_layout_head_source());

  let event_name = wait_for_line(&event_rx, |line| line == "event: template", Duration::from_secs(20))
    .unwrap_or_else(|error| {
      panic!(
        "{error}\nprocess output:\n{}",
        collected_lines(&output).join("\n")
      )
    });
  assert_eq!(event_name, "event: template");

  let data_line = wait_for_line(&event_rx, |line| line.starts_with("data:"), Duration::from_secs(5))
    .unwrap_or_else(|error| {
      panic!(
        "{error}\nprocess output:\n{}",
        collected_lines(&output).join("\n")
      )
    });
  assert!(data_line.contains(r#""routePattern":"/""#));

  wait_for_page_contains(fixture_port, "/", "Layout after", Duration::from_secs(20));
  wait_for_runtime_handshake_count(&output, 1, Duration::from_secs(5)).unwrap();
  assert!(child.try_wait().expect("child wait should succeed").is_none());

  child.terminate();
  let _ = sse_handle.join();
  for handle in output_threads {
    let _ = handle.join();
  }
}

#[test]
fn route_script_edit_should_restart_runtime_with_generated_input_reason() {
  let _guard = hotpatch_test_guard();
  let fixture_port = reserve_port();
  let fixture = TestProject::new("route-script-restart");
  fixture.write("Cargo.toml", &fixture_cargo_toml());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/index.trs", &script_restart_route_source("Before restart"));

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready_with_output(fixture_port, &output);
  assert!(fetch_page(fixture_port, "/").contains("Before restart"));

  fixture.write("src/routes/index.trs", &script_restart_route_source("After restart"));

  wait_for_output_line(
    &output,
    |line| line.contains("restart required — Thebe-generated input changed"),
    Duration::from_secs(30),
  )
  .unwrap();
  wait_for_runtime_handshake_count(&output, 2, Duration::from_secs(30)).unwrap();
  wait_for_page_contains(fixture_port, "/", "After restart", Duration::from_secs(30));
  assert!(child.try_wait().expect("child wait should succeed").is_none());

  child.terminate();
  for handle in output_threads {
    let _ = handle.join();
  }
}

#[test]
fn entry_point_edit_should_restart_runtime_with_entry_point_reason() {
  let _guard = hotpatch_test_guard();
  let fixture_port = reserve_port();
  let fixture = TestProject::new("entry-point-restart");
  fixture.write("Cargo.toml", &fixture_cargo_toml());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/index.trs", initial_template_route_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready_with_output(fixture_port, &output);
  fixture.write("src/main.rs", &format!("{}\n// entrypoint touch\n", fixture_main_rs(fixture_port)));

  wait_for_output_line(
    &output,
    |line| line.contains("restart required — application entry point changed"),
    Duration::from_secs(30),
  )
  .unwrap();
  wait_for_runtime_handshake_count(&output, 2, Duration::from_secs(30)).unwrap();
  wait_for_page_contains(fixture_port, "/", "Template before", Duration::from_secs(30));
  assert!(child.try_wait().expect("child wait should succeed").is_none());

  child.terminate();
  for handle in output_threads {
    let _ = handle.join();
  }
}

struct ManagedChild {
  child: Option<Child>,
}

impl ManagedChild {
  fn new(child: Child) -> Self {
    Self { child: Some(child) }
  }

  fn child_mut(&mut self) -> &mut Child {
    self.child.as_mut().expect("managed child should be present")
  }

  fn terminate(&mut self) {
    if let Some(mut child) = self.child.take() {
      terminate_process_tree(&mut child);
    }
  }

  fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
    self.child_mut().try_wait()
  }
}

impl Drop for ManagedChild {
  fn drop(&mut self) {
    self.terminate();
  }
}

struct TestProject {
  root: PathBuf,
}

impl TestProject {
  fn new(name: &str) -> Self {
    let suffix = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .expect("clock should be after unix epoch")
      .as_nanos();
    let root = std::env::temp_dir().join(format!(
      "thebe-cli-hotpatch-{name}-{suffix}"
    ));
    fs::create_dir_all(root.join("src/routes")).expect("fixture routes dir should create");
    Self { root }
  }

  fn root(&self) -> &Path {
    &self.root
  }

  fn write(&self, relative_path: &str, contents: &str) {
    let path = self.root.join(relative_path);
    if let Some(parent) = path.parent() {
      fs::create_dir_all(parent).expect("fixture parent dir should create");
    }
    fs::write(path, contents).expect("fixture file should write");
  }
}

impl Drop for TestProject {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.root);
  }
}

fn fixture_cargo_toml() -> String {
  format!(
    r#"[package]
name = "hotpatch-style-fixture"
version = "0.1.0"
edition = "2024"

[dependencies]
axum = "0.8"
tokio = {{ version = "1", features = ["full"] }}
minijinja = "2"
serde = {{ version = "1", features = ["derive"] }}
serde_json = "1"
thebe-runtime = {{ path = "{}" }}
"#,
    Path::new(env!("CARGO_MANIFEST_DIR"))
      .join("../thebe-runtime")
      .canonicalize()
      .expect("runtime crate path should resolve")
      .display(),
  )
}

fn fixture_main_rs(port: u16) -> String {
  format!(
    r#"#![allow(non_snake_case)]

include!("../.thebe/server/routes.rs");
include!("../.thebe/hotpatch.rs");

#[tokio::main]
async fn main() {{
  connect_thebe_hotpatch()
    .expect("hotpatch runtime handshake should succeed when enabled");

  let app = axum::Router::new().merge(thebe_routes());
  let listener = tokio::net::TcpListener::bind("127.0.0.1:{port}")
    .await
    .expect("fixture should bind its port");

  println!("fixture listening at http://127.0.0.1:{port}");
  axum::serve(listener, app).await.expect("fixture server error");
}}
"#,
  )
}

fn initial_route_source() -> &'static str {
  r#"<script setup>
struct Props {
  title: String,
}

#[thebe::get]
pub fn index() -> Props {
  Props {
    title: "Hello hotpatch".to_owned(),
  }
}
</script>

<style>
h1 {
  color: red;
}
</style>

<h1>{{ title }}</h1>
"#
}

fn updated_route_source() -> &'static str {
  r#"<script setup>
struct Props {
  title: String,
}

#[thebe::get]
pub fn index() -> Props {
  Props {
    title: "Hello hotpatch".to_owned(),
  }
}
</script>

<style>
h1 {
  color: blue;
}
</style>

<h1>{{ title }}</h1>
"#
}

fn initial_template_route_source() -> &'static str {
  r#"<script setup>
struct Props {
  title: String,
}

#[thebe::get]
pub fn index() -> Props {
  Props {
    title: "Hello hotpatch".to_owned(),
  }
}
</script>

<h1>{{ title }}</h1>
<p>Template before</p>
"#
}

fn updated_template_route_source() -> &'static str {
  r#"<script setup>
struct Props {
  title: String,
}

#[thebe::get]
pub fn index() -> Props {
  Props {
    title: "Hello hotpatch".to_owned(),
  }
}
</script>

<h1>{{ title }}</h1>
<p>Template after</p>
"#
}

fn hotpatch_test_guard() -> std::sync::MutexGuard<'static, ()> {
  static HOTPATCH_TEST_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();

  HOTPATCH_TEST_MUTEX
    .get_or_init(|| Mutex::new(()))
    .lock()
    .expect("hotpatch test mutex should lock")
}

fn route_using_card_source() -> &'static str {
  r#"<script setup>
struct Props {}

#[thebe::get]
pub fn index() -> Props {
  Props {}
}
</script>

<main>
  <Card />
</main>
"#
}

fn plain_about_route_source() -> &'static str {
  r#"<script setup>
struct Props {}

#[thebe::get]
pub fn about() -> Props {
  Props {}
}
</script>

<main>
  <p>About page</p>
</main>
"#
}

fn initial_card_component_source() -> &'static str {
  r#"<style>
.card {
  color: red;
}
</style>

<div class="card">Component before<slot /></div>
"#
}

fn updated_card_component_style_source() -> &'static str {
  r#"<style>
.card {
  color: blue;
}
</style>

<div class="card">Component before<slot /></div>
"#
}

fn updated_card_component_template_source() -> &'static str {
  r#"<style>
.card {
  color: red;
}
</style>

<div class="card">Component after<slot /></div>
"#
}

fn initial_layout_source() -> &'static str {
  r#"<head>
  <meta name="layout-probe" content="Layout before" />
</head>

<div>
  <slot />
</div>
"#
}

fn updated_layout_head_source() -> &'static str {
  r#"<head>
  <meta name="layout-probe" content="Layout after" />
</head>

<div>
  <slot />
</div>
"#
}

fn layout_body_route_source() -> &'static str {
  r#"<script setup>
struct Props {}

#[thebe::get]
pub fn index() -> Props {
  Props {}
}
</script>

<main>
  <p>Layout route body</p>
</main>
"#
}

fn script_restart_route_source(message: &str) -> String {
  format!(
    r#"<script setup>
struct Props {{
  title: String,
}}

#[thebe::get]
pub fn index() -> Props {{
  Props {{
    title: "{message}".to_owned(),
  }}
}}
</script>

<h1>{{{{ title }}}}</h1>
"#,
  )
}

fn spawn_hotpatch_process(project_root: &Path) -> ManagedChild {
  ManagedChild::new(
    Command::new(env!("CARGO_BIN_EXE_thebe"))
    .current_dir(project_root)
    .args(["dev", "--hotpatch"])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("thebe dev --hotpatch should spawn"),
  )
}

fn spawn_output_collectors(
  child: &mut ManagedChild,
  output: Arc<Mutex<Vec<String>>>,
) -> Vec<thread::JoinHandle<()>> {
  let mut handles = Vec::new();

  if let Some(stdout) = child.child_mut().stdout.take() {
    handles.push(spawn_output_collector(stdout, Arc::clone(&output)));
  }

  if let Some(stderr) = child.child_mut().stderr.take() {
    handles.push(spawn_output_collector(stderr, output));
  }

  handles
}

fn spawn_output_collector<R>(reader: R, output: Arc<Mutex<Vec<String>>>) -> thread::JoinHandle<()>
where
  R: std::io::Read + Send + 'static,
{
  thread::spawn(move || {
    let reader = BufReader::new(reader);
    for line in reader.lines() {
      let Ok(line) = line else {
        break;
      };
      output.lock().expect("output lock should hold").push(line);
    }
  })
}

fn collected_lines(output: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
  output.lock().expect("output lock should hold").clone()
}

fn open_event_stream(browser_addr: SocketAddr) -> (mpsc::Receiver<String>, thread::JoinHandle<()>) {
  let client = reqwest::blocking::Client::builder()
    .timeout(None)
    .build()
    .expect("blocking client should build");
  let (event_tx, event_rx) = mpsc::channel();
  let sse_handle = thread::spawn(move || {
    let response = client
      .get(format!("http://{browser_addr}/.thebe/dev/events"))
      .send()
      .expect("browser event stream should connect");
    let mut reader = BufReader::new(response);
    let mut line = String::new();

    loop {
      line.clear();
      let bytes_read = match reader.read_line(&mut line) {
        Ok(bytes_read) => bytes_read,
        Err(_error) => break,
      };
      if bytes_read == 0 {
        break;
      }

      let trimmed = line.trim().to_owned();
      if trimmed.starts_with("event:") || trimmed.starts_with("data:") {
        let _ = event_tx.send(trimmed);
      }
    }
  });

  (event_rx, sse_handle)
}

fn try_fetch_page(port: u16, path: &str) -> Option<String> {
  let client = reqwest::blocking::Client::builder()
    .timeout(Duration::from_millis(250))
    .build()
    .expect("polling client should build");
  let response = client
    .get(format!("http://127.0.0.1:{port}{path}"))
    .send()
    .ok()?;

  response.text().ok()
}

fn fetch_page(port: u16, path: &str) -> String {
  try_fetch_page(port, path).expect("fixture page should respond")
}

fn wait_for_page_contains(port: u16, path: &str, needle: &str, timeout: Duration) {
  wait_for_page_matching(port, path, |page| page.contains(needle), needle, timeout);
}

fn wait_for_page_matching<F>(
  port: u16,
  path: &str,
  predicate: F,
  description: &str,
  timeout: Duration,
)
where
  F: Fn(&str) -> bool,
{
  let started = Instant::now();

  while started.elapsed() < timeout {
    if try_fetch_page(port, path).is_some_and(|page| predicate(&page)) {
      return;
    }
    thread::sleep(Duration::from_millis(100));
  }

  panic!("page {path} did not contain {description:?} within {timeout:?}");
}

fn wait_for_runtime_handshake_count(
  output: &Arc<Mutex<Vec<String>>>,
  expected_count: usize,
  timeout: Duration,
) -> Result<(), String> {
  let started = Instant::now();

  while started.elapsed() < timeout {
    let connected_lines = collected_lines(output)
      .into_iter()
      .filter(|line| line.contains("thebe: hotpatch runtime connected"))
      .count();
    if connected_lines == expected_count {
      return Ok(());
    }
    thread::sleep(Duration::from_millis(100));
  }

  Err(format!(
    "timed out waiting for {expected_count} runtime handshake(s); seen output: {:?}",
    collected_lines(output)
  ))
}

fn wait_for_output_line<F>(
  output: &Arc<Mutex<Vec<String>>>,
  predicate: F,
  timeout: Duration,
) -> Result<String, String>
where
  F: Fn(&str) -> bool,
{
  let started = Instant::now();

  while started.elapsed() < timeout {
    if let Some(line) = collected_lines(output)
      .into_iter()
      .find(|line| predicate(line))
    {
      return Ok(line);
    }
    thread::sleep(Duration::from_millis(100));
  }

  Err(format!(
    "timed out waiting for matching process output; seen: {:?}",
    collected_lines(output)
  ))
}

fn wait_for_app_ready(port: u16) {
  let started = Instant::now();
  let client = reqwest::blocking::Client::builder()
    .timeout(Duration::from_millis(250))
    .build()
    .expect("polling client should build");

  while started.elapsed() < Duration::from_secs(30) {
    if let Ok(response) = client.get(format!("http://127.0.0.1:{port}/")).send()
      && response.status().is_success() {
        return;
      }
    thread::sleep(Duration::from_millis(100));
  }

  panic!("fixture app did not become ready on port {port}");
}

fn wait_for_app_ready_with_output(port: u16, output: &Arc<Mutex<Vec<String>>>) {
  let started = Instant::now();
  let client = reqwest::blocking::Client::builder()
    .timeout(Duration::from_millis(250))
    .build()
    .expect("polling client should build");
  let mut last_response = None;

  while started.elapsed() < Duration::from_secs(30) {
    if let Ok(response) = client.get(format!("http://127.0.0.1:{port}/")).send() {
      let status = response.status();
      let body = response.text().unwrap_or_default();
      if status.is_success() {
        return;
      }
      last_response = Some((status, body));
    }
    thread::sleep(Duration::from_millis(100));
  }

  let last_response = last_response
    .map(|(status, body)| format!("\nlast response: {status}\n{body}"))
    .unwrap_or_default();
  panic!(
    "fixture app did not become ready on port {port}{last_response}\nprocess output:\n{}",
    collected_lines(output).join("\n")
  );
}

fn wait_for_browser_addr(project_root: &Path) -> SocketAddr {
  let session_path = project_root.join(".thebe/hotpatch/session.json");
  let started = Instant::now();

  while started.elapsed() < Duration::from_secs(30) {
    if let Ok(source) = fs::read_to_string(&session_path)
      && let Ok(json) = serde_json::from_str::<serde_json::Value>(&source)
      && let Some(browser_addr) = json.get("browser_addr").and_then(serde_json::Value::as_str)
      && let Ok(addr) = browser_addr.parse() {
        return addr;
      }

    thread::sleep(Duration::from_millis(50));
  }

  panic!("browser patch address was not written to {}", session_path.display());
}

fn reserve_port() -> u16 {
  std::net::TcpListener::bind("127.0.0.1:0")
    .expect("ephemeral port should bind")
    .local_addr()
    .expect("listener should expose a local addr")
    .port()
}

fn wait_for_line<F>(
  receiver: &mpsc::Receiver<String>,
  predicate: F,
  timeout: Duration,
) -> Result<String, String>
where
  F: Fn(&str) -> bool,
{
  let started = Instant::now();
  let mut seen = Vec::new();

  while started.elapsed() < timeout {
    match receiver.recv_timeout(Duration::from_millis(250)) {
      Ok(line) if predicate(&line) => return Ok(line),
      Ok(line) => seen.push(line),
      Err(mpsc::RecvTimeoutError::Timeout) => {}
      Err(mpsc::RecvTimeoutError::Disconnected) => break,
    }
  }

  Err(format!(
    "timed out waiting for expected event stream line; seen: {:?}",
    seen
  ))
}

fn terminate_process_tree(child: &mut Child) {
  #[cfg(unix)]
  {
    let _ = Command::new("pkill")
      .args(["-TERM", "-P", &child.id().to_string()])
      .status();
    thread::sleep(Duration::from_millis(200));
  }

  let _ = child.kill();
  let _ = child.wait();
}
