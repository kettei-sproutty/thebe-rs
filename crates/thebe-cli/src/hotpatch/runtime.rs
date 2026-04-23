use crate::hotpatch::session::SessionManifest;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use thebe_runtime::hotpatch::{
  RuntimeHello, RuntimeMessage, decode_message, encode_message,
};

#[cfg(test)]
use thebe_runtime::hotpatch::{FrameError, PROTOCOL_VERSION};

const FRAME_HEADER_LEN: usize = 4;
const MAX_FRAME_SIZE: usize = 8 * 1024 * 1024;
const PATCH_ACK_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PatchServerEvent {
  HelloAccepted(RuntimeHello),
  PatchApplied(String),
}

#[derive(Debug)]
enum PatchServerCommand {
  ShutdownRuntime,
  ApplyPatch {
    build_id: String,
    payload: Vec<u8>,
    response_tx: Sender<io::Result<()>>,
  },
  RestartRuntime(String),
}

struct ActiveRuntime {
  process_id: u32,
  stream: TcpStream,
}

pub(crate) struct PatchServer {
  active_process_id: Arc<Mutex<Option<u32>>>,
  command_tx: Sender<PatchServerCommand>,
  local_addr: SocketAddr,
  shutdown_tx: Option<Sender<()>>,
  worker: Option<JoinHandle<()>>,
  #[cfg(test)]
  events_rx: Receiver<PatchServerEvent>,
}

impl PatchServer {
  pub(crate) fn bind() -> io::Result<(TcpListener, SocketAddr)> {
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    let local_addr = listener.local_addr()?;
    Ok((listener, local_addr))
  }

  pub(crate) fn spawn(listener: TcpListener, expected_session: SessionManifest) -> io::Result<Self> {
    listener.set_nonblocking(true)?;
    let local_addr = listener.local_addr()?;
    let active_process_id = Arc::new(Mutex::new(None));
    let active_process_id_for_worker = Arc::clone(&active_process_id);
    let (command_tx, command_rx) = mpsc::channel();
    let (shutdown_tx, shutdown_rx) = mpsc::channel();

    #[cfg(test)]
    let (events_tx, events_rx) = mpsc::channel();
    #[cfg(not(test))]
    let events_tx = None;
    #[cfg(test)]
    let events_tx = Some(events_tx);

    let worker = thread::Builder::new()
      .name(String::from("thebe-hotpatch-server"))
      .spawn(move || {
        run_patch_server(
          &listener,
          &expected_session,
          &active_process_id_for_worker,
          &command_rx,
          &shutdown_rx,
          events_tx.as_ref(),
        );
      })
      .map_err(io::Error::other)?;

    Ok(Self {
      active_process_id,
      command_tx,
      local_addr,
      shutdown_tx: Some(shutdown_tx),
      worker: Some(worker),
      #[cfg(test)]
      events_rx,
    })
  }

  #[must_use]
  pub(crate) fn local_addr(&self) -> SocketAddr {
    self.local_addr
  }

  pub(crate) fn request_shutdown(&self) {
    let _ = self.command_tx.send(PatchServerCommand::ShutdownRuntime);
  }

  pub(crate) fn request_restart(&self, reason: &str) {
    let _ = self.command_tx.send(PatchServerCommand::RestartRuntime(String::from(reason)));
  }

  pub(crate) fn request_patch(&self, build_id: &str, payload: Vec<u8>) -> io::Result<()> {
    let (response_tx, response_rx) = mpsc::channel();
    self.command_tx.send(PatchServerCommand::ApplyPatch {
      build_id: String::from(build_id),
      payload,
      response_tx,
    })
    .map_err(|_| io::Error::new(ErrorKind::BrokenPipe, "patch server is not running"))?;

    response_rx.recv().map_err(|_| {
      io::Error::new(ErrorKind::BrokenPipe, "patch server stopped before applying the patch")
    })?
  }

  pub(crate) fn active_process_id(&self) -> Option<u32> {
    self
      .active_process_id
      .lock()
      .ok()
      .and_then(|process_id| *process_id)
  }

  #[cfg(test)]
  fn next_event(&self, timeout: Duration) -> Option<PatchServerEvent> {
    self.events_rx.recv_timeout(timeout).ok()
  }
}

