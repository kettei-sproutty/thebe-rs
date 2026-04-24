use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use thebe_parser::{SfcBlocks, parse_component_sfc, parse_sfc};

/// Conservative action chosen for a batch of changed paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HotpatchAction {
  Ignore,
  AttemptPatch,
  Restart(RestartReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrsPatchKind {
  StyleOnly,
  TemplateLike,
}

/// Restart causes that should be surfaced clearly once hotpatch mode is wired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RestartReason {
  DependencyGraph,
  ThebeConfig,
  HtmlShell,
  GeneratedInput,
  RustSource,
  EntryPoint,
  ExternalRust,
}

impl RestartReason {
  #[must_use]
  pub(crate) fn describe(&self) -> &'static str {
    match self {
      Self::DependencyGraph => "dependency graph changed",
      Self::ThebeConfig => "thebe.toml changed",
      Self::HtmlShell => "app.html changed",
      Self::GeneratedInput => "Thebe-generated input changed",
      Self::RustSource => "Rust source changed",
      Self::EntryPoint => "application entry point changed",
      Self::ExternalRust => "Rust source outside src/ changed",
    }
  }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SourceSnapshots {
  sources: HashMap<PathBuf, String>,
}

impl SourceSnapshots {
  pub(crate) fn load(project_root: &Path) -> io::Result<Self> {
    let mut snapshots = Self::default();
    snapshots.refresh(project_root)?;
    Ok(snapshots)
  }

  pub(crate) fn refresh(&mut self, project_root: &Path) -> io::Result<()> {
    self.sources.clear();

    let snapshots_dir = project_root.join(thebe_project::THEBE_SOURCE_SNAPSHOTS_DIR);
    if !snapshots_dir.exists() {
      return Ok(());
    }

    collect_snapshot_sources(project_root, &snapshots_dir, &snapshots_dir, &mut self.sources)
  }

  fn source_for(&self, path: &Path) -> Option<&str> {
    self.sources.get(path).map(String::as_str)
  }
}

pub(crate) enum PathAction {
  Ignore,
  AttemptPatch,
  Restart(RestartReason),
}

/// Classify a filesystem event batch into an initial patch-vs-restart decision.
#[cfg_attr(not(test), expect(dead_code, reason = "retained as a disk-backed wrapper for focused tests"))]
pub(crate) fn classify_paths(project_root: &Path, paths: &[PathBuf]) -> HotpatchAction {
  let Ok(snapshots) = SourceSnapshots::load(project_root) else {
    return HotpatchAction::Restart(RestartReason::GeneratedInput);
  };

  classify_paths_with_snapshots(project_root, &snapshots, paths)
}

pub(crate) fn classify_paths_with_snapshots(
  project_root: &Path,
  snapshots: &SourceSnapshots,
  paths: &[PathBuf],
) -> HotpatchAction {
  let mut saw_patch_candidate = false;

  for path in paths {
    match classify_path(project_root, snapshots, path) {
      PathAction::Ignore => {}
      PathAction::AttemptPatch => saw_patch_candidate = true,
      PathAction::Restart(reason) => return HotpatchAction::Restart(reason),
    }
  }

  if saw_patch_candidate {
    HotpatchAction::AttemptPatch
  } else {
    HotpatchAction::Ignore
  }
}

pub(crate) fn classify_path(project_root: &Path, snapshots: &SourceSnapshots, path: &Path) -> PathAction {
  if is_project_root_file(path, project_root, "Cargo.toml") {
    return PathAction::Restart(RestartReason::DependencyGraph);
  }

  if is_project_root_file(path, project_root, "thebe.toml") {
    return PathAction::Restart(RestartReason::ThebeConfig);
  }

  if is_project_root_file(path, project_root, "app.html") {
    return PathAction::Restart(RestartReason::HtmlShell);
  }

  if path.extension().is_some_and(|extension| extension == "trs") {
    return classify_trs_path(project_root, snapshots, path);
  }

  if path.extension().is_some_and(|extension| extension == "rs") {
    return classify_rust_path(project_root, path);
  }

  PathAction::Ignore
}

