use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{OnceLock, RwLock};
use std::thread;
use std::time::Duration;
use thiserror::Error;

/// Environment variable pointing at the active `.thebe/hotpatch/session.json` file.
pub const HOTPATCH_SESSION_ENV: &str = "THEBE_HOTPATCH_SESSION";

/// Second protocol version for Thebe's local hotpatch transport.
pub const PROTOCOL_VERSION: u16 = 2;

const FRAME_HEADER_LEN: usize = 4;
const MAX_FRAME_SIZE: usize = 8 * 1024 * 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(250);
const HELLO_RESPONSE_TIMEOUT: Duration = Duration::from_millis(50);
static HOTPATCH_TEXT_ARTIFACTS: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();
const BROWSER_EVENTS_PATH: &str = "/.thebe/dev/events";

/// Handshake payload sent by the running process to the local patch server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeHello {
  pub protocol_version: u16,
  pub session_id: String,
  pub process_id: u32,
  pub build_id: String,
  pub aslr_reference: u64,
}

/// Framed messages exchanged over Thebe's local hotpatch transport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeMessage {
  Hello(RuntimeHello),
  Patch { build_id: String, payload: Vec<u8> },
  PatchApplied { build_id: String },
  RestartRequired { reason: String },
  Error { message: String },
  Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimePatchPayload {
  TextArtifact { path: String, contents: String },
}