impl Drop for PatchServer {
  fn drop(&mut self) {
    if let Some(shutdown_tx) = self.shutdown_tx.take() {
      let _ = shutdown_tx.send(());
    }

    if let Some(worker) = self.worker.take() {
      let _ = worker.join();
    }
  }
}

fn run_patch_server(
  listener: &TcpListener,
  expected_session: &SessionManifest,
  active_process_id: &Arc<Mutex<Option<u32>>>,
  command_rx: &Receiver<PatchServerCommand>,
  shutdown_rx: &Receiver<()>,
  events_tx: Option<&Sender<PatchServerEvent>>,
) {
  let mut active_runtime = None;

  loop {
    match shutdown_rx.try_recv() {
      Ok(()) | Err(mpsc::TryRecvError::Disconnected) => return,
      Err(mpsc::TryRecvError::Empty) => {}
    }

    drain_commands(command_rx, &mut active_runtime, active_process_id, events_tx);

    match listener.accept() {
      Ok((stream, _peer_addr)) => {
        if let Ok(Some(runtime)) = handle_connection(stream, expected_session, events_tx) {
          active_runtime = Some(runtime);
          set_active_process_id(active_process_id, active_runtime.as_ref().map(|runtime| runtime.process_id));
        }
      }
      Err(error) if error.kind() == ErrorKind::WouldBlock => {
        thread::sleep(Duration::from_millis(25));
      }
      Err(_error) => return,
    }
  }
}

fn handle_connection(
  mut stream: TcpStream,
  expected_session: &SessionManifest,
  events_tx: Option<&Sender<PatchServerEvent>>,
) -> io::Result<Option<ActiveRuntime>> {
  stream.set_nonblocking(false)?;
  let frame = read_frame(&mut stream)?;
  let message = decode_message(&frame)
    .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;

  match message {
    RuntimeMessage::Hello(hello) => {
      if let Err(message) = validate_hello(&hello, expected_session) {
        write_message(&mut stream, &RuntimeMessage::Error { message })?;
        return Ok(None);
      }

      println!(
        "thebe: hotpatch runtime connected — session {}, pid {}, build {}",
        hello.session_id, hello.process_id, hello.build_id
      );

      if let Some(events_tx) = events_tx {
        let _ = events_tx.send(PatchServerEvent::HelloAccepted(hello.clone()));
      }
      Ok(Some(ActiveRuntime {
        process_id: hello.process_id,
        stream,
      }))
    }
    _ => write_message(
      &mut stream,
      &RuntimeMessage::Error {
        message: String::from("unexpected runtime message during handshake"),
      },
    )
    .map(|()| None),
  }
}

fn drain_commands(
  command_rx: &Receiver<PatchServerCommand>,
  active_runtime: &mut Option<ActiveRuntime>,
  active_process_id: &Arc<Mutex<Option<u32>>>,
  events_tx: Option<&Sender<PatchServerEvent>>,
) {
  loop {
    match command_rx.try_recv() {
      Ok(command) => apply_command(command, active_runtime, active_process_id, events_tx),
      Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => return,
    }
  }
}

fn apply_command(
  command: PatchServerCommand,
  active_runtime: &mut Option<ActiveRuntime>,
  active_process_id: &Arc<Mutex<Option<u32>>>,
  events_tx: Option<&Sender<PatchServerEvent>>,
) {
  let Some(runtime) = active_runtime.as_mut() else {
    if let PatchServerCommand::ApplyPatch { response_tx, .. } = command {
      let _ = response_tx.send(Err(io::Error::new(
        ErrorKind::NotConnected,
        "no hotpatch runtime is connected",
      )));
    }
    return;
  };

  let result = match command {
    PatchServerCommand::ShutdownRuntime => {
      write_message(&mut runtime.stream, &RuntimeMessage::Shutdown)
    }
    PatchServerCommand::ApplyPatch {
      build_id,
      payload,
      response_tx,
    } => {
      let patch_result = write_message(
        &mut runtime.stream,
        &RuntimeMessage::Patch {
          build_id: build_id.clone(),
          payload,
        },
      )
      .and_then(|()| await_patch_applied(runtime, &build_id, events_tx));
      let response = patch_result
        .as_ref()
        .map(|_| ())
        .map_err(|error| io::Error::new(error.kind(), error.to_string()));
      let _ = response_tx.send(response);
      patch_result
    }
    PatchServerCommand::RestartRuntime(reason) => write_message(
      &mut runtime.stream,
      &RuntimeMessage::RestartRequired { reason },
    ),
  };

  if result.is_err() {
    *active_runtime = None;
    set_active_process_id(active_process_id, None);
  }
}