fn classify_trs_path(project_root: &Path, snapshots: &SourceSnapshots, path: &Path) -> PathAction {
  match trs_patch_kind_with_snapshots(project_root, snapshots, path) {
    Some(TrsPatchKind::StyleOnly | TrsPatchKind::TemplateLike) => PathAction::AttemptPatch,
    None => {
      let previous_blocks = match load_snapshot_blocks_from_snapshots(snapshots, project_root, path) {
        Some(blocks) => blocks,
        None => return PathAction::Restart(RestartReason::GeneratedInput),
      };
      let current_source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(_) => return PathAction::Restart(RestartReason::GeneratedInput),
      };
      let current_blocks = match parse_blocks_for_path(project_root, path, &current_source) {
        Some(blocks) => blocks,
        None => return PathAction::Restart(RestartReason::GeneratedInput),
      };

      let client_script_changed = previous_blocks.script_ts != current_blocks.script_ts;
      let restart_change = previous_blocks.script != current_blocks.script
        || previous_blocks.script_setup != current_blocks.script_setup
        || (client_script_changed && !path_supports_client_script_hotpatch(project_root, path));

      if restart_change {
        PathAction::Restart(RestartReason::GeneratedInput)
      } else {
        PathAction::Ignore
      }
    }
  }
}

pub(crate) fn trs_patch_kind_with_snapshots(
  project_root: &Path,
  snapshots: &SourceSnapshots,
  path: &Path,
) -> Option<TrsPatchKind> {
  let previous_blocks = match load_snapshot_blocks_from_snapshots(snapshots, project_root, path) {
    Some(blocks) => blocks,
    None => return None,
  };
  let current_source = match fs::read_to_string(path) {
    Ok(source) => source,
    Err(_) => return None,
  };
  let current_blocks = match parse_blocks_for_path(project_root, path, &current_source) {
    Some(blocks) => blocks,
    None => return None,
  };

  let client_script_changed = previous_blocks.script_ts != current_blocks.script_ts;
  let restart_change = previous_blocks.script != current_blocks.script
    || previous_blocks.script_setup != current_blocks.script_setup
    || (client_script_changed && !path_supports_client_script_hotpatch(project_root, path));
  let style_changed = previous_blocks.style != current_blocks.style;
  let template_like_change = previous_blocks.head != current_blocks.head
    || previous_blocks.template != current_blocks.template;

  if restart_change {
    None
  } else if style_changed && !template_like_change && !client_script_changed {
    Some(TrsPatchKind::StyleOnly)
  } else if style_changed || template_like_change || client_script_changed {
    Some(TrsPatchKind::TemplateLike)
  } else {
    None
  }
}

fn path_supports_client_script_hotpatch(project_root: &Path, path: &Path) -> bool {
  let routes_dir = project_root.join("src").join("routes");
  let components_dir = project_root.join("src").join("components");

  (path.starts_with(&routes_dir) || path.starts_with(&components_dir))
    && path.extension().is_some_and(|extension| extension == "trs")
}

fn load_snapshot_blocks_from_snapshots(
  snapshots: &SourceSnapshots,
  project_root: &Path,
  path: &Path,
) -> Option<SfcBlocks> {
  let snapshot_source = snapshots.source_for(path)?;
  parse_blocks_for_path(project_root, path, &snapshot_source)
}

fn collect_snapshot_sources(
  project_root: &Path,
  snapshots_root: &Path,
  dir: &Path,
  sources: &mut HashMap<PathBuf, String>,
) -> io::Result<()> {
  for entry in fs::read_dir(dir)? {
    let entry = entry?;
    let path = entry.path();
    let file_type = entry.file_type()?;

    if file_type.is_dir() {
      collect_snapshot_sources(project_root, snapshots_root, &path, sources)?;
      continue;
    }

    if !file_type.is_file() {
      continue;
    }

    let relative_path = match path.strip_prefix(snapshots_root) {
      Ok(relative_path) => relative_path,
      Err(_) => continue,
    };
    let source_path = project_root.join(relative_path);
    let source = fs::read_to_string(&path)?;
    sources.insert(source_path, source);
  }

  Ok(())
}

