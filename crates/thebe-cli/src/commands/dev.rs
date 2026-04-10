use anyhow::Context;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;
use walkdir::WalkDir;

/// Run `thebe dev`: parse all `.trs` route files, emit generated Rust sources,
/// and hand off to `cargo run`. Pass `watch = true` to auto-restart on changes.
pub fn run(watch: bool) -> anyhow::Result<()> {
  let project_root = find_project_root()?;
  println!("thebe: project root at {}", project_root.display());

  let src_dir = project_root.join("src");
  let routes_dir = src_dir.join("routes");
  anyhow::ensure!(
    routes_dir.exists(),
    "no `src/routes/` directory found — create your route `.trs` files there"
  );

  run_codegen(&project_root, &src_dir, &routes_dir)?;

  if watch {
    run_watch(&project_root, &src_dir, &routes_dir)
  } else {
    println!("thebe: running `cargo run`\u{2026}");
    let status = Command::new("cargo")
      .arg("run")
      .current_dir(&project_root)
      .status()
      .context("failed to invoke `cargo run`")?;
    std::process::exit(status.code().unwrap_or(1));
  }
}

/// Re-run codegen for every `.trs` file and write `__thebe_routes.rs`.
fn run_codegen(project_root: &Path, src_dir: &Path, routes_dir: &Path) -> anyhow::Result<()> {
  // Parse all `_layout.trs` files first so each route can find its wrapper.
  let layouts = collect_layouts(routes_dir)?;
  let app_html = load_app_html(project_root)?;

  let trs_files = collect_trs_files(routes_dir)?;
  anyhow::ensure!(
    !trs_files.is_empty(),
    "no `.trs` files found in `src/routes/`"
  );

  let mut route_entries: Vec<thebe_codegen::RouteEntry> = Vec::new();

  for trs_path in &trs_files {
    let source = std::fs::read_to_string(trs_path)
      .with_context(|| format!("failed to read {}", trs_path.display()))?;

    let blocks = thebe_parser::parse_sfc(&source)
      .with_context(|| format!("parse error in {}", trs_path.display()))?;

    let route_path = file_to_route_path(trs_path, routes_dir);
    let mod_name = file_to_mod_name(trs_path, routes_dir);

    // Find the nearest `_layout.trs` ancestor for this route.
    let layout_arg = find_layout(&layouts, trs_path, routes_dir)
      .map(|(blocks, scope_path)| (blocks, scope_path.as_str()));

    let generated = thebe_codegen::generate_route(&blocks, &route_path, layout_arg, &app_html)
      .with_context(|| format!("codegen error for {}", trs_path.display()))?;

    let rs_path = trs_path.with_extension("rs");
    std::fs::write(&rs_path, &generated)
      .with_context(|| format!("failed to write {}", rs_path.display()))?;

    println!(
      "thebe: {} \u{2192} {}",
      trs_path.display(),
      rs_path.display()
    );

    let source_path = rs_path
      .strip_prefix(src_dir)
      .with_context(|| format!("generated route {} is outside src/", rs_path.display()))?
      .to_string_lossy()
      .replace('\\', "/");

    route_entries.push(thebe_codegen::RouteEntry {
      mod_name,
      source_path,
    });
  }

  let routes_file = thebe_codegen::generate_routes_file(&route_entries);
  let routes_path = project_root.join("src").join("__thebe_routes.rs");
  std::fs::write(&routes_path, &routes_file).context("failed to write src/__thebe_routes.rs")?;
  println!("thebe: generated src/__thebe_routes.rs");

  Ok(())
}

fn load_app_html(project_root: &Path) -> anyhow::Result<String> {
  let app_html_path = project_root.join("app.html");

  if app_html_path.exists() {
    let app_html = std::fs::read_to_string(&app_html_path)
      .with_context(|| format!("failed to read {}", app_html_path.display()))?;
    thebe_codegen::validate_app_html(&app_html)
      .with_context(|| format!("invalid {}", app_html_path.display()))?;
    return Ok(app_html);
  }

  let app_html = thebe_codegen::default_app_html().to_owned();
  thebe_codegen::validate_app_html(&app_html).context("internal default app.html is invalid")?;
  Ok(app_html)
}

/// Spawn `cargo run` in `project_root` as a background child process.
fn spawn_server(project_root: &Path) -> anyhow::Result<Child> {
  println!("thebe: running `cargo run`\u{2026}");
  Command::new("cargo")
    .arg("run")
    .current_dir(project_root)
    .spawn()
    .context("failed to spawn `cargo run`")
}

/// Terminate a server child and all processes it spawned.
///
/// On Unix we use `pkill -P <pid>` to kill the Axum binary that cargo spawned,
/// then kill cargo itself.  On other platforms we can only kill cargo directly.
fn kill_server(child: &mut Child) {
  #[cfg(unix)]
  {
    let _ = Command::new("pkill")
      .args(["-TERM", "-P", &child.id().to_string()])
      .status();
    // Give child processes a moment to exit cleanly.
    std::thread::sleep(Duration::from_millis(200));
  }
  let _ = child.kill();
  let _ = child.wait();
}

