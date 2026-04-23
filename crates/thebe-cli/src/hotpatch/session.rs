use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};
use thebe_runtime::hotpatch::PROTOCOL_VERSION;

const HOTPATCH_DIR_NAME: &str = "hotpatch";
const SESSION_MANIFEST_NAME: &str = "session.json";

/// Persistent paths created for a hotpatch session under `.thebe/hotpatch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionPaths {
  pub root_dir: PathBuf,
  pub manifest_path: PathBuf,
}

/// Metadata written by the CLI so runtime and patch engine state share a stable session id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionManifest {
  pub protocol_version: u16,
  pub session_id: String,
  pub process_id: u32,
  pub created_at_unix_ms: u128,
  pub server_addr: SocketAddr,
  pub browser_addr: Option<SocketAddr>,
}

/// Session state created at hotpatch startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HotpatchSession {
  pub manifest: SessionManifest,
  pub paths: SessionPaths,
}

impl HotpatchSession {
  /// Create the `.thebe/hotpatch` layout and persist a new session manifest.
  pub(crate) fn initialize(
    project_root: &Path,
    server_addr: SocketAddr,
    browser_addr: Option<SocketAddr>,
  ) -> io::Result<Self> {
    let paths = SessionPaths::new(project_root);

    let session = Self {
      manifest: SessionManifest::new(server_addr, browser_addr),
      paths,
    };
    session.ensure_persisted()?;
    Ok(session)
  }

  /// Recreate the hotpatch session directory and rewrite the current manifest.
  pub(crate) fn ensure_persisted(&self) -> io::Result<()> {
    fs::create_dir_all(&self.paths.root_dir)?;
    self.persist()
  }

  /// Load a previously persisted session manifest.
  #[cfg_attr(
    not(test),
    expect(dead_code, reason = "the runtime bridge will load persisted hotpatch sessions")
  )]
  pub(crate) fn load(project_root: &Path) -> io::Result<Self> {
    let paths = SessionPaths::new(project_root);
    let manifest_bytes = fs::read(&paths.manifest_path)?;
    let manifest = serde_json::from_slice(&manifest_bytes)
      .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

    Ok(Self { manifest, paths })
  }

  fn persist(&self) -> io::Result<()> {
    let manifest_bytes = serde_json::to_vec_pretty(&self.manifest)
      .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    fs::write(&self.paths.manifest_path, manifest_bytes)
  }
}

impl SessionPaths {
  fn new(project_root: &Path) -> Self {
    let root_dir = project_root
      .join(thebe_project::THEBE_DIR)
      .join(HOTPATCH_DIR_NAME);
    let manifest_path = root_dir.join(SESSION_MANIFEST_NAME);
    Self {
      root_dir,
      manifest_path,
    }
  }
}

impl SessionManifest {
  fn new(server_addr: SocketAddr, browser_addr: Option<SocketAddr>) -> Self {
    let created_at_unix_ms = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap_or_default()
      .as_millis();

    Self {
      protocol_version: PROTOCOL_VERSION,
      session_id: generate_session_id(created_at_unix_ms),
      process_id: process::id(),
      created_at_unix_ms,
      server_addr,
      browser_addr,
    }
  }
}

fn generate_session_id(created_at_unix_ms: u128) -> String {
  format!("{:x}-{:x}", process::id(), created_at_unix_ms)
}

#[cfg(test)]
mod tests {
  use super::HotpatchSession;
  use std::env;
  use std::fs;
  use std::net::SocketAddr;
  use std::path::PathBuf;
  use std::process;
  use std::time::{SystemTime, UNIX_EPOCH};
  use thebe_runtime::hotpatch::PROTOCOL_VERSION;

  #[test]
  fn initialize_should_persist_session_manifest_under_thebe_hotpatch() {
    let project_root = temp_project_root("persist-manifest");

    let session = HotpatchSession::initialize(&project_root, test_server_addr(), Some(test_browser_addr()))
      .expect("session initialization should succeed");

    assert!(session.paths.root_dir.ends_with(".thebe/hotpatch"));
    assert!(session.paths.manifest_path.exists());

    let _ = fs::remove_dir_all(&project_root);
  }

  #[test]
  fn load_should_round_trip_the_persisted_manifest() {
    let project_root = temp_project_root("load-session");

    let initialized = HotpatchSession::initialize(&project_root, test_server_addr(), Some(test_browser_addr()))
      .expect("session initialization should succeed");
    let loaded = HotpatchSession::load(&project_root)
      .expect("loading a persisted session should succeed");

    assert_eq!(loaded.manifest, initialized.manifest);

    let _ = fs::remove_dir_all(&project_root);
  }

  #[test]
  fn initialize_should_stamp_the_current_protocol_version() {
    let project_root = temp_project_root("protocol-version");

    let session = HotpatchSession::initialize(&project_root, test_server_addr(), Some(test_browser_addr()))
      .expect("session initialization should succeed");

    assert_eq!(session.manifest.protocol_version, PROTOCOL_VERSION);

    let _ = fs::remove_dir_all(&project_root);
  }

  #[test]
  fn initialize_should_persist_the_patch_server_addr() {
    let project_root = temp_project_root("server-addr");

    let session = HotpatchSession::initialize(&project_root, test_server_addr(), Some(test_browser_addr()))
      .expect("session initialization should succeed");

    assert_eq!(session.manifest.server_addr, test_server_addr());

    let _ = fs::remove_dir_all(&project_root);
  }

  #[test]
  fn ensure_persisted_should_restore_deleted_session_manifest() {
    let project_root = temp_project_root("restore-session-manifest");

    let session = HotpatchSession::initialize(&project_root, test_server_addr(), Some(test_browser_addr()))
      .expect("session initialization should succeed");
    fs::remove_dir_all(&session.paths.root_dir)
      .expect("hotpatch session directory should be removable");

    session
      .ensure_persisted()
      .expect("restoring the hotpatch session should succeed");
    let restored = HotpatchSession::load(&project_root)
      .expect("restored hotpatch session should load");

    assert_eq!(restored.manifest, session.manifest);

    let _ = fs::remove_dir_all(&project_root);
  }

  fn test_server_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 4100))
  }

  fn test_browser_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 4200))
  }

  fn temp_project_root(test_name: &str) -> PathBuf {
    let unique_suffix = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap_or_default()
      .as_nanos();
    let project_root = env::temp_dir().join(format!(
      "thebe-hotpatch-{test_name}-{}-{unique_suffix}",
      process::id()
    ));
    fs::create_dir_all(project_root.join("src"))
      .expect("temporary project root should be created");
    project_root
  }
}