fn parse_blocks_for_path(project_root: &Path, path: &Path, source: &str) -> Option<SfcBlocks> {
  let components_dir = project_root.join("src").join("components");
  if path.starts_with(&components_dir) {
    parse_component_sfc(source).ok()
  } else {
    parse_sfc(source).ok()
  }
}

fn classify_rust_path(project_root: &Path, path: &Path) -> PathAction {
  let src_dir = project_root.join("src");
  if !path.starts_with(&src_dir) {
    return PathAction::Restart(RestartReason::ExternalRust);
  }

  if path.file_name().is_some_and(|file_name| file_name == "main.rs") {
    return PathAction::Restart(RestartReason::EntryPoint);
  }

  PathAction::Restart(RestartReason::RustSource)
}

fn is_project_root_file(path: &Path, project_root: &Path, file_name: &str) -> bool {
  path.file_name().is_some_and(|name| name == file_name)
    && path.parent().is_some_and(|parent| parent == project_root)
}

#[cfg(test)]
mod tests {
  use super::{
    HotpatchAction, RestartReason, SourceSnapshots, classify_paths,
    classify_paths_with_snapshots,
  };
  use std::fs;
  use std::path::Path;
  use std::path::PathBuf;
  use std::process;
  use std::time::{SystemTime, UNIX_EPOCH};

  #[test]
  fn classify_paths_should_restart_when_dependency_manifest_changes() {
    let project_root = Path::new("/tmp/project");
    let changed = vec![project_root.join("Cargo.toml")];

    let action = classify_paths(project_root, &changed);

    assert_eq!(
      action,
      HotpatchAction::Restart(RestartReason::DependencyGraph)
    );
  }

  #[test]
  fn classify_paths_should_restart_when_trs_source_changes() {
    let project_root = Path::new("/tmp/project");
    let changed = vec![project_root.join("src/routes/index.trs")];

    let action = classify_paths(project_root, &changed);

    assert_eq!(
      action,
      HotpatchAction::Restart(RestartReason::GeneratedInput)
    );
  }

  #[test]
  fn classify_paths_should_attempt_patch_when_only_template_changes() {
    let project_root = temp_project_root("patch-template");
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

    let action = classify_paths(&project_root, &[route_path]);

    assert_eq!(action, HotpatchAction::AttemptPatch);

    let _ = fs::remove_dir_all(project_root);
  }

  #[test]
  fn classify_paths_should_attempt_patch_when_component_style_only_changes() {
    let project_root = temp_project_root("patch-component-style");
    let component_path = project_root.join("src/components/Card.trs");

    write_snapshot(
      &project_root,
      &component_path,
      "<style>.card { color: red; }</style>\n<div class=\"card\">Card</div>",
    );
    fs::write(
      &component_path,
      "<style>.card { color: blue; }</style>\n<div class=\"card\">Card</div>",
    )
    .expect("current component should write");

    let action = classify_paths(&project_root, &[component_path]);

    assert_eq!(action, HotpatchAction::AttemptPatch);

    let _ = fs::remove_dir_all(project_root);
  }

  #[test]
  fn classify_paths_should_attempt_patch_when_layout_head_changes() {
    let project_root = temp_project_root("patch-layout-head");
    let layout_path = project_root.join("src/routes/_layout.trs");

    write_snapshot(
      &project_root,
      &layout_path,
      "<head><meta name=\"probe\" content=\"before\" /></head><div><slot /></div>",
    );
    fs::write(
      &layout_path,
      "<head><meta name=\"probe\" content=\"after\" /></head><div><slot /></div>",
    )
    .expect("current layout should write");

    let action = classify_paths(&project_root, &[layout_path]);

    assert_eq!(action, HotpatchAction::AttemptPatch);

    let _ = fs::remove_dir_all(project_root);
  }