/// Watch `routes_dir` for `.trs` changes and restart the server on each one.
fn run_watch(project_root: &Path, src_dir: &Path, routes_dir: &Path) -> anyhow::Result<()> {
  use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
  use std::sync::mpsc;

  println!("thebe: watch mode — watching {} for changes\u{2026}", routes_dir.display());

  let mut child = spawn_server(project_root)?;

  let (tx, rx) = mpsc::channel();
  let mut watcher = RecommendedWatcher::new(
    move |res| {
      let _ = tx.send(res);
    },
    Config::default(),
  )
  .context("failed to create file watcher")?;

  watcher
    .watch(routes_dir, RecursiveMode::Recursive)
    .context("failed to watch routes directory")?;
  watcher
    .watch(project_root, RecursiveMode::NonRecursive)
    .context("failed to watch project root")?;

  while let Ok(first) = rx.recv() {
    // Only act on events that touch route `.trs` files or `app.html`.
    let codegen_changed = is_codegen_event(&first, project_root);

    // Drain any rapid follow-up events (debounce over 150 ms).
    let mut any_codegen_change = codegen_changed;
    loop {
      match rx.recv_timeout(Duration::from_millis(150)) {
        Ok(res) => {
          if is_codegen_event(&res, project_root) {
            any_codegen_change = true;
          }
        }
        Err(mpsc::RecvTimeoutError::Timeout) => break,
        Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
      }
    }

    if !any_codegen_change {
      continue;
    }

    println!("thebe: change detected — rebuilding\u{2026}");

    kill_server(&mut child);

    match run_codegen(project_root, src_dir, routes_dir) {
      Err(e) => eprintln!("thebe: codegen error: {e:#}"),
      Ok(()) => match spawn_server(project_root) {
        Ok(new_child) => child = new_child,
        Err(e) => eprintln!("thebe: failed to restart server: {e:#}"),
      },
    }
  }

  Ok(())
}

/// Return `true` if a notify result should trigger route code generation.
fn is_codegen_event(res: &notify::Result<notify::Event>, project_root: &Path) -> bool {
  use notify::event::EventKind;
  match res {
    Ok(event) => {
      matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
      ) && event.paths.iter().any(|path| {
        path.extension().is_some_and(|ext| ext == "trs") || is_app_html_path(path, project_root)
      })
    }
    Err(_) => false,
  }
}

fn is_app_html_path(path: &Path, project_root: &Path) -> bool {
  path
    .file_name()
    .is_some_and(|file_name| file_name == "app.html")
    && path.parent().is_some_and(|parent| parent == project_root)
}

/// Walk up from the current directory to find the nearest `Cargo.toml`.
fn find_project_root() -> anyhow::Result<PathBuf> {
  let mut dir = std::env::current_dir().context("failed to get current directory")?;
  loop {
    if dir.join("Cargo.toml").exists() {
      return Ok(dir);
    }
    match dir.parent() {
      Some(parent) => dir = parent.to_path_buf(),
      None => anyhow::bail!("could not find a `Cargo.toml` in the current directory or any parent"),
    }
  }
}

/// Recursively collect `.trs` route files from `dir`, **excluding** any file
/// whose stem is `_layout` (those are collected separately by [`collect_layouts`]).
fn collect_trs_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
  let mut files = Vec::new();
  for entry in WalkDir::new(dir).min_depth(1) {
    let entry = entry.context("failed to read directory entry")?;
    if entry.file_type().is_file() && entry.path().extension().is_some_and(|e| e == "trs") {
      let stem = entry
        .path()
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
      if stem != "_layout" {
        files.push(entry.into_path());
      }
    }
  }
  files.sort();
  Ok(files)
}

/// Walk `routes_dir` and parse every `_layout.trs` file found.
///
/// Returns a map from **directory path** → (parsed `SfcBlocks`, scope path
/// string).  The scope path is the layout file's path relative to `routes_dir`
/// (without the `.trs` extension), used as the CSS scope identifier.
fn collect_layouts(
  routes_dir: &Path,
) -> anyhow::Result<HashMap<PathBuf, (thebe_parser::SfcBlocks, String)>> {
  let mut map = HashMap::new();
  for entry in WalkDir::new(routes_dir).min_depth(1) {
    let entry = entry.context("failed to read directory entry")?;
    if entry.file_type().is_file()
      && entry.path().extension().is_some_and(|e| e == "trs")
      && entry
        .path()
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        == "_layout"
    {
      let layout_path = entry.into_path();
      let source = std::fs::read_to_string(&layout_path)
        .with_context(|| format!("failed to read {}", layout_path.display()))?;
      let blocks = thebe_parser::parse_sfc(&source)
        .with_context(|| format!("parse error in {}", layout_path.display()))?;

      // Derive a stable scope path: relative to routes_dir, no extension.
      let rel = layout_path
        .strip_prefix(routes_dir)
        .unwrap_or(&layout_path);
      let scope_path = rel
        .with_extension("")
        .to_string_lossy()
        .replace('\\', "/");

      // Key by the directory that contains the layout file.
      let dir = layout_path
        .parent()
        .unwrap_or(routes_dir)
        .to_path_buf();
      map.insert(dir, (blocks, scope_path));

      println!("thebe: found layout {}", layout_path.display());
    }
  }
  Ok(map)
}