fn await_patch_applied(
  runtime: &mut ActiveRuntime,
  expected_build_id: &str,
  events_tx: Option<&Sender<PatchServerEvent>>,
) -> io::Result<()> {
  runtime.stream.set_read_timeout(Some(PATCH_ACK_TIMEOUT))?;

  let result = read_frame(&mut runtime.stream)
    .and_then(|frame| {
      decode_message(&frame).map_err(|error| io::Error::new(ErrorKind::InvalidData, error))
    })
    .and_then(|message| match message {
      RuntimeMessage::PatchApplied { build_id } if build_id == expected_build_id => {
        if let Some(events_tx) = events_tx {
          let _ = events_tx.send(PatchServerEvent::PatchApplied(build_id));
        }
        Ok(())
      }
      RuntimeMessage::PatchApplied { build_id } => Err(io::Error::new(
        ErrorKind::InvalidData,
        format!(
          "unexpected patch acknowledgement for build {build_id}; expected {expected_build_id}"
        ),
      )),
      RuntimeMessage::Error { message } => Err(io::Error::other(format!(
        "runtime rejected patch {expected_build_id}: {message}"
      ))),
      other => Err(io::Error::new(
        ErrorKind::InvalidData,
        format!("unexpected runtime response while awaiting patch acknowledgement: {other:?}"),
      )),
    });

  let _ = runtime.stream.set_read_timeout(None);
  result
}

fn set_active_process_id(active_process_id: &Arc<Mutex<Option<u32>>>, process_id: Option<u32>) {
  if let Ok(mut active_process_id) = active_process_id.lock() {
    *active_process_id = process_id;
  }
}