  #[test]
  fn classify_paths_should_attempt_patch_when_route_client_script_changes() {
    let project_root = temp_project_root("patch-route-client-script");
    let route_path = project_root.join("src/routes/index.trs");

    write_snapshot(
      &project_root,
      &route_path,
      "<script setup>fn index() -> Props { Props {} }</script>\n<script lang=\"ts\">window.__probe = \"before\";</script>\n<div>same</div>",
    );
    fs::write(
      &route_path,
      "<script setup>fn index() -> Props { Props {} }</script>\n<script lang=\"ts\">window.__probe = \"after\";</script>\n<div>same</div>",
    )
    .expect("current route should write");

    let action = classify_paths(&project_root, &[route_path]);

    assert_eq!(action, HotpatchAction::AttemptPatch);

    let _ = fs::remove_dir_all(project_root);
  }

  #[test]
  fn classify_paths_should_attempt_patch_when_component_client_script_changes() {
    let project_root = temp_project_root("patch-component-client-script");
    let component_path = project_root.join("src/components/Card.trs");

    write_snapshot(
      &project_root,
      &component_path,
      "<script>pub struct Props {}</script>\n<script lang=\"ts\">window.__probe = \"before\";</script>\n<div>same</div>",
    );
    fs::write(
      &component_path,
      "<script>pub struct Props {}</script>\n<script lang=\"ts\">window.__probe = \"after\";</script>\n<div>same</div>",
    )
    .expect("current component should write");

    let action = classify_paths(&project_root, &[component_path]);

    assert_eq!(action, HotpatchAction::AttemptPatch);

    let _ = fs::remove_dir_all(project_root);
  }

  #[test]
  fn classify_paths_should_attempt_patch_when_layout_client_script_changes() {
    let project_root = temp_project_root("patch-layout-client-script");
    let layout_path = project_root.join("src/routes/_layout.trs");

    write_snapshot(
      &project_root,
      &layout_path,
      "<script lang=\"ts\">window.__probe = \"before\";</script>\n<div><slot /></div>",
    );
    fs::write(
      &layout_path,
      "<script lang=\"ts\">window.__probe = \"after\";</script>\n<div><slot /></div>",
    )
    .expect("current layout should write");

    let action = classify_paths(&project_root, &[layout_path]);

    assert_eq!(action, HotpatchAction::AttemptPatch);

    let _ = fs::remove_dir_all(project_root);
  }

  #[test]
  fn classify_paths_with_snapshots_should_survive_snapshot_dir_rewrites() {
    let project_root = temp_project_root("stable-snapshots");
    let route_path = project_root.join("src/routes/index.trs");

    write_snapshot(
      &project_root,
      &route_path,
      "<script setup>fn index() -> Props { Props {} }</script>\n<div>before</div>",
    );
    let snapshots = SourceSnapshots::load(&project_root)
      .expect("snapshots should load from generated sources");
    fs::write(
      &route_path,
      "<script setup>fn index() -> Props { Props {} }</script>\n<div>after</div>",
    )
    .expect("current route should write");
    fs::remove_dir_all(project_root.join(thebe_project::THEBE_SOURCE_SNAPSHOTS_DIR))
      .expect("snapshot dir should be removable after load");

    let action = classify_paths_with_snapshots(&project_root, &snapshots, &[route_path]);

    assert_eq!(action, HotpatchAction::AttemptPatch);

    let _ = fs::remove_dir_all(project_root);
  }