/// Errors while encoding or decoding a framed runtime message.
#[derive(Debug, Error)]
pub enum FrameError {
  #[error("frame payload length {0} exceeds the maximum supported size")]
  FrameTooLarge(usize),
  #[error("frame header is truncated")]
  TruncatedHeader,
  #[error("frame payload is truncated")]
  TruncatedPayload,
  #[error("frame has trailing bytes")]
  TrailingBytes,
  #[error("failed to encode or decode runtime message: {0}")]
  Json(#[from] serde_json::Error),
}

/// Errors when the running app connects back to the local hotpatch server.
#[derive(Debug, Error)]
pub enum HotpatchConnectError {
  #[error("failed to read hotpatch session manifest {path}: {source}")]
  ReadSession {
    path: PathBuf,
    #[source]
    source: io::Error,
  },
  #[error("failed to parse hotpatch session manifest {path}: {source}")]
  ParseSession {
    path: PathBuf,
    #[source]
    source: serde_json::Error,
  },
  #[error("unsupported hotpatch protocol version {0}")]
  UnsupportedProtocolVersion(u16),
  #[error("failed to resolve the current executable path: {0}")]
  CurrentExecutable(#[source] io::Error),
  #[error("failed to configure the hotpatch transport for {addr}: {source}")]
  ConfigureSocket {
    addr: SocketAddr,
    #[source]
    source: io::Error,
  },
  #[error("failed to connect to the hotpatch server at {addr}: {source}")]
  Connect {
    addr: SocketAddr,
    #[source]
    source: io::Error,
  },
  #[error("failed to encode the runtime hello frame: {0}")]
  EncodeHello(#[source] FrameError),
  #[error("failed to write the runtime hello frame to {addr}: {source}")]
  WriteHello {
    addr: SocketAddr,
    #[source]
    source: io::Error,
  },
  #[error("failed to decode the hotpatch server response from {addr}: {source}")]
  DecodeResponse {
    addr: SocketAddr,
    #[source]
    source: FrameError,
  },
  #[error("failed to read the hotpatch server response from {addr}: {source}")]
  ReadResponse {
    addr: SocketAddr,
    #[source]
    source: io::Error,
  },
  #[error("hotpatch server rejected the runtime connection: {0}")]
  ServerRejected(String),
  #[error("hotpatch server returned an unexpected response: {0:?}")]
  UnexpectedResponse(RuntimeMessage),
  #[error("failed to start the hotpatch runtime control thread: {0}")]
  SpawnControlThread(#[source] io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RuntimeControlAction {
  Continue(Option<String>),
  Exit(String),
}

#[derive(Debug, Deserialize)]
struct SessionManifest {
  protocol_version: u16,
  session_id: String,
  server_addr: SocketAddr,
  #[serde(default)]
  browser_addr: Option<SocketAddr>,
}

/// Connect to the local hotpatch server when `THEBE_HOTPATCH_SESSION` is present.
///
/// # Errors
/// Returns an error when the session manifest cannot be read, the connection
/// cannot be established, or the server rejects the runtime handshake.
pub fn connect_hotpatch_from_env() -> Result<Option<RuntimeHello>, HotpatchConnectError> {
  let Some(session_path) = env::var_os(HOTPATCH_SESSION_ENV) else {
    return Ok(None);
  };

  connect_from_path(Path::new(&session_path)).map(Some)
}

/// Load a generated text artifact, consulting any hotpatch override first.
///
/// # Errors
/// Returns the underlying filesystem error when the artifact cannot be read.
pub fn load_text_artifact(path: &str) -> io::Result<String> {
  if let Ok(artifacts) = hotpatch_text_artifacts().read()
    && let Some(contents) = artifacts.get(path) {
      return Ok(contents.clone());
  }

  fs::read_to_string(path)
}

/// Resolve the CLI-owned browser event stream URL from the hotpatch session.
#[must_use]
pub fn browser_events_url_from_env() -> Option<String> {
  let session_path = env::var_os(HOTPATCH_SESSION_ENV)?;
  let manifest = read_session_manifest(Path::new(&session_path)).ok()?;
  manifest
    .browser_addr
    .map(|addr| format!("http://{addr}{BROWSER_EVENTS_PATH}"))
}

/// Encode a runtime patch payload for transport over the hotpatch control socket.
///
/// # Errors
/// Returns an error when the payload cannot be serialized.
pub fn encode_patch_payload(payload: &RuntimePatchPayload) -> Result<Vec<u8>, serde_json::Error> {
  serde_json::to_vec(payload)
}

/// Encode a runtime message as a length-prefixed JSON frame.
///
/// # Errors
/// Returns an error when the message cannot be serialized or the resulting
/// payload exceeds the transport frame limit.
pub fn encode_message(message: &RuntimeMessage) -> Result<Vec<u8>, FrameError> {
  let payload = serde_json::to_vec(message)?;
  if payload.len() > MAX_FRAME_SIZE {
    return Err(FrameError::FrameTooLarge(payload.len()));
  }

  let frame_len = u32::try_from(payload.len())
    .map_err(|_| FrameError::FrameTooLarge(payload.len()))?;
  let mut frame = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
  frame.extend_from_slice(&frame_len.to_be_bytes());
  frame.extend_from_slice(&payload);
  Ok(frame)
}

/// Decode a length-prefixed JSON frame into a runtime message.
///
/// # Errors
/// Returns an error when the frame header or payload is invalid or when the
/// message cannot be deserialized.
pub fn decode_message(frame: &[u8]) -> Result<RuntimeMessage, FrameError> {
  if frame.len() < FRAME_HEADER_LEN {
    return Err(FrameError::TruncatedHeader);
  }

  let mut header = [0_u8; FRAME_HEADER_LEN];
  header.copy_from_slice(&frame[..FRAME_HEADER_LEN]);
  let declared_len = u32::from_be_bytes(header);
  let declared_len = usize::try_from(declared_len)
    .map_err(|_| FrameError::FrameTooLarge(usize::MAX))?;

  if declared_len > MAX_FRAME_SIZE {
    return Err(FrameError::FrameTooLarge(declared_len));
  }

  let expected_total_len = FRAME_HEADER_LEN + declared_len;
  if frame.len() < expected_total_len {
    return Err(FrameError::TruncatedPayload);
  }

  if frame.len() > expected_total_len {
    return Err(FrameError::TrailingBytes);
  }

  let payload = &frame[FRAME_HEADER_LEN..expected_total_len];
  Ok(serde_json::from_slice(payload)?)
}

fn connect_from_path(session_path: &Path) -> Result<RuntimeHello, HotpatchConnectError> {
  let session_manifest = read_session_manifest(session_path)?;
  if session_manifest.protocol_version != PROTOCOL_VERSION {
    return Err(HotpatchConnectError::UnsupportedProtocolVersion(
      session_manifest.protocol_version,
    ));
  }

  let hello = RuntimeHello {
    protocol_version: PROTOCOL_VERSION,
    session_id: session_manifest.session_id.clone(),
    process_id: process::id(),
    build_id: current_build_id()?,
    aslr_reference: 0,
  };

  let mut stream = TcpStream::connect_timeout(&session_manifest.server_addr, HANDSHAKE_TIMEOUT)
    .map_err(|source| HotpatchConnectError::Connect {
      addr: session_manifest.server_addr,
      source,
    })?;
  stream
    .set_write_timeout(Some(HANDSHAKE_TIMEOUT))
    .map_err(|source| HotpatchConnectError::ConfigureSocket {
      addr: session_manifest.server_addr,
      source,
    })?;
  stream
    .set_read_timeout(Some(HELLO_RESPONSE_TIMEOUT))
    .map_err(|source| HotpatchConnectError::ConfigureSocket {
      addr: session_manifest.server_addr,
      source,
    })?;

  write_message(&mut stream, &hello, session_manifest.server_addr)?;

  match read_response(&mut stream, session_manifest.server_addr)? {
    Some(RuntimeMessage::Error { message }) => Err(HotpatchConnectError::ServerRejected(message)),
    Some(message) => Err(HotpatchConnectError::UnexpectedResponse(message)),
    None => {
      spawn_runtime_control_thread(stream)?;
      Ok(hello)
    }
  }
}

fn spawn_runtime_control_thread(mut stream: TcpStream) -> Result<(), HotpatchConnectError> {
  stream
    .set_read_timeout(None)
    .map_err(|source| HotpatchConnectError::ConfigureSocket {
      addr: stream.peer_addr().unwrap_or_else(|_| SocketAddr::from(([127, 0, 0, 1], 0))),
      source,
    })?;

  thread::Builder::new()
    .name(String::from("thebe-hotpatch-runtime"))
    .spawn(move || run_runtime_control_loop(&mut stream))
    .map(|_| ())
    .map_err(HotpatchConnectError::SpawnControlThread)
}

fn run_runtime_control_loop(stream: &mut TcpStream) {
  loop {
    let frame = match read_frame(stream) {
      Ok(frame) => frame,
      Err(error) if is_benign_handshake_end(&error) => return,
      Err(error) => {
        eprintln!("thebe: hotpatch control loop stopped: {error}");
        return;
      }
    };

    let message = match decode_message(&frame) {
      Ok(message) => message,
      Err(error) => {
        eprintln!("thebe: hotpatch control loop failed to decode a message: {error}");
        return;
      }
    };

    let (action, response) = handle_runtime_message(message);

    if let Some(response) = response
      && let Err(error) = write_control_response(stream, &response) {
        eprintln!("thebe: hotpatch control loop failed to send a response: {error}");
        return;
      }

    match action {
      RuntimeControlAction::Continue(Some(message)) => eprintln!("{message}"),
      RuntimeControlAction::Continue(None) => {}
      RuntimeControlAction::Exit(message) => {
        println!("{message}");
        process::exit(0);
      }
    }
  }
}

#[cfg_attr(not(test), expect(dead_code, reason = "retained for focused hotpatch tests"))]
fn runtime_control_action(message: RuntimeMessage) -> RuntimeControlAction {
  handle_runtime_message(message).0
}

fn handle_runtime_message(message: RuntimeMessage) -> (RuntimeControlAction, Option<RuntimeMessage>) {
  match message {
    RuntimeMessage::Shutdown => {
      (
        RuntimeControlAction::Exit(String::from("thebe: hotpatch shutdown requested")),
        None,
      )
    }
    RuntimeMessage::RestartRequired { reason } => (
      RuntimeControlAction::Exit(format!(
        "thebe: hotpatch restart requested — {reason}"
      )),
      None,
    ),
    RuntimeMessage::Patch { build_id, payload } => match apply_patch_payload(&build_id, &payload) {
      Ok(message) => (
        RuntimeControlAction::Continue(Some(message)),
        Some(RuntimeMessage::PatchApplied { build_id }),
      ),
      Err(message) => (
        RuntimeControlAction::Continue(Some(format!(
          "thebe: hotpatch patch apply failed — {message}"
        ))),
        Some(RuntimeMessage::Error {
          message: format!("failed to apply patch {build_id}: {message}"),
        }),
      ),
    },
    RuntimeMessage::PatchApplied { build_id } => (
      RuntimeControlAction::Continue(Some(format!(
        "thebe: hotpatch runtime received an unexpected patch acknowledgement for {build_id}"
      ))),
      None,
    ),
    RuntimeMessage::Error { message } => (
      RuntimeControlAction::Continue(Some(format!(
        "thebe: hotpatch server error — {message}"
      ))),
      None,
    ),
    RuntimeMessage::Hello(_) => (
      RuntimeControlAction::Continue(Some(String::from(
        "thebe: hotpatch runtime received an unexpected hello message",
      ))),
      None,
    ),
  }
}

fn write_control_response(stream: &mut TcpStream, message: &RuntimeMessage) -> io::Result<()> {
  let frame = encode_message(message)
    .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
  stream.write_all(&frame)?;
  stream.flush()
}

fn hotpatch_text_artifacts() -> &'static RwLock<HashMap<String, String>> {
  HOTPATCH_TEXT_ARTIFACTS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn apply_patch_payload(build_id: &str, payload: &[u8]) -> Result<String, String> {
  let patch = serde_json::from_slice::<RuntimePatchPayload>(payload)
    .map_err(|error| format!("invalid patch payload: {error}"))?;

  match patch {
    RuntimePatchPayload::TextArtifact { path, contents } => {
      let mut artifacts = hotpatch_text_artifacts()
        .write()
        .map_err(|_| String::from("artifact registry lock poisoned"))?;
      artifacts.insert(path.clone(), contents);
      Ok(format!(
        "thebe: hotpatch applied patch {build_id} to {path}"
      ))
    }
  }
}

fn read_session_manifest(session_path: &Path) -> Result<SessionManifest, HotpatchConnectError> {
  let manifest_bytes = fs::read(session_path).map_err(|source| {
    HotpatchConnectError::ReadSession {
      path: session_path.to_path_buf(),
      source,
    }
  })?;

  serde_json::from_slice(&manifest_bytes).map_err(|source| HotpatchConnectError::ParseSession {
    path: session_path.to_path_buf(),
    source,
  })
}

fn current_build_id() -> Result<String, HotpatchConnectError> {
  let current_exe = env::current_exe().map_err(HotpatchConnectError::CurrentExecutable)?;
  Ok(current_exe.display().to_string())
}

fn write_message(
  stream: &mut TcpStream,
  hello: &RuntimeHello,
  server_addr: SocketAddr,
) -> Result<(), HotpatchConnectError> {
  let frame = encode_message(&RuntimeMessage::Hello(hello.clone()))
    .map_err(HotpatchConnectError::EncodeHello)?;
  stream
    .write_all(&frame)
    .map_err(|source| HotpatchConnectError::WriteHello {
      addr: server_addr,
      source,
    })?;
  stream
    .flush()
    .map_err(|source| HotpatchConnectError::WriteHello {
      addr: server_addr,
      source,
    })
}

fn read_response(
  stream: &mut TcpStream,
  server_addr: SocketAddr,
) -> Result<Option<RuntimeMessage>, HotpatchConnectError> {
  let frame = match read_frame(stream) {
    Ok(frame) => frame,
    Err(error) if is_benign_handshake_end(&error) => return Ok(None),
    Err(source) => {
      return Err(HotpatchConnectError::ReadResponse {
        addr: server_addr,
        source,
      });
    }
  };

  decode_message(&frame)
    .map(Some)
    .map_err(|source| HotpatchConnectError::DecodeResponse {
      addr: server_addr,
      source,
    })
}

fn read_frame(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
  let mut header = [0_u8; FRAME_HEADER_LEN];
  stream.read_exact(&mut header)?;
  let payload_len = usize::try_from(u32::from_be_bytes(header))
    .map_err(|_| io::Error::new(ErrorKind::InvalidData, "frame length exceeds usize"))?;

  if payload_len > MAX_FRAME_SIZE {
    return Err(io::Error::new(
      ErrorKind::InvalidData,
      format!("frame payload length {payload_len} exceeds the maximum supported size"),
    ));
  }

  let mut payload = vec![0_u8; payload_len];
  stream.read_exact(&mut payload)?;

  let mut frame = header.to_vec();
  frame.extend_from_slice(&payload);
  Ok(frame)
}

fn is_benign_handshake_end(error: &io::Error) -> bool {
  matches!(
    error.kind(),
    ErrorKind::UnexpectedEof
      | ErrorKind::ConnectionReset
      | ErrorKind::ConnectionAborted
      | ErrorKind::TimedOut
      | ErrorKind::WouldBlock
  )
}

#[cfg(test)]
mod tests {
  use super::{
    HOTPATCH_SESSION_ENV, HotpatchConnectError, PROTOCOL_VERSION, RuntimeControlAction,
    RuntimeMessage, RuntimePatchPayload, connect_from_path, decode_message,
    encode_message, encode_patch_payload, load_text_artifact, read_frame, runtime_control_action,
  };
  use std::env;
  use std::fs;
  use std::io::{Read, Write};
  use std::net::{SocketAddr, TcpListener};
  use std::path::PathBuf;
  use std::process;
  use std::thread;
  use std::time::{SystemTime, UNIX_EPOCH};

  #[test]
  fn connect_from_path_should_send_a_runtime_hello() {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
      .expect("listener bind should succeed");
    let server_addr = listener.local_addr().expect("listener should expose a local addr");
    let session_path = write_session_manifest(server_addr, "session-1");

    let listener_thread = thread::spawn(move || {
      let (mut stream, _peer_addr) = listener.accept().expect("runtime should connect");
      let frame = read_frame(&mut stream).expect("hello frame should read");
      let message = decode_message(&frame).expect("hello frame should decode");
      let RuntimeMessage::Hello(hello) = message else {
        panic!("expected a hello frame");
      };
      hello
    });

    let hello = connect_from_path(&session_path).expect("runtime handshake should succeed");
    let received_hello = listener_thread
      .join()
      .expect("listener thread should join");

    assert_eq!(hello.session_id, String::from("session-1"));
    assert_eq!(received_hello.session_id, hello.session_id);
    assert_eq!(received_hello.protocol_version, PROTOCOL_VERSION);

    let _ = fs::remove_file(session_path);
  }

  #[test]
  fn connect_from_path_should_surface_server_rejections() {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
      .expect("listener bind should succeed");
    let server_addr = listener.local_addr().expect("listener should expose a local addr");
    let session_path = write_session_manifest(server_addr, "session-1");

    let server = thread::spawn(move || {
      let (mut stream, _peer_addr) = listener.accept().expect("runtime should connect");
      let mut header = [0_u8; 4];
      stream.read_exact(&mut header).expect("hello header should read");
      let payload_len = usize::try_from(u32::from_be_bytes(header))
        .expect("payload length should fit usize");
      let mut payload = vec![0_u8; payload_len];
      stream.read_exact(&mut payload).expect("hello payload should read");

      let frame = encode_message(&RuntimeMessage::Error {
        message: String::from("session mismatch"),
      })
      .expect("error frame should encode");
      stream.write_all(&frame).expect("error frame should send");
    });

    let error = connect_from_path(&session_path)
      .expect_err("server rejection should fail the handshake");

    assert!(matches!(
      error,
      HotpatchConnectError::ServerRejected(message) if message == "session mismatch"
    ));
    server.join().expect("listener thread should join");

    let _ = fs::remove_file(session_path);
  }

  #[test]
  fn connect_hotpatch_from_env_should_skip_when_the_env_var_is_missing() {
    let old_value = env::var_os(HOTPATCH_SESSION_ENV);
    unsafe {
      env::remove_var(HOTPATCH_SESSION_ENV);
    }

    let result = super::connect_hotpatch_from_env().expect("missing env should be a no-op");

    assert_eq!(result, None);
    restore_hotpatch_env(old_value);
  }

  #[test]
  fn runtime_control_action_should_exit_on_shutdown() {
    let action = runtime_control_action(RuntimeMessage::Shutdown);

    assert_eq!(
      action,
      RuntimeControlAction::Exit(String::from("thebe: hotpatch shutdown requested"))
    );
  }

  #[test]
  fn runtime_control_action_should_exit_on_restart_required() {
    let action = runtime_control_action(RuntimeMessage::RestartRequired {
      reason: String::from("rebuild required"),
    });

    assert_eq!(
      action,
      RuntimeControlAction::Exit(String::from(
        "thebe: hotpatch restart requested — rebuild required"
      ))
    );
  }

  #[test]
  fn runtime_control_action_should_apply_text_artifact_patches() {
    let artifact_path = format!(
      ".thebe/dev/routes/test-{}-{}.json",
      process::id(),
      SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
    );
    let payload = encode_patch_payload(&RuntimePatchPayload::TextArtifact {
      path: artifact_path.clone(),
      contents: String::from("patched-contents"),
    })
    .expect("patch payload should encode");

    let (action, response) = super::handle_runtime_message(RuntimeMessage::Patch {
      build_id: String::from("patch-1"),
      payload,
    });

    assert!(matches!(action, RuntimeControlAction::Continue(Some(message)) if message.contains("hotpatch applied patch")));
    assert_eq!(
      response,
      Some(RuntimeMessage::PatchApplied {
        build_id: String::from("patch-1"),
      })
    );
    assert_eq!(
      load_text_artifact(&artifact_path).expect("patched artifact should load"),
      "patched-contents"
    );
  }

  #[test]
  fn browser_events_url_from_env_should_resolve_the_cli_channel() {
    let session_path = temp_session_path("browser-events-url");
    let manifest = serde_json::json!({
      "protocol_version": PROTOCOL_VERSION,
      "session_id": "session-1",
      "process_id": process::id(),
      "created_at_unix_ms": 1,
      "server_addr": "127.0.0.1:4100",
      "browser_addr": "127.0.0.1:4200"
    });
    fs::write(
      &session_path,
      serde_json::to_vec_pretty(&manifest).expect("manifest should serialize"),
    )
    .expect("manifest should write");

    let old_value = env::var_os(HOTPATCH_SESSION_ENV);
    unsafe {
      env::set_var(HOTPATCH_SESSION_ENV, &session_path);
    }

    assert_eq!(
      super::browser_events_url_from_env().as_deref(),
      Some("http://127.0.0.1:4200/.thebe/dev/events")
    );

    restore_hotpatch_env(old_value);
    let _ = fs::remove_file(session_path);
  }

  fn write_session_manifest(server_addr: SocketAddr, session_id: &str) -> PathBuf {
    let session_path = temp_session_path(session_id);
    let manifest = serde_json::json!({
      "protocol_version": PROTOCOL_VERSION,
      "session_id": session_id,
      "process_id": process::id(),
      "created_at_unix_ms": 1,
      "server_addr": server_addr,
    });
    fs::write(
      &session_path,
      serde_json::to_vec_pretty(&manifest).expect("manifest should serialize"),
    )
    .expect("manifest should write");
    session_path
  }

  fn temp_session_path(test_name: &str) -> PathBuf {
    let unique_suffix = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap_or_default()
      .as_nanos();
    env::temp_dir().join(format!(
      "thebe-runtime-hotpatch-{test_name}-{}-{unique_suffix}.json",
      process::id()
    ))
  }

  fn restore_hotpatch_env(value: Option<std::ffi::OsString>) {
    if let Some(value) = value {
      unsafe {
        env::set_var(HOTPATCH_SESSION_ENV, value);
      }
    } else {
      unsafe {
        env::remove_var(HOTPATCH_SESSION_ENV);
      }
    }
  }
}
