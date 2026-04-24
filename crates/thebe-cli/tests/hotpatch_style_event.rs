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
fn route_client_script_edit_should_emit_template_event_without_runtime_restart() {
  let _guard = hotpatch_test_guard();
  let fixture_port = reserve_port();
  let fixture = TestProject::new("route-client-script-event");
  fixture.write("Cargo.toml", &fixture_cargo_toml_with_ts_rs());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/index.trs", initial_client_script_route_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready_with_output(fixture_port, &output);
  let browser_addr = wait_for_browser_addr(fixture.root());

  let page = fetch_page(fixture_port, "/");
  assert!(page.contains("id=\"counter\""));

  let (event_rx, sse_handle) = open_event_stream(browser_addr);

  thread::sleep(Duration::from_millis(150));
  fixture.write("src/routes/index.trs", updated_client_script_route_source());

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

  wait_for_runtime_handshake_count(&output, 1, Duration::from_secs(5)).unwrap();
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
fn component_client_script_edit_should_emit_template_event_without_runtime_restart() {
  let _guard = hotpatch_test_guard();
  let fixture_port = reserve_port();
  let fixture = TestProject::new("component-client-script-event");
  fixture.write("Cargo.toml", &fixture_cargo_toml());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/index.trs", route_using_counter_button_source());
  fixture.write("src/components/CounterButton.trs", counter_button_component_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready_with_output(fixture_port, &output);
  let browser_addr = wait_for_browser_addr(fixture.root());

  let page = fetch_page(fixture_port, "/");
  assert!(page.contains("id=\"component-counter\""));

  let (event_rx, sse_handle) = open_event_stream(browser_addr);

  thread::sleep(Duration::from_millis(150));
  fixture.write(
    "src/components/CounterButton.trs",
    updated_counter_button_component_source(),
  );

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

  wait_for_runtime_handshake_count(&output, 1, Duration::from_secs(5)).unwrap();
  assert!(child.try_wait().expect("child wait should succeed").is_none());

  child.terminate();
  let _ = sse_handle.join();
  for handle in output_threads {
    let _ = handle.join();
  }
}

#[test]
fn layout_client_script_edit_should_emit_template_event_without_runtime_restart() {
  let _guard = hotpatch_test_guard();
  let fixture_port = reserve_port();
  let fixture = TestProject::new("layout-client-script-event");
  fixture.write("Cargo.toml", &fixture_cargo_toml());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/_layout.trs", initial_layout_client_script_source());
  fixture.write("src/routes/index.trs", layout_client_script_route_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready_with_output(fixture_port, &output);
  let browser_addr = wait_for_browser_addr(fixture.root());

  let page = fetch_page(fixture_port, "/");
  assert!(page.contains("id=\"layout-counter\""));

  let (event_rx, sse_handle) = open_event_stream(browser_addr);

  thread::sleep(Duration::from_millis(150));
  fixture.write("src/routes/_layout.trs", updated_layout_client_script_source());

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

  wait_for_runtime_handshake_count(&output, 1, Duration::from_secs(5)).unwrap();
  assert!(child.try_wait().expect("child wait should succeed").is_none());

  child.terminate();
  let _ = sse_handle.join();
  for handle in output_threads {
    let _ = handle.join();
  }
}

#[test]
fn style_only_route_edit_should_update_live_browser_styles_without_page_reload() {
  let _guard = hotpatch_test_guard();
  if !playwright_probe_supported() {
    eprintln!("skipping Playwright hotpatch browser test: local Playwright runtime is unavailable");
    return;
  }

  let fixture_port = reserve_port();
  let fixture = TestProject::new("style-browser-patch");
  fixture.write("Cargo.toml", &fixture_cargo_toml());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/index.trs", initial_route_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready_with_output(fixture_port, &output);
  assert!(fetch_page(fixture_port, "/").contains("Hello hotpatch"));

  let probe = spawn_playwright_probe(
    &style_hotpatch_probe_script(fixture_port),
    &fixture.root().join("src/routes/index.trs"),
    updated_route_source(),
  );
  let probe_output = probe
    .wait_with_output()
    .expect("Playwright style probe should complete");
  assert_playwright_probe_success(&probe_output, &output, "style hotpatch browser probe");

  let probe_stdout = String::from_utf8_lossy(&probe_output.stdout);
  assert!(probe_stdout.contains(r#""beforeUnloadCount":"0""#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""managedStyleCount":1"#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains("style-hotpatch-probe"), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains("blue") || probe_stdout.contains("0, 0, 255"), "unexpected probe output: {probe_stdout}");

  wait_for_runtime_handshake_count(&output, 1, Duration::from_secs(5)).unwrap();
  assert!(child.try_wait().expect("child wait should succeed").is_none());

  child.terminate();
  for handle in output_threads {
    let _ = handle.join();
  }
}

#[test]
fn head_only_layout_edit_should_sync_live_browser_head_without_page_reload() {
  let _guard = hotpatch_test_guard();
  if !playwright_probe_supported() {
    eprintln!("skipping Playwright hotpatch browser test: local Playwright runtime is unavailable");
    return;
  }

  let fixture_port = reserve_port();
  let fixture = TestProject::new("layout-head-browser-patch");
  fixture.write("Cargo.toml", &fixture_cargo_toml());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/_layout.trs", initial_layout_source());
  fixture.write("src/routes/index.trs", layout_body_route_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready_with_output(fixture_port, &output);
  assert!(fetch_page(fixture_port, "/").contains("Layout before"));

  let probe = spawn_playwright_probe(
    &layout_head_hotpatch_probe_script(fixture_port),
    &fixture.root().join("src/routes/_layout.trs"),
    updated_layout_head_source(),
  );
  let probe_output = probe
    .wait_with_output()
    .expect("Playwright layout head probe should complete");
  assert_playwright_probe_success(&probe_output, &output, "layout head hotpatch browser probe");

  let probe_stdout = String::from_utf8_lossy(&probe_output.stdout);
  assert!(probe_stdout.contains(r#""beforeUnloadCount":"0""#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""metaContent":"Layout after""#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains("layout-head-hotpatch-probe"), "unexpected probe output: {probe_stdout}");

  wait_for_runtime_handshake_count(&output, 1, Duration::from_secs(5)).unwrap();
  assert!(child.try_wait().expect("child wait should succeed").is_none());

  child.terminate();
  for handle in output_threads {
    let _ = handle.join();
  }
}

#[test]
fn template_only_route_edit_should_refresh_live_browser_without_page_reload() {
  let _guard = hotpatch_test_guard();
  if !playwright_probe_supported() {
    eprintln!("skipping Playwright hotpatch browser test: local Playwright runtime is unavailable");
    return;
  }

  let fixture_port = reserve_port();
  let fixture = TestProject::new("template-browser-patch");
  fixture.write("Cargo.toml", &fixture_cargo_toml());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/index.trs", initial_template_route_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready_with_output(fixture_port, &output);
  assert!(fetch_page(fixture_port, "/").contains("Template before"));

  let probe = spawn_playwright_probe(
    &route_template_hotpatch_probe_script(fixture_port),
    &fixture.root().join("src/routes/index.trs"),
    updated_template_route_source(),
  );
  let probe_output = probe
    .wait_with_output()
    .expect("Playwright route template probe should complete");
  assert_playwright_probe_success(&probe_output, &output, "route template hotpatch browser probe");

  let probe_stdout = String::from_utf8_lossy(&probe_output.stdout);
  assert!(probe_stdout.contains(r#""beforeUnloadCount":"0""#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""refreshCount":1"#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""templateSeen":true"#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains("template-route-hotpatch-probe"), "unexpected probe output: {probe_stdout}");

  wait_for_runtime_handshake_count(&output, 1, Duration::from_secs(5)).unwrap();
  assert!(child.try_wait().expect("child wait should succeed").is_none());

  child.terminate();
  for handle in output_threads {
    let _ = handle.join();
  }
}

#[test]
fn route_component_prop_edit_should_refresh_component_output_without_page_reload() {
  let _guard = hotpatch_test_guard();
  if !playwright_probe_supported() {
    eprintln!("skipping Playwright hotpatch browser test: local Playwright runtime is unavailable");
    return;
  }

  let fixture_port = reserve_port();
  let fixture = TestProject::new("route-component-prop-browser-patch");
  fixture.write("Cargo.toml", &fixture_cargo_toml());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/index.trs", route_using_stat_card_before_source());
  fixture.write("src/components/StatCard.trs", stat_card_component_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready_with_output(fixture_port, &output);
  assert!(fetch_page(fixture_port, "/").contains("Template before"));

  let probe = spawn_playwright_probe(
    &route_template_hotpatch_probe_script(fixture_port),
    &fixture.root().join("src/routes/index.trs"),
    route_using_stat_card_after_source(),
  );
  let probe_output = probe
    .wait_with_output()
    .expect("Playwright route component prop probe should complete");
  assert_playwright_probe_success(&probe_output, &output, "route component prop hotpatch browser probe");

  let probe_stdout = String::from_utf8_lossy(&probe_output.stdout);
  assert!(probe_stdout.contains(r#""beforeUnloadCount":"0""#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""refreshCount":1"#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""templateSeen":true"#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains("template-route-hotpatch-probe"), "unexpected probe output: {probe_stdout}");

  wait_for_runtime_handshake_count(&output, 1, Duration::from_secs(5)).unwrap();
  assert!(child.try_wait().expect("child wait should succeed").is_none());

  child.terminate();
  for handle in output_threads {
    let _ = handle.join();
  }
}

#[test]
fn route_client_script_edit_should_refresh_live_browser_and_apply_new_handler_without_page_reload() {
  let _guard = hotpatch_test_guard();
  if !playwright_probe_supported() {
    eprintln!("skipping Playwright hotpatch browser test: local Playwright runtime is unavailable");
    return;
  }

  let fixture_port = reserve_port();
  let fixture = TestProject::new("route-client-script-browser-patch");
  fixture.write("Cargo.toml", &fixture_cargo_toml_with_ts_rs());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/index.trs", initial_client_script_route_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready_with_output(fixture_port, &output);
  assert!(fetch_page(fixture_port, "/").contains("id=\"counter\""));

  let probe = spawn_playwright_probe(
    &route_client_script_hotpatch_probe_script(fixture_port),
    &fixture.root().join("src/routes/index.trs"),
    updated_client_script_route_source(),
  );
  let probe_output = probe
    .wait_with_output()
    .expect("Playwright route client script probe should complete");
  assert_playwright_probe_success(&probe_output, &output, "route client script hotpatch browser probe");

  let probe_stdout = String::from_utf8_lossy(&probe_output.stdout);
  assert!(probe_stdout.contains(r#""beforeUnloadCount":"0""#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""refreshCount":1"#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""initialHandlerCount":"1""#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""postPatchCount":"2""#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains("route-client-script-hotpatch-probe"), "unexpected probe output: {probe_stdout}");

  wait_for_runtime_handshake_count(&output, 1, Duration::from_secs(5)).unwrap();
  assert!(child.try_wait().expect("child wait should succeed").is_none());

  child.terminate();
  for handle in output_threads {
    let _ = handle.join();
  }
}

#[test]
fn component_client_script_edit_should_refresh_live_browser_and_apply_new_handler_without_page_reload() {
  let _guard = hotpatch_test_guard();
  if !playwright_probe_supported() {
    eprintln!("skipping Playwright hotpatch browser test: local Playwright runtime is unavailable");
    return;
  }

  let fixture_port = reserve_port();
  let fixture = TestProject::new("component-client-script-browser-patch");
  fixture.write("Cargo.toml", &fixture_cargo_toml());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/index.trs", route_using_counter_button_source());
  fixture.write("src/components/CounterButton.trs", counter_button_component_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready_with_output(fixture_port, &output);
  assert!(fetch_page(fixture_port, "/").contains("id=\"component-counter\""));

  let probe = spawn_playwright_probe(
    &component_client_script_hotpatch_probe_script(fixture_port),
    &fixture.root().join("src/components/CounterButton.trs"),
    updated_counter_button_component_source(),
  );
  let probe_output = probe
    .wait_with_output()
    .expect("Playwright component client script hotpatch probe should complete");
  assert_playwright_probe_success(&probe_output, &output, "component client script hotpatch browser probe");

  let probe_stdout = String::from_utf8_lossy(&probe_output.stdout);
  assert!(probe_stdout.contains(r#""beforeUnloadCount":"0""#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""refreshCount":1"#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""initialHandlerCount":"1""#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""postPatchCount":"2""#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains("component-client-script-hotpatch-probe"), "unexpected probe output: {probe_stdout}");

  wait_for_runtime_handshake_count(&output, 1, Duration::from_secs(5)).unwrap();
  assert!(child.try_wait().expect("child wait should succeed").is_none());

  child.terminate();
  for handle in output_threads {
    let _ = handle.join();
  }
}

#[test]
fn layout_client_script_edit_should_refresh_live_browser_and_apply_new_handler_without_page_reload() {
  let _guard = hotpatch_test_guard();
  if !playwright_probe_supported() {
    eprintln!("skipping Playwright hotpatch browser test: local Playwright runtime is unavailable");
    return;
  }

  let fixture_port = reserve_port();
  let fixture = TestProject::new("layout-client-script-browser-patch");
  fixture.write("Cargo.toml", &fixture_cargo_toml());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/_layout.trs", initial_layout_client_script_source());
  fixture.write("src/routes/index.trs", layout_client_script_route_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready_with_output(fixture_port, &output);
  assert!(fetch_page(fixture_port, "/").contains("id=\"layout-counter\""));

  let probe = spawn_playwright_probe(
    &layout_client_script_hotpatch_probe_script(fixture_port),
    &fixture.root().join("src/routes/_layout.trs"),
    updated_layout_client_script_source(),
  );
  let probe_output = probe
    .wait_with_output()
    .expect("Playwright layout client script hotpatch probe should complete");
  assert_playwright_probe_success(&probe_output, &output, "layout client script hotpatch browser probe");

  let probe_stdout = String::from_utf8_lossy(&probe_output.stdout);
  assert!(probe_stdout.contains(r#""beforeUnloadCount":"0""#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""refreshCount":1"#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""initialHandlerCount":"1""#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""postPatchCount":"2""#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains("layout-client-script-hotpatch-probe"), "unexpected probe output: {probe_stdout}");

  wait_for_runtime_handshake_count(&output, 1, Duration::from_secs(5)).unwrap();
  assert!(child.try_wait().expect("child wait should succeed").is_none());

  child.terminate();
  for handle in output_threads {
    let _ = handle.join();
  }
}

#[cfg(unix)]
#[test]
fn hotpatch_process_should_exit_cleanly_on_sigint_with_live_browser_page_connected() {
  let _guard = hotpatch_test_guard();
  if !playwright_probe_supported() {
    eprintln!("skipping Playwright hotpatch shutdown test: local Playwright runtime is unavailable");
    return;
  }

  let fixture_port = reserve_port();
  let fixture = TestProject::new("shutdown-browser-session");
  fixture.write("Cargo.toml", &fixture_cargo_toml());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/index.trs", initial_template_route_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready_with_output(fixture_port, &output);
  assert!(fetch_page(fixture_port, "/").contains("Template before"));

  let mut probe = spawn_playwright_probe(
    &shutdown_hotpatch_probe_script(fixture_port),
    &fixture.root().join("src/routes/index.trs"),
    initial_template_route_source(),
  );
  let probe_output = Arc::new(Mutex::new(Vec::<String>::new()));
  let probe_threads = spawn_child_output_collectors(&mut probe, Arc::clone(&probe_output));

  wait_for_output_line(
    &probe_output,
    |line| line.starts_with("event-source-opened "),
    Duration::from_secs(30),
  )
  .unwrap_or_else(|error| {
    panic!(
      "{error}\nprobe output:\n{}\nhotpatch output:\n{}",
      collected_lines(&probe_output).join("\n"),
      collected_lines(&output).join("\n")
    )
  });

  let exit_status = child
    .interrupt_and_wait(Duration::from_secs(10))
    .unwrap_or_else(|error| {
      panic!(
        "{error}\nprobe output:\n{}\nhotpatch output:\n{}",
        collected_lines(&probe_output).join("\n"),
        collected_lines(&output).join("\n")
      )
    });
  assert!(
    exit_status.success(),
    "expected clean shutdown after SIGINT, got {exit_status}\nprobe output:\n{}\nhotpatch output:\n{}",
    collected_lines(&probe_output).join("\n"),
    collected_lines(&output).join("\n")
  );
  wait_for_app_to_stop(fixture_port, Duration::from_secs(5));

  terminate_process_tree(&mut probe);
  for handle in probe_threads {
    let _ = handle.join();
  }
  for handle in output_threads {
    let _ = handle.join();
  }
}

#[test]
fn component_client_script_should_work_in_live_browser() {
  let _guard = hotpatch_test_guard();
  if !playwright_probe_supported() {
    eprintln!("skipping Playwright component browser test: local Playwright runtime is unavailable");
    return;
  }

  let fixture_port = reserve_port();
  let fixture = TestProject::new("component-client-script-browser");
  fixture.write("Cargo.toml", &fixture_cargo_toml());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/index.trs", route_using_counter_button_source());
  fixture.write("src/components/CounterButton.trs", counter_button_component_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready_with_output(fixture_port, &output);
  assert!(fetch_page(fixture_port, "/").contains("id=\"component-counter\""));

  let probe = spawn_playwright_probe(
    &component_client_script_probe_script(fixture_port),
    &fixture.root().join("src/routes/index.trs"),
    route_using_counter_button_source(),
  );
  let probe_output = probe
    .wait_with_output()
    .expect("Playwright component client script probe should complete");
  assert_playwright_probe_success(&probe_output, &output, "component client script browser probe");

  let probe_stdout = String::from_utf8_lossy(&probe_output.stdout);
  assert!(probe_stdout.contains(r#""beforeUnloadCount":"0""#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""count":"1""#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains("component-client-script-probe"), "unexpected probe output: {probe_stdout}");

  wait_for_runtime_handshake_count(&output, 1, Duration::from_secs(5)).unwrap();
  assert!(child.try_wait().expect("child wait should succeed").is_none());

  child.terminate();
  for handle in output_threads {
    let _ = handle.join();
  }
}

#[test]
fn style_only_component_edit_should_update_affected_live_route_and_ignore_unaffected_browser_route() {
  let _guard = hotpatch_test_guard();
  if !playwright_probe_supported() {
    eprintln!("skipping Playwright hotpatch browser test: local Playwright runtime is unavailable");
    return;
  }

  let fixture_port = reserve_port();
  let fixture = TestProject::new("component-style-browser-patch");
  fixture.write("Cargo.toml", &fixture_cargo_toml());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/index.trs", route_using_card_source());
  fixture.write("src/routes/about.trs", plain_about_route_source());
  fixture.write("src/components/Card.trs", initial_card_component_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready_with_output(fixture_port, &output);
  assert!(fetch_page(fixture_port, "/").contains("Component before"));
  assert!(fetch_page(fixture_port, "/about").contains("About page"));

  let probe = spawn_playwright_probe(
    &component_style_hotpatch_probe_script(fixture_port),
    &fixture.root().join("src/components/Card.trs"),
    updated_card_component_style_source(),
  );
  let probe_output = probe
    .wait_with_output()
    .expect("Playwright component style probe should complete");
  assert_playwright_probe_success(&probe_output, &output, "component style hotpatch browser probe");

  let probe_stdout = String::from_utf8_lossy(&probe_output.stdout);
  assert!(probe_stdout.contains(r#""affectedBeforeUnloadCount":"0""#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""affectedApplyCount":1"#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""unaffectedBeforeUnloadCount":"0""#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""unaffectedApplyCount":0"#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""unaffectedStillAbout":true"#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains("component-style-affected-probe"), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains("component-style-unaffected-probe"), "unexpected probe output: {probe_stdout}");

  wait_for_runtime_handshake_count(&output, 1, Duration::from_secs(5)).unwrap();
  assert!(child.try_wait().expect("child wait should succeed").is_none());

  child.terminate();
  for handle in output_threads {
    let _ = handle.join();
  }
}

#[test]
fn template_only_component_edit_should_refresh_affected_live_route_and_ignore_unaffected_browser_route() {
  let _guard = hotpatch_test_guard();
  if !playwright_probe_supported() {
    eprintln!("skipping Playwright hotpatch browser test: local Playwright runtime is unavailable");
    return;
  }

  let fixture_port = reserve_port();
  let fixture = TestProject::new("component-template-browser-patch");
  fixture.write("Cargo.toml", &fixture_cargo_toml());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/index.trs", route_using_card_source());
  fixture.write("src/routes/about.trs", plain_about_route_source());
  fixture.write("src/components/Card.trs", initial_card_component_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready_with_output(fixture_port, &output);
  assert!(fetch_page(fixture_port, "/").contains("Component before"));
  assert!(fetch_page(fixture_port, "/about").contains("About page"));

  let probe = spawn_playwright_probe(
    &component_template_hotpatch_probe_script(fixture_port),
    &fixture.root().join("src/components/Card.trs"),
    updated_card_component_template_source(),
  );
  let probe_output = probe
    .wait_with_output()
    .expect("Playwright component template probe should complete");
  assert_playwright_probe_success(&probe_output, &output, "component template hotpatch browser probe");

  let probe_stdout = String::from_utf8_lossy(&probe_output.stdout);
  assert!(probe_stdout.contains(r#""affectedBeforeUnloadCount":"0""#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""affectedRefreshCount":1"#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""affectedTemplateSeen":true"#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""unaffectedBeforeUnloadCount":"0""#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""unaffectedRefreshCount":0"#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""unaffectedStillAbout":true"#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains("component-template-affected-probe"), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains("component-template-unaffected-probe"), "unexpected probe output: {probe_stdout}");

  wait_for_runtime_handshake_count(&output, 1, Duration::from_secs(5)).unwrap();
  assert!(child.try_wait().expect("child wait should succeed").is_none());

  child.terminate();
  for handle in output_threads {
    let _ = handle.join();
  }
}

#[test]
fn unused_layout_edit_should_leave_live_browser_untouched() {
  let _guard = hotpatch_test_guard();
  if !playwright_probe_supported() {
    eprintln!("skipping Playwright hotpatch browser test: local Playwright runtime is unavailable");
    return;
  }

  let fixture_port = reserve_port();
  let fixture = TestProject::new("unused-layout-browser-noop");
  fixture.write("Cargo.toml", &fixture_cargo_toml());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/index.trs", initial_template_route_source());
  fixture.write("src/routes/admin/_layout.trs", initial_unused_layout_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready_with_output(fixture_port, &output);
  assert!(fetch_page(fixture_port, "/").contains("Template before"));

  let probe = spawn_playwright_probe(
    &noop_hotpatch_probe_script(fixture_port),
    &fixture.root().join("src/routes/admin/_layout.trs"),
    updated_unused_layout_source(),
  );
  wait_for_output_line(
    &output,
    |line| line.contains("refreshing hotpatch state"),
    Duration::from_secs(20),
  )
  .unwrap();
  let probe_output = probe
    .wait_with_output()
    .expect("Playwright no-op probe should complete");
  assert_playwright_probe_success(&probe_output, &output, "unused layout hotpatch browser probe");

  let probe_stdout = String::from_utf8_lossy(&probe_output.stdout);
  assert!(probe_stdout.contains(r#""beforeUnloadCount":"0""#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""refreshCount":0"#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""applyStyleCount":0"#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains(r#""templateBeforeStillVisible":true"#), "unexpected probe output: {probe_stdout}");
  assert!(probe_stdout.contains("noop-hotpatch-probe"), "unexpected probe output: {probe_stdout}");

  wait_for_runtime_handshake_count(&output, 1, Duration::from_secs(5)).unwrap();
  assert!(child.try_wait().expect("child wait should succeed").is_none());

  child.terminate();
  for handle in output_threads {
    let _ = handle.join();
  }
}

#[test]
fn unused_layout_edit_should_not_emit_browser_event_or_restart_runtime() {
  let _guard = hotpatch_test_guard();
  let fixture_port = reserve_port();
  let fixture = TestProject::new("unused-layout-noop");
  fixture.write("Cargo.toml", &fixture_cargo_toml());
  fixture.write("src/main.rs", &fixture_main_rs(fixture_port));
  fixture.write("src/routes/index.trs", initial_template_route_source());
  fixture.write("src/routes/admin/_layout.trs", initial_unused_layout_source());

  let mut child = spawn_hotpatch_process(fixture.root());
  let output = Arc::new(Mutex::new(Vec::<String>::new()));
  let output_threads = spawn_output_collectors(&mut child, Arc::clone(&output));

  wait_for_app_ready_with_output(fixture_port, &output);
  let browser_addr = wait_for_browser_addr(fixture.root());

  assert!(fetch_page(fixture_port, "/").contains("Template before"));

  let (event_rx, sse_handle) = open_event_stream(browser_addr);

  thread::sleep(Duration::from_millis(150));
  fixture.write("src/routes/admin/_layout.trs", updated_unused_layout_source());

  wait_for_output_line(
    &output,
    |line| line.contains("refreshing hotpatch state"),
    Duration::from_secs(20),
  )
  .unwrap_or_else(|error| {
    panic!(
      "{error}\nprocess output:\n{}",
      collected_lines(&output).join("\n")
    )
  });

  match event_rx.recv_timeout(Duration::from_secs(2)) {
    Err(mpsc::RecvTimeoutError::Timeout) => {}
    Err(mpsc::RecvTimeoutError::Disconnected) => {
      panic!(
        "event stream disconnected unexpectedly\nprocess output:\n{}",
        collected_lines(&output).join("\n")
      );
    }
    Ok(line) => {
      panic!(
        "unexpected browser event for unused layout edit: {line}\nprocess output:\n{}",
        collected_lines(&output).join("\n")
      );
    }
  }

  assert!(fetch_page(fixture_port, "/").contains("Template before"));
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
  wait_for_output_line(
    &output,
    |line| line.contains("thebe: hotpatch restart requested — Thebe-generated input changed"),
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
  wait_for_output_line(
    &output,
    |line| line.contains("thebe: hotpatch restart requested — application entry point changed"),
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

  #[cfg(unix)]
  fn interrupt_and_wait(
    &mut self,
    timeout: Duration,
  ) -> Result<std::process::ExitStatus, String> {
    let pid = self.child_mut().id();
    let status = Command::new("kill")
      .args(["-INT", &pid.to_string()])
      .status()
      .map_err(|error| format!("failed to send SIGINT to hotpatch process {pid}: {error}"))?;
    if !status.success() {
      return Err(format!(
        "kill -INT {pid} returned non-zero status {status}"
      ));
    }

    let started = Instant::now();
    while started.elapsed() < timeout {
      let exit_status = self
        .child
        .as_mut()
        .expect("managed child should be present")
        .try_wait()
        .map_err(|error| format!("failed to wait for hotpatch process {pid}: {error}"))?;
      if let Some(exit_status) = exit_status {
        self.child = None;
        return Ok(exit_status);
      }
      thread::sleep(Duration::from_millis(100));
    }

    self.terminate();
    Err(format!(
      "timed out waiting for hotpatch process {pid} to exit after SIGINT"
    ))
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

fn fixture_cargo_toml_with_ts_rs() -> String {
  format!("{}ts-rs = \"12\"\n", fixture_cargo_toml())
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

fn initial_client_script_route_source() -> &'static str {
  r#"<script setup>
struct Props {
  count: i32,
}

#[thebe::get]
pub fn index() -> Props {
  Props { count: 0 }
}
</script>

<script lang="ts">
let props = getProps<Props>();

function increment() {
  props.count += 1;
}
</script>

<button id="counter" onclick="increment">{{ count }}</button>
"#
}

fn updated_client_script_route_source() -> &'static str {
  r#"<script setup>
struct Props {
  count: i32,
}

#[thebe::get]
pub fn index() -> Props {
  Props { count: 0 }
}
</script>

<script lang="ts">
let props = getProps<Props>();

function increment() {
  props.count += 2;
}
</script>

<button id="counter" onclick="increment">{{ count }}</button>
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

fn route_using_stat_card_before_source() -> &'static str {
  r#"<script setup>
struct Props {
  name: String,
  tagline: String,
}

#[thebe::get]
pub fn index() -> Props {
  Props {
    name: "Thebe".to_owned(),
    tagline: "Hotpatch value".to_owned(),
  }
}
</script>

<main>
  <StatCard label="Template before" :value="name" />
</main>
"#
}

fn route_using_stat_card_after_source() -> &'static str {
  r#"<script setup>
struct Props {
  name: String,
  tagline: String,
}

#[thebe::get]
pub fn index() -> Props {
  Props {
    name: "Thebe".to_owned(),
    tagline: "Hotpatch value".to_owned(),
  }
}
</script>

<main>
  <StatCard label="Template after" :value="tagline" />
</main>
"#
}

fn route_using_counter_button_source() -> &'static str {
  r#"<script setup>
struct Props {
  count: i32,
}

#[thebe::get]
pub fn index() -> Props {
  Props { count: 0 }
}
</script>

<main>
  <CounterButton :count="count" />
</main>
"#
}

fn counter_button_component_source() -> &'static str {
  r#"<script>
pub struct Props {
  pub count: i32,
}
</script>

<script lang="ts">
let props = getProps<Props>();

function increment() {
  props.count += 1;
}
</script>

<button id="component-counter" onclick="increment">{{ props.count }}</button>
"#
}

fn updated_counter_button_component_source() -> &'static str {
  r#"<script>
pub struct Props {
  pub count: i32,
}
</script>

<script lang="ts">
let props = getProps<Props>();

function increment() {
  props.count += 2;
}
</script>

<button id="component-counter" onclick="increment">{{ props.count }}</button>
"#
}

fn stat_card_component_source() -> &'static str {
  r#"<script>
pub struct Props {
  pub label: String,
  pub value: String,
}
</script>

<article class="stat-card">
  <h2>{{ props.label }}</h2>
  <p>{{ props.value }}</p>
</article>
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

fn initial_layout_client_script_source() -> &'static str {
  r#"<script lang="ts">
let props = getProps<Props>();

function increment() {
  props.count += 1;
}
</script>

<div>
  <slot />
</div>
"#
}

fn updated_layout_client_script_source() -> &'static str {
  r#"<script lang="ts">
let props = getProps<Props>();

function increment() {
  props.count += 2;
}
</script>

<div>
  <slot />
</div>
"#
}

fn layout_client_script_route_source() -> &'static str {
  r#"<script setup>
struct Props {
  count: i32,
}

#[thebe::get]
pub fn index() -> Props {
  Props { count: 0 }
}
</script>

<button id="layout-counter" onclick="increment">{{ count }}</button>
"#
}

fn initial_unused_layout_source() -> &'static str {
  r#"<style>
.admin-shell {
  color: red;
}
</style>

<div class="admin-shell">
  <slot />
</div>
"#
}

fn updated_unused_layout_source() -> &'static str {
  r#"<style>
.admin-shell {
  color: blue;
}
</style>

<div class="admin-shell">
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

const STYLE_HOTPATCH_PROBE_SCRIPT: &str = r##"
const fs = require("node:fs");
const { chromium } = require("playwright");

const targetUrl = "__TARGET_URL__";
const patchFile = process.env.THEBE_HOTPATCH_FILE;
const patchSource = process.env.THEBE_HOTPATCH_SOURCE;
const beforeUnloadKey = "__thebe_style_beforeunload__";

(async () => {
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  const pageErrors = [];
  page.on("pageerror", (error) => {
    pageErrors.push(error && error.stack ? error.stack : String(error));
  });
  await page.addInitScript(() => {
    const registeredNames = [];
    const registeredHandlers = {};
    let realRegister = null;

    Object.defineProperty(window, "__thebeRegisteredNames", {
      configurable: true,
      value: registeredNames
    });
    Object.defineProperty(window, "__thebeRegisteredHandlers", {
      configurable: true,
      value: registeredHandlers
    });
    Object.defineProperty(window, "__thebe_register", {
      configurable: true,
      get() {
        return realRegister;
      },
      set(fn) {
        realRegister = function(name, handler) {
          registeredNames.push(name);
          registeredHandlers[name] = handler;
          return fn(name, handler);
        };
      }
    });
  });

  try {
    await page.goto(targetUrl, { waitUntil: "domcontentloaded" });
    await page.waitForFunction(
      () => typeof window.__thebe_dev_apply_style === "function",
      null,
      { timeout: 30000 }
    );
    await page.locator("h1").waitFor({ state: "attached", timeout: 30000 });
    await page.evaluate((key) => {
      window.sessionStorage.removeItem(key);
      window.addEventListener("beforeunload", () => {
        const count = Number(window.sessionStorage.getItem(key) || "0");
        window.sessionStorage.setItem(key, String(count + 1));
      });
      window.__thebeStyleProbeToken = "style-hotpatch-probe";
    }, beforeUnloadKey);

    const initialColor = await page.locator("h1").evaluate((el) => getComputedStyle(el).color);
    fs.writeFileSync(patchFile, patchSource);

    await page.waitForFunction(
      ({ initialColor, beforeUnloadKey }) => {
        const heading = document.querySelector("h1");
        const managedStyles = Array.from(
          document.head.querySelectorAll('[data-thebe-head="style"]')
        );

        return Boolean(
          heading &&
            getComputedStyle(heading).color !== initialColor &&
            window.__thebeStyleProbeToken === "style-hotpatch-probe" &&
            (window.sessionStorage.getItem(beforeUnloadKey) || "0") === "0" &&
            managedStyles.length === 1 &&
            managedStyles.some((style) => {
              const text = style.textContent || "";
              return text.includes("blue") || text.includes("#00f") || text.includes("#0000ff");
            })
        );
      },
      { initialColor, beforeUnloadKey },
      { timeout: 30000 }
    );

    const result = await page.evaluate((key) => {
      const heading = document.querySelector("h1");
      const managedStyles = Array.from(
        document.head.querySelectorAll('[data-thebe-head="style"]')
      );

      return {
        beforeUnloadCount: window.sessionStorage.getItem(key) || "0",
        color: heading ? getComputedStyle(heading).color : null,
        probeToken: window.__thebeStyleProbeToken || null,
        managedStyleCount: managedStyles.length,
        managedStyleText: managedStyles.map((style) => style.textContent || "").join("\n")
      };
    }, beforeUnloadKey);

    console.log(JSON.stringify(result));
  } catch (error) {
    console.error(error && error.stack ? error.stack : String(error));
    process.exitCode = 1;
  } finally {
    await browser.close();
  }
})();
"##;

const LAYOUT_HEAD_HOTPATCH_PROBE_SCRIPT: &str = r#"
const fs = require("node:fs");
const { chromium } = require("playwright");

const targetUrl = "__TARGET_URL__";
const patchFile = process.env.THEBE_HOTPATCH_FILE;
const patchSource = process.env.THEBE_HOTPATCH_SOURCE;
const beforeUnloadKey = "__thebe_layout_head_beforeunload__";

(async () => {
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  const pageErrors = [];
  page.on("pageerror", (error) => {
    pageErrors.push(error && error.stack ? error.stack : String(error));
  });

  try {
    await page.goto(targetUrl, { waitUntil: "domcontentloaded" });
    await page.waitForFunction(
      () => typeof window.__thebe_dev_refresh === "function",
      null,
      { timeout: 30000 }
    );
    await page.waitForFunction(
      () => {
        const meta = document.head.querySelector('meta[name="layout-probe"]');
        return meta && meta.getAttribute("content") === "Layout before";
      },
      null,
      { timeout: 30000 }
    );
    await page.evaluate((key) => {
      window.sessionStorage.removeItem(key);
      window.addEventListener("beforeunload", () => {
        const count = Number(window.sessionStorage.getItem(key) || "0");
        window.sessionStorage.setItem(key, String(count + 1));
      });
      window.__thebeLayoutHeadProbeToken = "layout-head-hotpatch-probe";
    }, beforeUnloadKey);

    fs.writeFileSync(patchFile, patchSource);

    await page.waitForFunction(
      (beforeUnloadKey) => {
        const meta = document.head.querySelector('meta[name="layout-probe"]');

        return Boolean(
          meta &&
            meta.getAttribute("content") === "Layout after" &&
            window.__thebeLayoutHeadProbeToken === "layout-head-hotpatch-probe" &&
            (window.sessionStorage.getItem(beforeUnloadKey) || "0") === "0" &&
            document.body.textContent.includes("Layout route body")
        );
      },
      beforeUnloadKey,
      { timeout: 30000 }
    );

    const result = await page.evaluate((key) => {
      const meta = document.head.querySelector('meta[name="layout-probe"]');

      return {
        beforeUnloadCount: window.sessionStorage.getItem(key) || "0",
        metaContent: meta ? meta.getAttribute("content") : null,
        probeToken: window.__thebeLayoutHeadProbeToken || null
      };
    }, beforeUnloadKey);

    console.log(JSON.stringify(result));
  } catch (error) {
    console.error(error && error.stack ? error.stack : String(error));
    process.exitCode = 1;
  } finally {
    await browser.close();
  }
})();
"#;

const ROUTE_TEMPLATE_HOTPATCH_PROBE_SCRIPT: &str = r#"
const fs = require("node:fs");
const { chromium } = require("playwright");

const targetUrl = "__TARGET_URL__";
const patchFile = process.env.THEBE_HOTPATCH_FILE;
const patchSource = process.env.THEBE_HOTPATCH_SOURCE;
const beforeUnloadKey = "__thebe_template_route_beforeunload__";

(async () => {
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  const pageErrors = [];
  page.on("pageerror", (error) => {
    pageErrors.push(error && error.stack ? error.stack : String(error));
  });

  try {
    await page.goto(targetUrl, { waitUntil: "domcontentloaded" });
    await page.waitForFunction(
      () => typeof window.__thebe_dev_refresh === "function",
      null,
      { timeout: 30000 }
    );
    await page.waitForFunction(
      () => document.body.textContent.includes("Template before"),
      null,
      { timeout: 30000 }
    );
    await page.evaluate((key) => {
      const originalRefresh = window.__thebe_dev_refresh;
      window.sessionStorage.removeItem(key);
      window.__thebeTemplateRouteRefreshCount = 0;
      window.addEventListener("beforeunload", () => {
        const count = Number(window.sessionStorage.getItem(key) || "0");
        window.sessionStorage.setItem(key, String(count + 1));
      });
      window.__thebe_dev_refresh = function (...args) {
        window.__thebeTemplateRouteRefreshCount += 1;
        return originalRefresh.apply(this, args);
      };
      window.__thebeTemplateRouteProbeToken = "template-route-hotpatch-probe";
    }, beforeUnloadKey);

    fs.writeFileSync(patchFile, patchSource);

    await page.waitForFunction(
      (beforeUnloadKey) =>
        document.body.textContent.includes("Template after") &&
        window.__thebeTemplateRouteProbeToken === "template-route-hotpatch-probe" &&
        (window.sessionStorage.getItem(beforeUnloadKey) || "0") === "0" &&
        (window.__thebeTemplateRouteRefreshCount || 0) >= 1,
      beforeUnloadKey,
      { timeout: 30000 }
    );

    const result = await page.evaluate((key) => ({
      beforeUnloadCount: window.sessionStorage.getItem(key) || "0",
      refreshCount: window.__thebeTemplateRouteRefreshCount || 0,
      templateSeen: document.body.textContent.includes("Template after"),
      probeToken: window.__thebeTemplateRouteProbeToken || null
    }), beforeUnloadKey);

    console.log(JSON.stringify(result));
  } catch (error) {
    console.error(error && error.stack ? error.stack : String(error));
    process.exitCode = 1;
  } finally {
    await browser.close();
  }
})();
"#;

const ROUTE_CLIENT_SCRIPT_HOTPATCH_PROBE_SCRIPT: &str = r##"
const fs = require("node:fs");
const { chromium } = require("playwright");

const targetUrl = "__TARGET_URL__";
const patchFile = process.env.THEBE_HOTPATCH_FILE;
const patchSource = process.env.THEBE_HOTPATCH_SOURCE;
const beforeUnloadKey = "__thebe_route_client_script_beforeunload__";

(async () => {
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();

  try {
    await page.goto(targetUrl, { waitUntil: "domcontentloaded" });
    await page.waitForFunction(
      () => typeof window.__thebe_dev_refresh === "function",
      null,
      { timeout: 30000 }
    );
    await page.locator("#counter").waitFor({ state: "attached", timeout: 30000 });
    await page.waitForFunction(
      () => {
        const button = document.querySelector("#counter");
        return button && button.textContent.trim() === "0";
      },
      null,
      { timeout: 30000 }
    );

    await page.evaluate((key) => {
      const originalRefresh = window.__thebe_dev_refresh;
      window.sessionStorage.removeItem(key);
      window.__thebeRouteClientScriptRefreshCount = 0;
      window.addEventListener("beforeunload", () => {
        const count = Number(window.sessionStorage.getItem(key) || "0");
        window.sessionStorage.setItem(key, String(count + 1));
      });
      window.__thebe_dev_refresh = function (...args) {
        window.__thebeRouteClientScriptRefreshCount += 1;
        return originalRefresh.apply(this, args);
      };
      window.__thebeRouteClientScriptProbeToken = "route-client-script-hotpatch-probe";
    }, beforeUnloadKey);

    await page.locator("#counter").click();
    await page.waitForFunction(
      () => {
        const button = document.querySelector("#counter");
        return button && button.textContent.trim() === "1";
      },
      null,
      { timeout: 30000 }
    );

    fs.writeFileSync(patchFile, patchSource);

    await page.waitForFunction(
      (beforeUnloadKey) => {
        const button = document.querySelector("#counter");

        return Boolean(
          button &&
            button.textContent.trim() === "0" &&
            window.__thebeRouteClientScriptProbeToken === "route-client-script-hotpatch-probe" &&
            (window.sessionStorage.getItem(beforeUnloadKey) || "0") === "0" &&
            (window.__thebeRouteClientScriptRefreshCount || 0) >= 1
        );
      },
      beforeUnloadKey,
      { timeout: 30000 }
    );

    await page.locator("#counter").click();
    await page.waitForFunction(
      () => {
        const button = document.querySelector("#counter");
        return button && button.textContent.trim() === "2";
      },
      null,
      { timeout: 30000 }
    );

    const result = await page.evaluate((key) => {
      const button = document.querySelector("#counter");

      return {
        beforeUnloadCount: window.sessionStorage.getItem(key) || "0",
        refreshCount: window.__thebeRouteClientScriptRefreshCount || 0,
        initialHandlerCount: "1",
        postPatchCount: button ? button.textContent.trim() : null,
        probeToken: window.__thebeRouteClientScriptProbeToken || null
      };
    }, beforeUnloadKey);

    console.log(JSON.stringify(result));
  } catch (error) {
    console.error(error && error.stack ? error.stack : String(error));
    process.exitCode = 1;
  } finally {
    await browser.close();
  }
})();
"##;

const COMPONENT_CLIENT_SCRIPT_HOTPATCH_PROBE_SCRIPT: &str = r##"
const fs = require("node:fs");
const { chromium } = require("playwright");

const targetUrl = "__TARGET_URL__";
const patchFile = process.env.THEBE_HOTPATCH_FILE;
const patchSource = process.env.THEBE_HOTPATCH_SOURCE;
const beforeUnloadKey = "__thebe_component_client_script_hotpatch_beforeunload__";

(async () => {
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();

  try {
    await page.goto(targetUrl, { waitUntil: "domcontentloaded" });
    await page.waitForFunction(
      () => typeof window.__thebe_dev_refresh === "function",
      null,
      { timeout: 30000 }
    );
    await page.locator("#component-counter").waitFor({ state: "attached", timeout: 30000 });
    await page.waitForFunction(
      () => {
        const button = document.querySelector("#component-counter");
        return button && button.textContent.trim() === "0";
      },
      null,
      { timeout: 30000 }
    );

    await page.evaluate((key) => {
      const originalRefresh = window.__thebe_dev_refresh;
      window.sessionStorage.removeItem(key);
      window.__thebeComponentClientScriptHotpatchRefreshCount = 0;
      window.addEventListener("beforeunload", () => {
        const count = Number(window.sessionStorage.getItem(key) || "0");
        window.sessionStorage.setItem(key, String(count + 1));
      });
      window.__thebe_dev_refresh = function (...args) {
        window.__thebeComponentClientScriptHotpatchRefreshCount += 1;
        return originalRefresh.apply(this, args);
      };
      window.__thebeComponentClientScriptHotpatchProbeToken = "component-client-script-hotpatch-probe";
    }, beforeUnloadKey);

    await page.locator("#component-counter").click();
    await page.waitForFunction(
      () => {
        const button = document.querySelector("#component-counter");
        return button && button.textContent.trim() === "1";
      },
      null,
      { timeout: 30000 }
    );

    fs.writeFileSync(patchFile, patchSource);

    await page.waitForFunction(
      (beforeUnloadKey) => {
        const button = document.querySelector("#component-counter");

        return Boolean(
          button &&
            button.textContent.trim() === "0" &&
            window.__thebeComponentClientScriptHotpatchProbeToken === "component-client-script-hotpatch-probe" &&
            (window.sessionStorage.getItem(beforeUnloadKey) || "0") === "0" &&
            (window.__thebeComponentClientScriptHotpatchRefreshCount || 0) >= 1
        );
      },
      beforeUnloadKey,
      { timeout: 30000 }
    );

    await page.locator("#component-counter").click();
    await page.waitForFunction(
      () => {
        const button = document.querySelector("#component-counter");
        return button && button.textContent.trim() === "2";
      },
      null,
      { timeout: 30000 }
    );

    const result = await page.evaluate((key) => {
      const button = document.querySelector("#component-counter");

      return {
        beforeUnloadCount: window.sessionStorage.getItem(key) || "0",
        refreshCount: window.__thebeComponentClientScriptHotpatchRefreshCount || 0,
        initialHandlerCount: "1",
        postPatchCount: button ? button.textContent.trim() : null,
        probeToken: window.__thebeComponentClientScriptHotpatchProbeToken || null
      };
    }, beforeUnloadKey);

    console.log(JSON.stringify(result));
  } catch (error) {
    console.error(error && error.stack ? error.stack : String(error));
    process.exitCode = 1;
  } finally {
    await browser.close();
  }
})();
"##;

const LAYOUT_CLIENT_SCRIPT_HOTPATCH_PROBE_SCRIPT: &str = r##"
const fs = require("node:fs");
const { chromium } = require("playwright");

const targetUrl = "__TARGET_URL__";
const patchFile = process.env.THEBE_HOTPATCH_FILE;
const patchSource = process.env.THEBE_HOTPATCH_SOURCE;
const beforeUnloadKey = "__thebe_layout_client_script_hotpatch_beforeunload__";

(async () => {
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();

  try {
    await page.goto(targetUrl, { waitUntil: "domcontentloaded" });
    await page.waitForFunction(
      () => typeof window.__thebe_dev_refresh === "function",
      null,
      { timeout: 30000 }
    );
    await page.locator("#layout-counter").waitFor({ state: "attached", timeout: 30000 });
    await page.waitForFunction(
      () => {
        const button = document.querySelector("#layout-counter");
        return button && button.textContent.trim() === "0";
      },
      null,
      { timeout: 30000 }
    );

    await page.evaluate((key) => {
      const originalRefresh = window.__thebe_dev_refresh;
      window.sessionStorage.removeItem(key);
      window.__thebeLayoutClientScriptHotpatchRefreshCount = 0;
      window.addEventListener("beforeunload", () => {
        const count = Number(window.sessionStorage.getItem(key) || "0");
        window.sessionStorage.setItem(key, String(count + 1));
      });
      window.__thebe_dev_refresh = function (...args) {
        window.__thebeLayoutClientScriptHotpatchRefreshCount += 1;
        return originalRefresh.apply(this, args);
      };
      window.__thebeLayoutClientScriptHotpatchProbeToken = "layout-client-script-hotpatch-probe";
    }, beforeUnloadKey);

    await page.locator("#layout-counter").click();
    await page.waitForFunction(
      () => {
        const button = document.querySelector("#layout-counter");
        return button && button.textContent.trim() === "1";
      },
      null,
      { timeout: 30000 }
    );

    fs.writeFileSync(patchFile, patchSource);

    await page.waitForFunction(
      (beforeUnloadKey) => {
        const button = document.querySelector("#layout-counter");

        return Boolean(
          button &&
            button.textContent.trim() === "0" &&
            window.__thebeLayoutClientScriptHotpatchProbeToken === "layout-client-script-hotpatch-probe" &&
            (window.sessionStorage.getItem(beforeUnloadKey) || "0") === "0" &&
            (window.__thebeLayoutClientScriptHotpatchRefreshCount || 0) >= 1
        );
      },
      beforeUnloadKey,
      { timeout: 30000 }
    );

    await page.locator("#layout-counter").click();
    await page.waitForFunction(
      () => {
        const button = document.querySelector("#layout-counter");
        return button && button.textContent.trim() === "2";
      },
      null,
      { timeout: 30000 }
    );

    const result = await page.evaluate((key) => {
      const button = document.querySelector("#layout-counter");

      return {
        beforeUnloadCount: window.sessionStorage.getItem(key) || "0",
        refreshCount: window.__thebeLayoutClientScriptHotpatchRefreshCount || 0,
        initialHandlerCount: "1",
        postPatchCount: button ? button.textContent.trim() : null,
        probeToken: window.__thebeLayoutClientScriptHotpatchProbeToken || null
      };
    }, beforeUnloadKey);

    console.log(JSON.stringify(result));
  } catch (error) {
    console.error(error && error.stack ? error.stack : String(error));
    process.exitCode = 1;
  } finally {
    await browser.close();
  }
})();
"##;

const COMPONENT_CLIENT_SCRIPT_PROBE_SCRIPT: &str = r##"
const { chromium } = require("playwright");

const targetUrl = "__TARGET_URL__";
const beforeUnloadKey = "__thebe_component_client_script_beforeunload__";

(async () => {
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  const pageErrors = [];
  page.on("pageerror", (error) => {
    pageErrors.push(error && error.stack ? error.stack : String(error));
  });

  try {
    await page.goto(targetUrl, { waitUntil: "domcontentloaded" });
    await page.waitForFunction(
      () => typeof window.__thebe_dev_refresh === "function",
      null,
      { timeout: 30000 }
    );
    await page.locator("#component-counter").waitFor({ state: "attached", timeout: 30000 });
    await page.waitForFunction(
      () => {
        const button = document.querySelector("#component-counter");
        return button && button.textContent.trim() === "0";
      },
      null,
      { timeout: 30000 }
    );

    await page.evaluate((key) => {
      window.sessionStorage.removeItem(key);
      window.__thebeComponentClientScriptErrors = [];
      window.addEventListener("error", (event) => {
        const message = event && event.error && event.error.stack
          ? event.error.stack
          : (event && event.message) || "unknown error";
        window.__thebeComponentClientScriptErrors.push(message);
      });
      window.addEventListener("beforeunload", () => {
        const count = Number(window.sessionStorage.getItem(key) || "0");
        window.sessionStorage.setItem(key, String(count + 1));
      });
      window.__thebeComponentClientScriptProbeToken = "component-client-script-probe";
    }, beforeUnloadKey);

    await page.locator("#component-counter").click();

    await page.waitForFunction(
      () => {
        const button = document.querySelector("#component-counter");
        return button && button.textContent.trim() === "1";
      },
      null,
      { timeout: 30000 }
    );

    const result = await page.evaluate((key) => {
      const button = document.querySelector("#component-counter");

      return {
        beforeUnloadCount: window.sessionStorage.getItem(key) || "0",
        count: button ? button.textContent.trim() : null,
        probeToken: window.__thebeComponentClientScriptProbeToken || null,
        errors: window.__thebeComponentClientScriptErrors || []
      };
    }, beforeUnloadKey);

    result.pageErrors = pageErrors;

    console.log(JSON.stringify(result));
  } catch (error) {
    const diagnostics = await page.evaluate(() => {
      const button = document.querySelector("#component-counter");

      return {
        count: button ? button.textContent.trim() : null,
        onclickAttr: button ? button.getAttribute("onclick") : null,
        errors: window.__thebeComponentClientScriptErrors || []
      };
    }).catch(() => null);
    if (diagnostics) {
      console.error(JSON.stringify({ diagnostics, pageErrors }));
    }
    console.error(error && error.stack ? error.stack : String(error));
    process.exitCode = 1;
  } finally {
    await browser.close();
  }
})();
"##;

const COMPONENT_STYLE_HOTPATCH_PROBE_SCRIPT: &str = r#"
const fs = require("node:fs");
const { chromium } = require("playwright");

const affectedUrl = "__AFFECTED_URL__";
const unaffectedUrl = "__UNAFFECTED_URL__";
const patchFile = process.env.THEBE_HOTPATCH_FILE;
const patchSource = process.env.THEBE_HOTPATCH_SOURCE;
const affectedBeforeUnloadKey = "__thebe_component_style_affected_beforeunload__";
const unaffectedBeforeUnloadKey = "__thebe_component_style_unaffected_beforeunload__";

(async () => {
  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext();
  const affectedPage = await context.newPage();
  const unaffectedPage = await context.newPage();

  try {
    await Promise.all([
      affectedPage.goto(affectedUrl, { waitUntil: "domcontentloaded" }),
      unaffectedPage.goto(unaffectedUrl, { waitUntil: "domcontentloaded" })
    ]);
    await Promise.all([
      affectedPage.waitForFunction(
        () => typeof window.__thebe_dev_apply_style === "function",
        null,
        { timeout: 30000 }
      ),
      unaffectedPage.waitForFunction(
        () => typeof window.__thebe_dev_apply_style === "function",
        null,
        { timeout: 30000 }
      )
    ]);
    await affectedPage.locator(".card").waitFor({ state: "attached", timeout: 30000 });
    await unaffectedPage.waitForFunction(
      () => document.body.textContent.includes("About page"),
      null,
      { timeout: 30000 }
    );

    await affectedPage.evaluate((key) => {
      const originalApplyStyle = window.__thebe_dev_apply_style;
      window.sessionStorage.removeItem(key);
      window.__thebeComponentStyleApplyCount = 0;
      window.addEventListener("beforeunload", () => {
        const count = Number(window.sessionStorage.getItem(key) || "0");
        window.sessionStorage.setItem(key, String(count + 1));
      });
      window.__thebe_dev_apply_style = function (...args) {
        window.__thebeComponentStyleApplyCount += 1;
        return originalApplyStyle.apply(this, args);
      };
      window.__thebeComponentStyleProbeToken = "component-style-affected-probe";
    }, affectedBeforeUnloadKey);
    await unaffectedPage.evaluate((key) => {
      const originalApplyStyle = window.__thebe_dev_apply_style;
      window.sessionStorage.removeItem(key);
      window.__thebeComponentStyleApplyCount = 0;
      window.addEventListener("beforeunload", () => {
        const count = Number(window.sessionStorage.getItem(key) || "0");
        window.sessionStorage.setItem(key, String(count + 1));
      });
      window.__thebe_dev_apply_style = function (...args) {
        window.__thebeComponentStyleApplyCount += 1;
        return originalApplyStyle.apply(this, args);
      };
      window.__thebeComponentStyleProbeToken = "component-style-unaffected-probe";
    }, unaffectedBeforeUnloadKey);

    const initialAffectedColor = await affectedPage.locator(".card").evaluate((el) => getComputedStyle(el).color);
    fs.writeFileSync(patchFile, patchSource);

    await affectedPage.waitForFunction(
      ({ initialAffectedColor, affectedBeforeUnloadKey }) => {
        const card = document.querySelector(".card");
        return Boolean(
          card &&
            getComputedStyle(card).color !== initialAffectedColor &&
            window.__thebeComponentStyleProbeToken === "component-style-affected-probe" &&
            (window.sessionStorage.getItem(affectedBeforeUnloadKey) || "0") === "0" &&
            (window.__thebeComponentStyleApplyCount || 0) >= 1
        );
      },
      { initialAffectedColor, affectedBeforeUnloadKey },
      { timeout: 30000 }
    );

    const result = {
      affected: await affectedPage.evaluate((key) => ({
        beforeUnloadCount: window.sessionStorage.getItem(key) || "0",
        applyCount: window.__thebeComponentStyleApplyCount || 0,
        probeToken: window.__thebeComponentStyleProbeToken || null,
        computedColor: (() => {
          const card = document.querySelector(".card");
          return card ? getComputedStyle(card).color : null;
        })()
      }), affectedBeforeUnloadKey),
      unaffected: await unaffectedPage.evaluate((key) => ({
        beforeUnloadCount: window.sessionStorage.getItem(key) || "0",
        applyCount: window.__thebeComponentStyleApplyCount || 0,
        probeToken: window.__thebeComponentStyleProbeToken || null,
        stillAbout: document.body.textContent.includes("About page"),
        hasCard: Boolean(document.querySelector(".card"))
      }), unaffectedBeforeUnloadKey)
    };

    console.log(JSON.stringify({
      affectedBeforeUnloadCount: result.affected.beforeUnloadCount,
      affectedApplyCount: result.affected.applyCount,
      affectedColor: result.affected.computedColor,
      affectedProbeToken: result.affected.probeToken,
      unaffectedBeforeUnloadCount: result.unaffected.beforeUnloadCount,
      unaffectedApplyCount: result.unaffected.applyCount,
      unaffectedStillAbout: result.unaffected.stillAbout,
      unaffectedHasCard: result.unaffected.hasCard,
      unaffectedProbeToken: result.unaffected.probeToken
    }));
  } catch (error) {
    console.error(error && error.stack ? error.stack : String(error));
    process.exitCode = 1;
  } finally {
    await context.close();
    await browser.close();
  }
})();
"#;

const COMPONENT_TEMPLATE_HOTPATCH_PROBE_SCRIPT: &str = r#"
const fs = require("node:fs");
const { chromium } = require("playwright");

const affectedUrl = "__AFFECTED_URL__";
const unaffectedUrl = "__UNAFFECTED_URL__";
const patchFile = process.env.THEBE_HOTPATCH_FILE;
const patchSource = process.env.THEBE_HOTPATCH_SOURCE;
const affectedBeforeUnloadKey = "__thebe_component_template_affected_beforeunload__";
const unaffectedBeforeUnloadKey = "__thebe_component_template_unaffected_beforeunload__";

(async () => {
  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext();
  const affectedPage = await context.newPage();
  const unaffectedPage = await context.newPage();

  try {
    await Promise.all([
      affectedPage.goto(affectedUrl, { waitUntil: "domcontentloaded" }),
      unaffectedPage.goto(unaffectedUrl, { waitUntil: "domcontentloaded" })
    ]);
    await Promise.all([
      affectedPage.waitForFunction(
        () => typeof window.__thebe_dev_refresh === "function",
        null,
        { timeout: 30000 }
      ),
      unaffectedPage.waitForFunction(
        () => typeof window.__thebe_dev_refresh === "function",
        null,
        { timeout: 30000 }
      )
    ]);
    await affectedPage.waitForFunction(
      () => document.body.textContent.includes("Component before"),
      null,
      { timeout: 30000 }
    );
    await unaffectedPage.waitForFunction(
      () => document.body.textContent.includes("About page"),
      null,
      { timeout: 30000 }
    );

    await affectedPage.evaluate((key) => {
      const originalRefresh = window.__thebe_dev_refresh;
      window.sessionStorage.removeItem(key);
      window.__thebeComponentTemplateRefreshCount = 0;
      window.addEventListener("beforeunload", () => {
        const count = Number(window.sessionStorage.getItem(key) || "0");
        window.sessionStorage.setItem(key, String(count + 1));
      });
      window.__thebe_dev_refresh = function (...args) {
        window.__thebeComponentTemplateRefreshCount += 1;
        return originalRefresh.apply(this, args);
      };
      window.__thebeComponentTemplateProbeToken = "component-template-affected-probe";
    }, affectedBeforeUnloadKey);
    await unaffectedPage.evaluate((key) => {
      const originalRefresh = window.__thebe_dev_refresh;
      window.sessionStorage.removeItem(key);
      window.__thebeComponentTemplateRefreshCount = 0;
      window.addEventListener("beforeunload", () => {
        const count = Number(window.sessionStorage.getItem(key) || "0");
        window.sessionStorage.setItem(key, String(count + 1));
      });
      window.__thebe_dev_refresh = function (...args) {
        window.__thebeComponentTemplateRefreshCount += 1;
        return originalRefresh.apply(this, args);
      };
      window.__thebeComponentTemplateProbeToken = "component-template-unaffected-probe";
    }, unaffectedBeforeUnloadKey);

    fs.writeFileSync(patchFile, patchSource);

    await affectedPage.waitForFunction(
      (affectedBeforeUnloadKey) =>
        document.body.textContent.includes("Component after") &&
        window.__thebeComponentTemplateProbeToken === "component-template-affected-probe" &&
        (window.sessionStorage.getItem(affectedBeforeUnloadKey) || "0") === "0" &&
        (window.__thebeComponentTemplateRefreshCount || 0) >= 1,
      affectedBeforeUnloadKey,
      { timeout: 30000 }
    );

    const result = {
      affected: await affectedPage.evaluate((key) => ({
        beforeUnloadCount: window.sessionStorage.getItem(key) || "0",
        refreshCount: window.__thebeComponentTemplateRefreshCount || 0,
        probeToken: window.__thebeComponentTemplateProbeToken || null,
        templateSeen: document.body.textContent.includes("Component after")
      }), affectedBeforeUnloadKey),
      unaffected: await unaffectedPage.evaluate((key) => ({
        beforeUnloadCount: window.sessionStorage.getItem(key) || "0",
        refreshCount: window.__thebeComponentTemplateRefreshCount || 0,
        probeToken: window.__thebeComponentTemplateProbeToken || null,
        stillAbout: document.body.textContent.includes("About page"),
        templateSeen: document.body.textContent.includes("Component after")
      }), unaffectedBeforeUnloadKey)
    };

    console.log(JSON.stringify({
      affectedBeforeUnloadCount: result.affected.beforeUnloadCount,
      affectedRefreshCount: result.affected.refreshCount,
      affectedTemplateSeen: result.affected.templateSeen,
      affectedProbeToken: result.affected.probeToken,
      unaffectedBeforeUnloadCount: result.unaffected.beforeUnloadCount,
      unaffectedRefreshCount: result.unaffected.refreshCount,
      unaffectedStillAbout: result.unaffected.stillAbout,
      unaffectedTemplateSeen: result.unaffected.templateSeen,
      unaffectedProbeToken: result.unaffected.probeToken
    }));
  } catch (error) {
    console.error(error && error.stack ? error.stack : String(error));
    process.exitCode = 1;
  } finally {
    await context.close();
    await browser.close();
  }
})();
"#;

const NOOP_HOTPATCH_PROBE_SCRIPT: &str = r#"
const fs = require("node:fs");
const { chromium } = require("playwright");

const targetUrl = "__TARGET_URL__";
const patchFile = process.env.THEBE_HOTPATCH_FILE;
const patchSource = process.env.THEBE_HOTPATCH_SOURCE;
const beforeUnloadKey = "__thebe_noop_beforeunload__";

(async () => {
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();

  try {
    await page.goto(targetUrl, { waitUntil: "domcontentloaded" });
    await page.waitForFunction(
      () =>
        typeof window.__thebe_dev_refresh === "function" &&
        typeof window.__thebe_dev_apply_style === "function",
      null,
      { timeout: 30000 }
    );
    await page.waitForFunction(
      () => document.body.textContent.includes("Template before"),
      null,
      { timeout: 30000 }
    );
    await page.evaluate((key) => {
      const originalRefresh = window.__thebe_dev_refresh;
      const originalApplyStyle = window.__thebe_dev_apply_style;
      window.sessionStorage.removeItem(key);
      window.__thebeNoopRefreshCount = 0;
      window.__thebeNoopApplyStyleCount = 0;
      window.addEventListener("beforeunload", () => {
        const count = Number(window.sessionStorage.getItem(key) || "0");
        window.sessionStorage.setItem(key, String(count + 1));
      });
      window.__thebe_dev_refresh = function (...args) {
        window.__thebeNoopRefreshCount += 1;
        return originalRefresh.apply(this, args);
      };
      window.__thebe_dev_apply_style = function (...args) {
        window.__thebeNoopApplyStyleCount += 1;
        return originalApplyStyle.apply(this, args);
      };
      window.__thebeNoopProbeToken = "noop-hotpatch-probe";
    }, beforeUnloadKey);

    fs.writeFileSync(patchFile, patchSource);
    await page.waitForTimeout(3000);

    const result = await page.evaluate((key) => ({
      beforeUnloadCount: window.sessionStorage.getItem(key) || "0",
      refreshCount: window.__thebeNoopRefreshCount || 0,
      applyStyleCount: window.__thebeNoopApplyStyleCount || 0,
      templateBeforeStillVisible: document.body.textContent.includes("Template before"),
      probeToken: window.__thebeNoopProbeToken || null
    }), beforeUnloadKey);

    console.log(JSON.stringify(result));
  } catch (error) {
    console.error(error && error.stack ? error.stack : String(error));
    process.exitCode = 1;
  } finally {
    await browser.close();
  }
})();
"#;

const SHUTDOWN_HOTPATCH_PROBE_SCRIPT: &str = r#"
const { chromium } = require("playwright");

const targetUrl = "__TARGET_URL__";

(async () => {
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  const pageErrors = [];
  page.on("pageerror", (error) => {
    pageErrors.push(error && error.stack ? error.stack : String(error));
  });

  await page.addInitScript(() => {
    const NativeEventSource = window.EventSource;
    const probe = {
      openCount: 0,
      eventSourceUrl: null
    };

    Object.defineProperty(window, "__thebeShutdownProbe", {
      configurable: true,
      value: probe
    });

    if (typeof NativeEventSource !== "function") {
      probe.unsupported = true;
      return;
    }

    class PatchedEventSource extends NativeEventSource {
      constructor(...args) {
        super(...args);
        probe.eventSourceUrl = String(args[0] || "");
        this.addEventListener("open", () => {
          probe.openCount += 1;
        });
      }
    }

    PatchedEventSource.CONNECTING = NativeEventSource.CONNECTING;
    PatchedEventSource.OPEN = NativeEventSource.OPEN;
    PatchedEventSource.CLOSED = NativeEventSource.CLOSED;
    window.EventSource = PatchedEventSource;
  });

  try {
    await page.goto(targetUrl, { waitUntil: "domcontentloaded" });
    await page.waitForFunction(
      () =>
        window.__thebeShutdownProbe &&
        window.__thebeShutdownProbe.openCount > 0 &&
        typeof window.__thebe_dev_refresh === "function" &&
        typeof window.__thebe_dev_apply_style === "function",
      null,
      { timeout: 30000 }
    );

    const result = await page.evaluate(() => ({
      openCount: window.__thebeShutdownProbe.openCount,
      eventSourceUrl: window.__thebeShutdownProbe.eventSourceUrl,
      hasRefresh: typeof window.__thebe_dev_refresh === "function",
      hasStyleApply: typeof window.__thebe_dev_apply_style === "function"
    }));
    console.log(`event-source-opened ${JSON.stringify(result)}`);

    await new Promise(() => {});
  } catch (error) {
    console.error(error && error.stack ? error.stack : String(error));
    if (pageErrors.length > 0) {
      console.error(pageErrors.join("\n"));
    }
    process.exitCode = 1;
  } finally {
    await browser.close();
  }
})();
"#;

fn style_hotpatch_probe_script(port: u16) -> String {
  STYLE_HOTPATCH_PROBE_SCRIPT.replace("__TARGET_URL__", &format!("http://127.0.0.1:{port}/"))
}

fn layout_head_hotpatch_probe_script(port: u16) -> String {
  LAYOUT_HEAD_HOTPATCH_PROBE_SCRIPT.replace("__TARGET_URL__", &format!("http://127.0.0.1:{port}/"))
}

fn route_template_hotpatch_probe_script(port: u16) -> String {
  ROUTE_TEMPLATE_HOTPATCH_PROBE_SCRIPT.replace("__TARGET_URL__", &format!("http://127.0.0.1:{port}/"))
}

fn route_client_script_hotpatch_probe_script(port: u16) -> String {
  ROUTE_CLIENT_SCRIPT_HOTPATCH_PROBE_SCRIPT
    .replace("__TARGET_URL__", &format!("http://127.0.0.1:{port}/"))
}

fn component_client_script_hotpatch_probe_script(port: u16) -> String {
  COMPONENT_CLIENT_SCRIPT_HOTPATCH_PROBE_SCRIPT
    .replace("__TARGET_URL__", &format!("http://127.0.0.1:{port}/"))
}

fn layout_client_script_hotpatch_probe_script(port: u16) -> String {
  LAYOUT_CLIENT_SCRIPT_HOTPATCH_PROBE_SCRIPT
    .replace("__TARGET_URL__", &format!("http://127.0.0.1:{port}/"))
}

fn component_client_script_probe_script(port: u16) -> String {
  COMPONENT_CLIENT_SCRIPT_PROBE_SCRIPT
    .replace("__TARGET_URL__", &format!("http://127.0.0.1:{port}/"))
}

fn component_style_hotpatch_probe_script(port: u16) -> String {
  COMPONENT_STYLE_HOTPATCH_PROBE_SCRIPT
    .replace("__AFFECTED_URL__", &format!("http://127.0.0.1:{port}/"))
    .replace("__UNAFFECTED_URL__", &format!("http://127.0.0.1:{port}/about"))
}

fn component_template_hotpatch_probe_script(port: u16) -> String {
  COMPONENT_TEMPLATE_HOTPATCH_PROBE_SCRIPT
    .replace("__AFFECTED_URL__", &format!("http://127.0.0.1:{port}/"))
    .replace("__UNAFFECTED_URL__", &format!("http://127.0.0.1:{port}/about"))
}

fn noop_hotpatch_probe_script(port: u16) -> String {
  NOOP_HOTPATCH_PROBE_SCRIPT.replace("__TARGET_URL__", &format!("http://127.0.0.1:{port}/"))
}

fn shutdown_hotpatch_probe_script(port: u16) -> String {
  SHUTDOWN_HOTPATCH_PROBE_SCRIPT.replace("__TARGET_URL__", &format!("http://127.0.0.1:{port}/"))
}

fn workspace_root() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("../..")
    .canonicalize()
    .expect("workspace root should resolve")
}

fn playwright_probe_supported() -> bool {
  static PLAYWRIGHT_SUPPORTED: OnceLock<bool> = OnceLock::new();

  *PLAYWRIGHT_SUPPORTED.get_or_init(|| {
    let perf_dir = workspace_root().join("scripts/perf");
    if !perf_dir.join("node_modules/playwright").exists() {
      return false;
    }

    let output = Command::new("node")
      .current_dir(&perf_dir)
      .args([
        "-e",
        "const { chromium } = require('playwright'); process.stdout.write(chromium.executablePath());",
      ])
      .output();

    match output {
      Ok(output) if output.status.success() => {
        let executable_path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        !executable_path.is_empty() && Path::new(&executable_path).exists()
      }
      _ => false,
    }
  })
}

fn spawn_playwright_probe(script: &str, patch_file: &Path, patch_source: &str) -> Child {
  Command::new("node")
    .current_dir(workspace_root().join("scripts/perf"))
    .env("THEBE_HOTPATCH_FILE", patch_file)
    .env("THEBE_HOTPATCH_SOURCE", patch_source)
    .args(["-e", script])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("Playwright hotpatch probe should spawn")
}

fn assert_playwright_probe_success(
  output: &std::process::Output,
  hotpatch_output: &Arc<Mutex<Vec<String>>>,
  description: &str,
) {
  if output.status.success() {
    return;
  }

  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  panic!(
    "{description} failed\nstdout:\n{stdout}\nstderr:\n{stderr}\nhotpatch output:\n{}",
    collected_lines(hotpatch_output).join("\n")
  );
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
  spawn_child_output_collectors(child.child_mut(), output)
}

fn spawn_child_output_collectors(
  child: &mut Child,
  output: Arc<Mutex<Vec<String>>>,
) -> Vec<thread::JoinHandle<()>> {
  let mut handles = Vec::new();

  if let Some(stdout) = child.stdout.take() {
    handles.push(spawn_output_collector(stdout, Arc::clone(&output)));
  }

  if let Some(stderr) = child.stderr.take() {
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

fn wait_for_app_to_stop(port: u16, timeout: Duration) {
  let started = Instant::now();

  while started.elapsed() < timeout {
    if try_fetch_page(port, "/").is_none() {
      return;
    }

    thread::sleep(Duration::from_millis(100));
  }

  panic!("fixture app on port {port} still responded after {timeout:?}");
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