  #[test]
  fn classify_paths_should_restart_when_script_setup_changes_between_snapshots() {
    let project_root = temp_project_root("restart-script-setup");
    let route_path = project_root.join("src/routes/index.trs");

    write_snapshot(
      &project_root,
      &route_path,
      "<script setup>fn index() -> Props { Props { count: 1 } }</script>\n<div>{{ count }}</div>",
    );
    fs::write(
      &route_path,
      "<script setup>fn index() -> Props { Props { count: 2 } }</script>\n<div>{{ count }}</div>",
    )
    .expect("current route should write");

    let action = classify_paths(&project_root, &[route_path]);

    assert_eq!(
      action,
      HotpatchAction::Restart(RestartReason::GeneratedInput)
    );

    let _ = fs::remove_dir_all(project_root);
  }

  #[test]
  fn classify_paths_should_restart_when_new_route_file_appears() {
    let project_root = temp_project_root("restart-new-route");
    let route_path = project_root.join("src/routes/index.trs");

    fs::write(
      &route_path,
      "<script setup>struct Props {}\n#[thebe::get]\npub fn index() -> Props { Props {} }</script>\n<div>new</div>",
    )
    .expect("current route should write");

    let action = classify_paths(&project_root, &[route_path]);

    assert_eq!(
      action,
      HotpatchAction::Restart(RestartReason::GeneratedInput)
    );

    let _ = fs::remove_dir_all(project_root);
  }

  #[test]
  fn classify_paths_should_restart_when_existing_route_is_deleted() {
    let project_root = temp_project_root("restart-deleted-route");
    let route_path = project_root.join("src/routes/index.trs");

    write_snapshot(
      &project_root,
      &route_path,
      "<script setup>struct Props {}\n#[thebe::get]\npub fn index() -> Props { Props {} }</script>\n<div>before</div>",
    );

    let action = classify_paths(&project_root, &[route_path]);

    assert_eq!(
      action,
      HotpatchAction::Restart(RestartReason::GeneratedInput)
    );

    let _ = fs::remove_dir_all(project_root);
  }

  #[test]
  fn classify_paths_should_restart_when_component_plain_script_changes() {
    let project_root = temp_project_root("restart-component-script");
    let component_path = project_root.join("src/components/Card.trs");

    write_snapshot(
      &project_root,
      &component_path,
      "<script>export interface Props { title: string }</script>\n<div>{{ props.title }}</div>",
    );
    fs::write(
      &component_path,
      "<script>export interface Props { label: string }</script>\n<div>{{ props.label }}</div>",
    )
    .expect("current component should write");

    let action = classify_paths(&project_root, &[component_path]);

    assert_eq!(
      action,
      HotpatchAction::Restart(RestartReason::GeneratedInput)
    );

    let _ = fs::remove_dir_all(project_root);
  }

  fn temp_project_root(test_name: &str) -> PathBuf {
    let unique_suffix = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap_or_default()
      .as_nanos();
    let project_root = std::env::temp_dir().join(format!(
      "thebe-classify-{test_name}-{}-{unique_suffix}",
      process::id()
    ));
    fs::create_dir_all(project_root.join("src/routes")).expect("routes dir should create");
    fs::create_dir_all(project_root.join("src/components")).expect("components dir should create");
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

  #[test]
  fn classify_paths_should_restart_when_non_entry_rust_sources_change() {
    let project_root = Path::new("/tmp/project");
    let changed = vec![project_root.join("src/state.rs")];

    let action = classify_paths(project_root, &changed);

    assert_eq!(action, HotpatchAction::Restart(RestartReason::RustSource));
  }

  #[test]
  fn classify_paths_should_restart_when_entry_point_changes() {
    let project_root = Path::new("/tmp/project");
    let changed = vec![project_root.join("src/main.rs")];

    let action = classify_paths(project_root, &changed);

    assert_eq!(
      action,
      HotpatchAction::Restart(RestartReason::EntryPoint)
    );
  }

  #[test]
  fn classify_paths_should_ignore_non_compiler_inputs() {
    let project_root = Path::new("/tmp/project");
    let changed = vec![PathBuf::from("/tmp/project/public/logo.png")];

    let action = classify_paths(project_root, &changed);

    assert_eq!(action, HotpatchAction::Ignore);
  }
}