fn validate_hello(hello: &RuntimeHello, expected_session: &SessionManifest) -> Result<(), String> {
  if hello.protocol_version != expected_session.protocol_version {
    return Err(format!(
      "protocol version mismatch: expected {}, received {}",
      expected_session.protocol_version, hello.protocol_version
    ));
  }

  if hello.session_id != expected_session.session_id {
    return Err(format!(
      "session mismatch: expected {}, received {}",
      expected_session.session_id, hello.session_id
    ));
  }

  Ok(())
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

fn write_message(stream: &mut TcpStream, message: &RuntimeMessage) -> io::Result<()> {
  let frame = encode_message(message)
    .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
  stream.write_all(&frame)?;
  stream.flush()
}

#[cfg(test)]
mod tests {
  use super::{
    FrameError, PROTOCOL_VERSION, PatchServer, PatchServerEvent, RuntimeHello,
    RuntimeMessage, decode_message, encode_message, read_frame,
  };
  use crate::hotpatch::session::SessionManifest;
  use std::io::Write;
  use std::net::{SocketAddr, TcpStream};
  use std::time::Duration;

  const TEST_CREATED_AT: u128 = 42;

  #[test]
  fn encode_message_should_round_trip_hello_frames() {
    let message = RuntimeMessage::Hello(RuntimeHello {
      protocol_version: PROTOCOL_VERSION,
      session_id: String::from("session-1"),
      process_id: 42,
      build_id: String::from("build-abc"),
      aslr_reference: 7,
    });

    let frame = encode_message(&message).expect("hello frame should encode");
    let decoded = decode_message(&frame).expect("hello frame should decode");

    assert_eq!(decoded, message);
  }

  #[test]
  fn decode_message_should_reject_truncated_header() {
    let error = decode_message(&[0_u8, 1, 2]).expect_err("short header must fail");

    assert!(matches!(error, FrameError::TruncatedHeader));
  }

  #[test]
  fn decode_message_should_reject_truncated_payload() {
    let error = decode_message(&[0, 0, 0, 5, b'{', b'}'])
      .expect_err("short payload must fail");

    assert!(matches!(error, FrameError::TruncatedPayload));
  }

  #[test]
  fn decode_message_should_reject_trailing_bytes() {
    let message = RuntimeMessage::Shutdown;
    let mut frame = encode_message(&message).expect("shutdown frame should encode");
    frame.push(0);

    let error = decode_message(&frame).expect_err("extra bytes must fail");

    assert!(matches!(error, FrameError::TrailingBytes));
  }

  #[test]
  fn patch_server_should_record_matching_hello_frames() {
    let (listener, server_addr) = PatchServer::bind().expect("listener bind should succeed");
    let session = test_session(server_addr, "session-1");
    let server = PatchServer::spawn(listener, session.clone())
      .expect("patch server startup should succeed");

    let hello = RuntimeHello {
      protocol_version: PROTOCOL_VERSION,
      session_id: session.session_id.clone(),
      process_id: 99,
      build_id: String::from("build-1"),
      aslr_reference: 7,
    };

    send_message(
      server.local_addr(),
      &RuntimeMessage::Hello(hello.clone()),
    );

    let event = server
      .next_event(Duration::from_millis(500))
      .expect("matching hello should be recorded");

    assert_eq!(event, PatchServerEvent::HelloAccepted(hello));
  }

  #[test]
  fn patch_server_should_reply_with_error_for_session_mismatches() {
    let (listener, server_addr) = PatchServer::bind().expect("listener bind should succeed");
    let session = test_session(server_addr, "session-1");
    let server = PatchServer::spawn(listener, session)
      .expect("patch server startup should succeed");

    let mut stream = TcpStream::connect(server.local_addr())
      .expect("client should connect to patch server");
    let frame = encode_message(&RuntimeMessage::Hello(RuntimeHello {
      protocol_version: PROTOCOL_VERSION,
      session_id: String::from("other-session"),
      process_id: 99,
      build_id: String::from("build-1"),
      aslr_reference: 7,
    }))
    .expect("hello frame should encode");
    stream.write_all(&frame).expect("hello frame should send");

    let response = decode_message(&read_frame(&mut stream).expect("error frame should read"))
      .expect("error frame should decode");

    assert!(matches!(
      response,
      RuntimeMessage::Error { message } if message.contains("session mismatch")
    ));
  }

  #[test]
  fn patch_server_should_send_shutdown_to_connected_runtime() {
    let (listener, server_addr) = PatchServer::bind().expect("listener bind should succeed");
    let session = test_session(server_addr, "session-1");
    let server = PatchServer::spawn(listener, session.clone())
      .expect("patch server startup should succeed");

    let mut stream = TcpStream::connect(server.local_addr())
      .expect("client should connect to patch server");
    stream
      .set_read_timeout(Some(Duration::from_millis(500)))
      .expect("client read timeout should set");
    let frame = encode_message(&RuntimeMessage::Hello(RuntimeHello {
      protocol_version: PROTOCOL_VERSION,
      session_id: session.session_id,
      process_id: 99,
      build_id: String::from("build-1"),
      aslr_reference: 7,
    }))
    .expect("hello frame should encode");
    stream.write_all(&frame).expect("hello frame should send");

    assert!(wait_for_active_runtime_process_id(&server, 99, Duration::from_millis(500)));

    server.request_shutdown();

    let response = decode_message(&read_frame(&mut stream).expect("shutdown frame should read"))
      .expect("shutdown frame should decode");

    assert_eq!(response, RuntimeMessage::Shutdown);
  }

  #[test]
  fn patch_server_should_send_restart_required_to_connected_runtime() {
    let (listener, server_addr) = PatchServer::bind().expect("listener bind should succeed");
    let session = test_session(server_addr, "session-1");
    let server = PatchServer::spawn(listener, session.clone())
      .expect("patch server startup should succeed");

    let mut stream = TcpStream::connect(server.local_addr())
      .expect("client should connect to patch server");
    stream
      .set_read_timeout(Some(Duration::from_millis(500)))
      .expect("client read timeout should set");
    let frame = encode_message(&RuntimeMessage::Hello(RuntimeHello {
      protocol_version: PROTOCOL_VERSION,
      session_id: session.session_id,
      process_id: 99,
      build_id: String::from("build-1"),
      aslr_reference: 7,
    }))
    .expect("hello frame should encode");
    stream.write_all(&frame).expect("hello frame should send");

    assert!(wait_for_active_runtime_process_id(&server, 99, Duration::from_millis(500)));

    server.request_restart("application entry point changed");

    let response = decode_message(&read_frame(&mut stream).expect("restart frame should read"))
      .expect("restart frame should decode");

    assert_eq!(
      response,
      RuntimeMessage::RestartRequired {
        reason: String::from("application entry point changed"),
      }
    );
  }

  #[test]
  fn patch_server_should_wait_for_patch_acknowledgements() {
    let (listener, server_addr) = PatchServer::bind().expect("listener bind should succeed");
    let session = test_session(server_addr, "session-1");
    let server = PatchServer::spawn(listener, session.clone())
      .expect("patch server startup should succeed");

    let mut stream = TcpStream::connect(server.local_addr())
      .expect("client should connect to patch server");
    stream
      .set_read_timeout(Some(Duration::from_millis(500)))
      .expect("client read timeout should set");
    let frame = encode_message(&RuntimeMessage::Hello(RuntimeHello {
      protocol_version: PROTOCOL_VERSION,
      session_id: session.session_id,
      process_id: 99,
      build_id: String::from("build-1"),
      aslr_reference: 7,
    }))
    .expect("hello frame should encode");
    stream.write_all(&frame).expect("hello frame should send");

    assert!(wait_for_active_runtime_process_id(&server, 99, Duration::from_millis(500)));
    let hello_event = server
      .next_event(Duration::from_millis(500))
      .expect("matching hello should be recorded");
    assert!(matches!(hello_event, PatchServerEvent::HelloAccepted(_)));

    let runtime = std::thread::spawn(move || {
      let response = decode_message(&read_frame(&mut stream).expect("patch frame should read"))
        .expect("patch frame should decode");

      assert_eq!(
        response,
        RuntimeMessage::Patch {
          build_id: String::from("patch-1"),
          payload: vec![1, 2, 3],
        }
      );

      let ack = encode_message(&RuntimeMessage::PatchApplied {
        build_id: String::from("patch-1"),
      })
      .expect("ack frame should encode");
      stream.write_all(&ack).expect("ack frame should send");
    });

    server
      .request_patch("patch-1", vec![1, 2, 3])
      .expect("patch acknowledgement should complete the request");
    runtime.join().expect("runtime thread should join");

    let event = server
      .next_event(Duration::from_millis(500))
      .expect("patch acknowledgement should be recorded");
    assert_eq!(event, PatchServerEvent::PatchApplied(String::from("patch-1")));
  }

  #[test]
  fn patch_server_should_expose_the_connected_runtime_process_id() {
    let (listener, server_addr) = PatchServer::bind().expect("listener bind should succeed");
    let session = test_session(server_addr, "session-1");
    let server = PatchServer::spawn(listener, session.clone())
      .expect("patch server startup should succeed");

    send_message(
      server.local_addr(),
      &RuntimeMessage::Hello(RuntimeHello {
        protocol_version: PROTOCOL_VERSION,
        session_id: session.session_id,
        process_id: 321,
        build_id: String::from("build-1"),
        aslr_reference: 7,
      }),
    );

    let event = server
      .next_event(Duration::from_millis(500))
      .expect("matching hello should be recorded");
    assert!(matches!(event, PatchServerEvent::HelloAccepted(_)));
    assert_eq!(server.active_process_id(), Some(321));
  }

  fn send_message(server_addr: SocketAddr, message: &RuntimeMessage) {
    let mut stream = TcpStream::connect(server_addr)
      .expect("client should connect to patch server");
    let frame = encode_message(message).expect("message should encode");
    stream.write_all(&frame).expect("message should send");
  }

  fn wait_for_active_runtime_process_id(
    server: &PatchServer,
    process_id: u32,
    timeout: Duration,
  ) -> bool {
    let started = std::time::Instant::now();

    while started.elapsed() < timeout {
      if server.active_process_id() == Some(process_id) {
        return true;
      }

      std::thread::sleep(Duration::from_millis(25));
    }

    false
  }

  fn test_session(server_addr: SocketAddr, session_id: &str) -> SessionManifest {
    SessionManifest {
      protocol_version: PROTOCOL_VERSION,
      session_id: String::from(session_id),
      process_id: 1,
      created_at_unix_ms: TEST_CREATED_AT,
      server_addr,
      browser_addr: None,
    }
  }
}