/// Find the nearest `_layout.trs` that applies to `route_file`.
///
/// Walks up from the route file's directory towards (and including) `routes_dir`
/// and returns a reference to the first layout found, or `None`.
fn find_layout<'a>(
  layouts: &'a HashMap<PathBuf, (thebe_parser::SfcBlocks, String)>,
  route_file: &Path,
  routes_dir: &Path,
) -> Option<(&'a thebe_parser::SfcBlocks, &'a String)> {
  let mut dir = route_file.parent().unwrap_or(routes_dir).to_path_buf();
  loop {
    if let Some((blocks, scope)) = layouts.get(&dir) {
      return Some((blocks, scope));
    }
    // Stop after checking routes_dir itself.
    if dir == routes_dir {
      break;
    }
    match dir.parent() {
      Some(parent) => dir = parent.to_path_buf(),
      None => break,
    }
  }
  None
}

/// Derive the Axum route path from the file's position under `routes_dir`.
///
/// * `src/routes/index.trs` → `/`
/// * `src/routes/about.trs` → `/about`
/// * `src/routes/blog/[slug].trs` → `/blog/:slug`
fn file_to_route_path(trs_path: &Path, routes_dir: &Path) -> String {
  let rel = trs_path.strip_prefix(routes_dir).unwrap_or(trs_path);
  let stem = rel.file_stem().unwrap_or_default().to_string_lossy();
  let parent = rel.parent().unwrap_or(Path::new(""));

  let mut segments: Vec<String> = parent
    .components()
    .map(|c| c.as_os_str().to_string_lossy().into_owned())
    .collect();

  if stem != "index" {
    segments.push(stem.into_owned());
  }

  if segments.is_empty() {
    "/".to_owned()
  } else {
    let path = segments.join("/");
    // Convert file-system dynamic segments `[param]` → `:param`.
    let path = path.replace('[', ":").replace(']', "");
    format!("/{path}")
  }
}

/// Derive the Rust module name from the `.trs` filename stem.
fn file_to_mod_name(trs_path: &Path, routes_dir: &Path) -> String {
  let rel = trs_path.strip_prefix(routes_dir).unwrap_or(trs_path);

  let mut parts: Vec<String> = rel
    .components()
    .map(|component| component.as_os_str().to_string_lossy().into_owned())
    .collect();
  if let Some(last) = parts.last_mut() {
    let stem = Path::new(last)
      .file_stem()
      .unwrap_or_default()
      .to_string_lossy()
      .into_owned();
    *last = stem;
  }

  let module = parts
    .iter()
    .map(|part| sanitize_module_segment(part))
    .collect::<Vec<_>>()
    .join("__");

  format!("route__{module}")
}

fn sanitize_module_segment(segment: &str) -> String {
  let raw = if let Some(dynamic) = segment
    .strip_prefix('[')
    .and_then(|value| value.strip_suffix(']'))
  {
    format!("dyn_{dynamic}")
  } else {
    segment.to_owned()
  };

  let mut out = String::new();
  let mut prev_was_underscore = false;
  for ch in raw.chars() {
    let mapped = if ch.is_ascii_alphanumeric() {
      prev_was_underscore = false;
      ch.to_ascii_lowercase()
    } else {
      if prev_was_underscore {
        continue;
      }
      prev_was_underscore = true;
      '_'
    };
    out.push(mapped);
  }

  while out.ends_with('_') {
    out.pop();
  }
  if out.is_empty() {
    out.push_str("route");
  }
  if out.starts_with(|c: char| c.is_ascii_digit()) {
    out.insert(0, '_');
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn file_to_route_path_maps_nested_and_dynamic_routes() {
    let routes_dir = Path::new("/tmp/app/src/routes");
    let path = Path::new("/tmp/app/src/routes/blog/[slug].trs");
    assert_eq!(file_to_route_path(path, routes_dir), "/blog/:slug");
  }

  #[test]
  fn file_to_mod_name_uses_relative_path_segments() {
    let routes_dir = Path::new("/tmp/app/src/routes");
    let path = Path::new("/tmp/app/src/routes/blog/[slug].trs");
    assert_eq!(file_to_mod_name(path, routes_dir), "route__blog__dyn_slug");
  }

  #[test]
  fn sanitize_module_segment_normalizes_static_segments() {
    assert_eq!(sanitize_module_segment("My-page"), "my_page");
  }
}
