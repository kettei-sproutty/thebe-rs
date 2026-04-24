use crate::commands::dev_command::{
  kill_server, rebuild_for_dev_change, spawn_hotpatch_server,
};
use crate::hotpatch::browser::BrowserPatchServer;
use crate::hotpatch::classify::{
  HotpatchAction, RestartReason, SourceSnapshots, TrsPatchKind, classify_path,
  classify_paths_with_snapshots, trs_patch_kind_with_snapshots,
};
use crate::hotpatch::runtime::PatchServer;
use crate::hotpatch::session::HotpatchSession;
use anyhow::Context;
use notify::event::{DataChange, EventKind, ModifyKind, RenameMode};
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};
use std::time::{Duration, Instant};

/// Run the experimental hotpatch dev loop.
pub(crate) fn run(project_root: &Path) -> anyhow::Result<()> {
  let (listener, server_addr) = PatchServer::bind()
    .context("failed to bind the local hotpatch server")?;
  let browser_server = BrowserPatchServer::spawn()
    .context("failed to bind the browser patch server")?;
  let session = HotpatchSession::initialize(project_root, server_addr, Some(browser_server.local_addr()))
    .context("failed to initialize hotpatch session")?;
  let patch_server = PatchServer::spawn(listener, session.manifest.clone())
    .context("failed to start the local hotpatch server")?;
  println!(
    "thebe: hotpatch session {} ready ({})",
    session.manifest.session_id,
    session.paths.manifest_path.display()
  );
  println!(
    "thebe: hotpatch patch server listening on {}",
    patch_server.local_addr()
  );
  println!(
    "thebe: hotpatch browser channel listening on http://{}",
    browser_server.local_addr()
  );
  println!(
    "thebe: hotpatch mode — template/style .trs changes patch in place; script and Rust changes restart"
  );

  let src_dir = project_root.join("src");
  let session_manifest_path = session.paths.manifest_path.clone();
  let mut source_snapshots = SourceSnapshots::load(project_root)
    .context("failed to load hotpatch source snapshots")?;
  let mut child = spawn_hotpatch_server(project_root, &session_manifest_path)?;
  let shutdown_requested = Arc::new(AtomicBool::new(false));
  let shutdown_requested_for_handler = Arc::clone(&shutdown_requested);

  ctrlc::set_handler(move || {
    shutdown_requested_for_handler.store(true, Ordering::SeqCst);
  })
  .context("failed to install hotpatch shutdown handler")?;

  let (tx, rx) = mpsc::channel();
  let mut watcher = RecommendedWatcher::new(
    move |res| {
      let _ = tx.send(res);
    },
    Config::default(),
  )
  .context("failed to create file watcher")?;

  watcher
    .watch(&src_dir, RecursiveMode::Recursive)
    .with_context(|| format!("failed to watch {}", src_dir.display()))?;
  watcher
    .watch(project_root, RecursiveMode::NonRecursive)
    .with_context(|| format!("failed to watch {}", project_root.display()))?;

  loop {
    if shutdown_requested.load(Ordering::SeqCst) {
      break;
    }

    let first = match rx.recv_timeout(Duration::from_millis(100)) {
      Ok(first) => first,
      Err(mpsc::RecvTimeoutError::Timeout) => continue,
      Err(mpsc::RecvTimeoutError::Disconnected) => break,
    };

    let mut changed_paths = collect_changed_paths(&first);
    loop {
      match rx.recv_timeout(Duration::from_millis(150)) {
        Ok(res) => extend_changed_paths(&mut changed_paths, &res),
        Err(mpsc::RecvTimeoutError::Timeout) => break,
        Err(mpsc::RecvTimeoutError::Disconnected) => {
          stop_runtime(&patch_server, &mut child);
          return Ok(());
        }
      }
    }

    let changed_paths: Vec<_> = changed_paths.into_iter().collect();
  let change_plan = plan_change_batch_with_snapshots(project_root, &source_snapshots, &changed_paths);

    match change_plan {
      ChangePlan::Ignore => {}
      ChangePlan::Patch(patch_plan) => {
        println!("thebe: {}", patch_plan.message());

        if let Err(err) = rebuild_for_dev_change(project_root, patch_plan.refresh_codegen) {
          eprintln!("thebe: failed to refresh dev state: {err:#}");
          browser_server.broadcast_reload();
          continue;
        }

        if let Err(err) = source_snapshots.refresh(project_root) {
          eprintln!("thebe: failed to refresh hotpatch source snapshots: {err}");
          browser_server.broadcast_reload();
          continue;
        }

        if let Err(err) = session.ensure_persisted() {
          eprintln!("thebe: failed to restore hotpatch session: {err}");
          browser_server.broadcast_reload();
          continue;
        }

        if let Err(err) = deliver_runtime_patches(project_root, &patch_server) {
          eprintln!("thebe: failed to deliver runtime patch: {err:#}");
          browser_server.broadcast_reload();
          continue;
        }

        if let Err(err) = dispatch_browser_patch(project_root, &browser_server, &patch_plan) {
          eprintln!("thebe: failed to notify browsers: {err:#}");
          browser_server.broadcast_reload();
        }
      }
      ChangePlan::Restart(restart_plan) => {
        println!("thebe: {}", restart_plan.trigger.message());
        shutdown_runtime(&patch_server, &mut child, restart_plan.trigger.protocol_reason());

        if let Err(err) = rebuild_for_dev_change(project_root, restart_plan.refresh_codegen) {
          eprintln!("thebe: failed to refresh dev state: {err:#}");
        } else {
          if let Err(err) = source_snapshots.refresh(project_root) {
            eprintln!("thebe: failed to refresh hotpatch source snapshots: {err}");
            continue;
          }

          if let Err(err) = session.ensure_persisted() {
            eprintln!("thebe: failed to restore hotpatch session: {err}");
            continue;
          }

          match spawn_hotpatch_server(project_root, &session_manifest_path) {
            Ok(new_child) => child = new_child,
            Err(err) => eprintln!("thebe: failed to restart server: {err:#}"),
          }
        }
      }
    }
  }

  stop_runtime(&patch_server, &mut child);

  Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ChangePlan {
  Ignore,
  Patch(PatchPlan),
  Restart(RestartPlan),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PatchPlan {
  browser_patch: BrowserPatch,
  refresh_codegen: bool,
}

impl PatchPlan {
  fn message(&self) -> &'static str {
    match self.browser_patch {
      BrowserPatch::StyleRoutes(_) => "applying CSS hotpatch",
      BrowserPatch::TemplateRoutes(_) | BrowserPatch::TemplateGlobal => {
        "applying template hotpatch"
      }
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BrowserPatch {
  StyleRoutes(Vec<PathBuf>),
  TemplateRoutes(Vec<PathBuf>),
  TemplateGlobal,
}

#[derive(Debug, Deserialize)]
struct DevRouteArtifactFile {
  style: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RestartPlan {
  refresh_codegen: bool,
  trigger: RestartTrigger,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RestartTrigger {
  PatchCandidate,
  RestartRequired(RestartReason),
}

impl RestartTrigger {
  fn message(&self) -> String {
    match self {
      Self::PatchCandidate => {
        String::from("hotpatch candidate detected — runtime patch delivery is not wired yet, restarting")
      }
      Self::RestartRequired(reason) => {
        format!("restart required — {}", reason.describe())
      }
    }
  }

  fn protocol_reason(&self) -> &'static str {
    match self {
      Self::PatchCandidate => {
        "patch candidate requires restart until runtime patch delivery is wired"
      }
      Self::RestartRequired(reason) => reason.describe(),
    }
  }
}

#[cfg_attr(not(test), expect(dead_code, reason = "retained as a disk-backed wrapper for focused tests"))]
fn plan_change_batch(project_root: &Path, changed_paths: &[PathBuf]) -> ChangePlan {
  let Ok(source_snapshots) = SourceSnapshots::load(project_root) else {
    return ChangePlan::Restart(RestartPlan {
      refresh_codegen: should_refresh_codegen(project_root, changed_paths),
      trigger: RestartTrigger::RestartRequired(RestartReason::GeneratedInput),
    });
  };

  plan_change_batch_with_snapshots(project_root, &source_snapshots, changed_paths)
}

fn plan_change_batch_with_snapshots(
  project_root: &Path,
  source_snapshots: &SourceSnapshots,
  changed_paths: &[PathBuf],
) -> ChangePlan {
  match classify_paths_with_snapshots(project_root, source_snapshots, changed_paths) {
    HotpatchAction::Ignore => ChangePlan::Ignore,
    HotpatchAction::AttemptPatch => plan_patch_batch(project_root, source_snapshots, changed_paths).map_or_else(
      || {
        ChangePlan::Restart(RestartPlan {
          refresh_codegen: should_refresh_codegen(project_root, changed_paths),
          trigger: RestartTrigger::PatchCandidate,
        })
      },
      ChangePlan::Patch,
    ),
    HotpatchAction::Restart(reason) => ChangePlan::Restart(RestartPlan {
      refresh_codegen: should_refresh_codegen(project_root, changed_paths),
      trigger: RestartTrigger::RestartRequired(reason),
    }),
  }
}

fn plan_patch_batch(
  project_root: &Path,
  source_snapshots: &SourceSnapshots,
  changed_paths: &[PathBuf],
) -> Option<PatchPlan> {
  if changed_paths.is_empty() {
    return None;
  }

  let routes_dir = project_root.join("src").join("routes");
  let mut style_only_routes = Vec::new();
  let mut template_routes = Vec::new();
  let mut requires_global_template = false;
  let mut saw_patchable_trs = false;

  for path in changed_paths {
    match classify_path(project_root, source_snapshots, path) {
      crate::hotpatch::classify::PathAction::Ignore => continue,
      crate::hotpatch::classify::PathAction::Restart(_) => return None,
      crate::hotpatch::classify::PathAction::AttemptPatch => {}
    }

    if path.extension().is_none_or(|ext| ext != "trs") {
      return None;
    }

    let patch_kind = trs_patch_kind_with_snapshots(project_root, source_snapshots, path)?;
    let is_route = path.starts_with(&routes_dir);
    saw_patchable_trs = true;

    match (patch_kind, is_route) {
      (TrsPatchKind::StyleOnly, true) => style_only_routes.push(path.clone()),
      (TrsPatchKind::TemplateLike, true) => template_routes.push(path.clone()),
      (_, false) => requires_global_template = true,
    }
  }

  if !saw_patchable_trs {
    return None;
  }

  let browser_patch = if requires_global_template {
    BrowserPatch::TemplateGlobal
  } else if !template_routes.is_empty() {
    BrowserPatch::TemplateRoutes(template_routes)
  } else {
    BrowserPatch::StyleRoutes(style_only_routes)
  };

  Some(PatchPlan {
    browser_patch,
    refresh_codegen: true,
  })
}

fn should_refresh_codegen(project_root: &Path, changed_paths: &[PathBuf]) -> bool {
  changed_paths.iter().any(|path| {
    path.extension().is_some_and(|extension| extension == "trs")
      || is_project_root_file(path, project_root, "app.html")
      || is_project_root_file(path, project_root, "thebe.toml")
  })
}

fn deliver_runtime_patches(project_root: &Path, patch_server: &PatchServer) -> anyhow::Result<()> {
  let manifest = thebe_project::load_manifest(project_root)?;
  let build_id = patch_build_id();

  for route in manifest.routes {
    let artifact_path = thebe_codegen::dev_route_artifact_path(&route.route_path);
    let contents = fs::read_to_string(project_root.join(&artifact_path))
      .with_context(|| format!("failed to read {}", project_root.join(&artifact_path).display()))?;
    let payload = thebe_runtime::hotpatch::encode_patch_payload(
      &thebe_runtime::hotpatch::RuntimePatchPayload::TextArtifact {
        path: artifact_path.clone(),
        contents,
      },
    )
    .context("failed to encode runtime patch payload")?;
    patch_server
      .request_patch(&build_id, payload)
      .with_context(|| format!("failed to apply runtime patch for {artifact_path}"))?;
  }

  Ok(())
}

fn dispatch_browser_patch(
  project_root: &Path,
  browser_server: &BrowserPatchServer,
  patch_plan: &PatchPlan,
) -> anyhow::Result<()> {
  let manifest = thebe_project::load_manifest(project_root)?;

  match &patch_plan.browser_patch {
    BrowserPatch::StyleRoutes(route_paths) => {
      for changed_path in route_paths {
        let Some(route) = find_route_metadata(project_root, &manifest, changed_path) else {
          browser_server.broadcast_template(None);
          return Ok(());
        };
        let artifact_path = thebe_codegen::dev_route_artifact_path(&route.route_path);
        let artifact_source = fs::read_to_string(project_root.join(&artifact_path)).with_context(|| {
          format!("failed to read {}", project_root.join(&artifact_path).display())
        })?;
        let artifact: DevRouteArtifactFile = serde_json::from_str(&artifact_source)
          .context("failed to parse dev route artifact")?;
        browser_server.broadcast_style(Some(&route.route_path), artifact.style);
      }
    }
    BrowserPatch::TemplateRoutes(route_paths) => {
      for changed_path in route_paths {
        let Some(route) = find_route_metadata(project_root, &manifest, changed_path) else {
          browser_server.broadcast_template(None);
          return Ok(());
        };
        browser_server.broadcast_template(Some(&route.route_path));
      }
    }
    BrowserPatch::TemplateGlobal => browser_server.broadcast_template(None),
  }

  Ok(())
}

fn find_route_metadata<'a>(
  project_root: &Path,
  manifest: &'a thebe_project::ThebeManifest,
  changed_path: &Path,
) -> Option<&'a thebe_project::RouteMetadata> {
  manifest.routes.iter().find(|route| project_root.join(&route.source_path) == changed_path)
}

fn patch_build_id() -> String {
  let unix_nanos = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_nanos();
  format!("patch-{:x}-{:x}", std::process::id(), unix_nanos)
}

fn collect_changed_paths(res: &notify::Result<Event>) -> BTreeSet<PathBuf> {
  let mut changed_paths = BTreeSet::new();
  extend_changed_paths(&mut changed_paths, res);
  changed_paths
}

fn extend_changed_paths(changed_paths: &mut BTreeSet<PathBuf>, res: &notify::Result<Event>) {
  let Ok(event) = res else {
    return;
  };

  if !is_rebuild_event_kind(event.kind) {
    return;
  }

  changed_paths.extend(event.paths.iter().cloned());
}

fn is_rebuild_event_kind(kind: EventKind) -> bool {
  matches!(
    kind,
    EventKind::Create(_)
      | EventKind::Remove(_)
      | EventKind::Modify(ModifyKind::Any)
      | EventKind::Modify(ModifyKind::Data(DataChange::Any | DataChange::Size | DataChange::Content | DataChange::Other))
      | EventKind::Modify(ModifyKind::Name(RenameMode::Any | RenameMode::To | RenameMode::From | RenameMode::Both | RenameMode::Other))
      | EventKind::Modify(ModifyKind::Other)
  )
}

fn is_project_root_file(path: &Path, project_root: &Path, file_name: &str) -> bool {
  path.file_name().is_some_and(|name| name == file_name)
    && path.parent().is_some_and(|parent| parent == project_root)
}

fn shutdown_runtime(patch_server: &PatchServer, child: &mut Child, reason: &str) {
  let runtime_process_id = patch_server.active_process_id();
  patch_server.request_restart(reason);

  let runtime_exited = wait_for_runtime_exit(runtime_process_id, Duration::from_millis(300));
  let child_exited = wait_for_child_exit(child, Duration::from_millis(300));

  if !runtime_exited {
    terminate_runtime_process(runtime_process_id);
    let _ = wait_for_runtime_exit(runtime_process_id, Duration::from_secs(2));
  }

  if !child_exited {
    kill_server(child);
  }

  std::thread::sleep(Duration::from_millis(100));
}

fn stop_runtime(patch_server: &PatchServer, child: &mut Child) {
  let runtime_process_id = patch_server.active_process_id();
  patch_server.request_shutdown();

  let runtime_exited = wait_for_runtime_exit(runtime_process_id, Duration::from_millis(300));
  let child_exited = wait_for_child_exit(child, Duration::from_millis(300));

  if !runtime_exited {
    terminate_runtime_process(runtime_process_id);
    let _ = wait_for_runtime_exit(runtime_process_id, Duration::from_secs(2));
  }

  if !child_exited {
    kill_server(child);
  }
}

fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> bool {
  let start = Instant::now();

  while start.elapsed() < timeout {
    match child.try_wait() {
      Ok(Some(_status)) => return true,
      Ok(None) => std::thread::sleep(Duration::from_millis(25)),
      Err(_error) => break,
    }
  }

  false
}

fn wait_for_runtime_exit(process_id: Option<u32>, timeout: Duration) -> bool {
  let Some(process_id) = process_id else {
    return true;
  };

  let start = Instant::now();
  while start.elapsed() < timeout {
    if !process_exists(process_id) {
      return true;
    }

    std::thread::sleep(Duration::from_millis(25));
  }

  !process_exists(process_id)
}

#[cfg(unix)]
fn process_exists(process_id: u32) -> bool {
  std::process::Command::new("kill")
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .args(["-0", &process_id.to_string()])
    .status()
    .is_ok_and(|status| status.success())
}

#[cfg(not(unix))]
fn process_exists(_process_id: u32) -> bool {
  false
}

#[cfg(unix)]
fn terminate_runtime_process(process_id: Option<u32>) {
  let Some(process_id) = process_id else {
    return;
  };

  let _ = std::process::Command::new("kill")
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .args(["-TERM", &process_id.to_string()])
    .status();
}

#[cfg(not(unix))]
fn terminate_runtime_process(_process_id: Option<u32>) {}

#[cfg(test)]
mod tests {
  use super::{
    BrowserPatch, ChangePlan, PatchPlan, RestartPlan, RestartTrigger, is_rebuild_event_kind,
    plan_change_batch, wait_for_runtime_exit,
  };
  use crate::hotpatch::classify::RestartReason;
  use std::fs;
  use notify::event::{CreateKind, DataChange, EventKind, MetadataKind, ModifyKind};
  use std::path::{Path, PathBuf};
  use std::process::Command;
  use std::process;
  use std::time::Duration;
  use std::time::{SystemTime, UNIX_EPOCH};

  #[test]
  fn plan_change_batch_should_restart_with_codegen_for_thebe_inputs() {
    let project_root = Path::new("/tmp/project");
    let changed_paths = vec![project_root.join("src/routes/index.trs")];

    let plan = plan_change_batch(project_root, &changed_paths);

    assert_eq!(
      plan,
      ChangePlan::Restart(RestartPlan {
        refresh_codegen: true,
        trigger: RestartTrigger::RestartRequired(RestartReason::GeneratedInput),
      })
    );
  }

  #[test]
  fn plan_change_batch_should_restart_without_codegen_for_rust_source_changes() {
    let project_root = Path::new("/tmp/project");
    let changed_paths = vec![project_root.join("src/state.rs")];

    let plan = plan_change_batch(project_root, &changed_paths);

    assert_eq!(
      plan,
      ChangePlan::Restart(RestartPlan {
        refresh_codegen: false,
        trigger: RestartTrigger::RestartRequired(RestartReason::RustSource),
      })
    );
  }

  #[test]
  fn plan_change_batch_should_patch_style_only_route_changes() {
    let project_root = temp_project_root("patch-style-route");
    let route_path = project_root.join("src/routes/index.trs");

    write_snapshot(
      &project_root,
      &route_path,
      "<script setup>fn index() -> Props { Props {} }</script>\n<style>.card { color: red; }</style>\n<div>hello</div>",
    );
    fs::write(
      &route_path,
      "<script setup>fn index() -> Props { Props {} }</script>\n<style>.card { color: blue; }</style>\n<div>hello</div>",
    )
    .expect("current route should write");

    let plan = plan_change_batch(&project_root, &[route_path.clone()]);

    assert_eq!(
      plan,
      ChangePlan::Patch(PatchPlan {
        browser_patch: BrowserPatch::StyleRoutes(vec![route_path]),
        refresh_codegen: true,
      })
    );

    let _ = fs::remove_dir_all(project_root);
  }

  #[test]
  fn plan_change_batch_should_patch_template_like_route_changes() {
    let project_root = temp_project_root("patch-template-route");
    let route_path = project_root.join("src/routes/index.trs");

    write_snapshot(
      &project_root,
      &route_path,
      "<script setup>fn index() -> Props { Props {} }</script>\n<div>before</div>",
    );
    fs::write(
      &route_path,
      "<script setup>fn index() -> Props { Props {} }</script>\n<div>after</div>",
    )
    .expect("current route should write");

    let plan = plan_change_batch(&project_root, &[route_path.clone()]);

    assert_eq!(
      plan,
      ChangePlan::Patch(PatchPlan {
        browser_patch: BrowserPatch::TemplateRoutes(vec![route_path]),
        refresh_codegen: true,
      })
    );

    let _ = fs::remove_dir_all(project_root);
  }

  #[test]
  fn plan_change_batch_should_ignore_non_rebuild_inputs() {
    let project_root = Path::new("/tmp/project");
    let changed_paths = vec![PathBuf::from("/tmp/project/public/logo.png")];

    let plan = plan_change_batch(project_root, &changed_paths);

    assert_eq!(plan, ChangePlan::Ignore);
  }

  #[test]
  fn is_rebuild_event_kind_should_ignore_metadata_only_changes() {
    assert!(is_rebuild_event_kind(EventKind::Modify(ModifyKind::Data(DataChange::Content))));
    assert!(is_rebuild_event_kind(EventKind::Create(CreateKind::File)));
    assert!(!is_rebuild_event_kind(EventKind::Modify(ModifyKind::Metadata(MetadataKind::WriteTime))));
  }

  #[cfg(unix)]
  #[test]
  fn wait_for_runtime_exit_should_observe_process_shutdown() {
    let mut child = Command::new("sh")
      .args(["-c", "sleep 5"])
      .spawn()
      .expect("sleep process should spawn");

    assert!(!wait_for_runtime_exit(Some(child.id()), Duration::from_millis(75)));

    child.kill().expect("sleep process should accept kill");
    child.wait().expect("sleep process should exit after kill");

    assert!(wait_for_runtime_exit(Some(child.id()), Duration::from_secs(1)));
  }

  fn temp_project_root(test_name: &str) -> PathBuf {
    let unique_suffix = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap_or_default()
      .as_nanos();
    let project_root = std::env::temp_dir().join(format!(
      "thebe-orchestrator-{test_name}-{}-{unique_suffix}",
      process::id()
    ));
    fs::create_dir_all(project_root.join("src/routes")).expect("routes dir should create");
    project_root
  }

  fn write_snapshot(project_root: &Path, source_path: &Path, source: &str) {
    let relative_path = source_path
      .strip_prefix(project_root)
      .expect("source should be inside project root");
    let snapshot_path = project_root
      .join(thebe_project::THEBE_SOURCE_SNAPSHOTS_DIR)
      .join(relative_path);
    if let Some(parent) = snapshot_path.parent() {
      fs::create_dir_all(parent).expect("snapshot parent should create");
    }
    fs::write(&snapshot_path, source).expect("snapshot should write");
  }
}
