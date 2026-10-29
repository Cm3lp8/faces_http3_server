//! A real downstream, immediate callback output, and an HTTP acknowledgement.
use crate::{test_support::TestSessions, *};
use quiche::h3::{self, Header, NameValue};
use std::{
    io::ErrorKind,
    net::{SocketAddr, UdpSocket},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use stream_framer::{FrameParser, ParsedStreamData};

#[derive(Clone)]
struct State {
    sessions: StreamSessions<TestSessions>,
    target: Arc<Mutex<Option<(Vec<u8>, u64)>>>,
    called: PathBuf,
}
fn send_frame(state: &State, bytes: &[u8]) -> Result<(), ()> {
    let target = state.target.lock().unwrap().clone().unwrap();
    state
        .sessions
        .get_stream_session(|session| {
            session.send_one_message_on_stream((&target.0, target.1), |ids| {
                Ok((
                    crate::prelude::StreamMessageCapsule::new(&(), ids.0, ids.1),
                    bytes.to_vec(),
                ))
            })
        })
        .map(|_| ())
        .map_err(|_| ())
}
fn stream_callback(
    event: FinishedEvent,
    _: &StreamSessions<TestSessions>,
    state: &State,
) -> Result<(), ()> {
    std::fs::write(&state.called, b"called").unwrap();
    *state.target.lock().unwrap() = Some((event.scid().to_vec(), event.stream_id()));
    if std::env::var("FACES_STREAM_CASE").as_deref() == Ok("callback-error") {
        return Err(());
    }
    send_frame(state, b"first")
}
#[test]
#[ignore = "owned subprocess entry point"]
fn stream_server_child() {
    let address: SocketAddr = std::env::var("FACES_STREAM_ADDRESS")
        .unwrap()
        .parse()
        .unwrap();
    assert!(address.ip().is_loopback());
    let root = PathBuf::from(std::env::var("FACES_STREAM_ROOT").unwrap());
    env_logger::init();
    let sessions = StreamSessions::new();
    let state = State {
        sessions: sessions.clone(),
        target: Arc::new(Mutex::new(None)),
        called: root.join("called"),
    };
    let mut router = RouteManager::new_with_app_state(state).with_stream_sessions(&sessions);
    let auth = router.middleware(&|headers: Vec<Header>, _: &State| {
        if headers
            .iter()
            .any(|h| h.name() == b"authorization" && h.value() == b"local-fixture")
        {
            MiddleWareFlow::Continue(headers)
        } else {
            MiddleWareFlow::Abort(ErrorResponse::Error403(None))
        }
    });
    let stream = router.to_stream_handler(&stream_callback);
    router.down_stream("/stream", (), |r| {
        r.middleware(&auth);
        r.stream_handler(&stream);
    });
    let ack = router.handler(&|event, state: &State, _| {
        send_frame(state, b"second").unwrap();
        Response::ok_200_with_data(event, b"ack".to_vec())
    });
    router.route_get("/ack", RouteConfig::default(), |r| {
        r.handler(&ack);
    });
    let tls = PathBuf::from(std::env::var("FACES_STREAM_TLS").unwrap());
    Http3Server::new(&address.to_string())
        .add_cert_path(tls.join("cert.pem"))
        .add_key_path(tls.join("key.pem"))
        .set_file_storage_path(root.join("staging"))
        .try_run_blocking(router)
        .unwrap();
}
struct OwnedServer {
    child: Child,
    root: PathBuf,
}
impl Drop for OwnedServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
struct WireClient {
    socket: UdpSocket,
    conn: quiche::Connection,
    http: Option<h3::Connection>,
}
impl WireClient {
    fn new(address: SocketAddr, tls: &std::path::Path) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let mut cfg = quiche::Config::new(quiche::PROTOCOL_VERSION).unwrap();
        cfg.load_verify_locations_from_file(tls.join("cert.pem").to_str().unwrap())
            .unwrap();
        cfg.set_application_protos(h3::APPLICATION_PROTOCOL)
            .unwrap();
        cfg.set_max_idle_timeout(5000);
        cfg.set_max_send_udp_payload_size(1350);
        cfg.set_initial_max_data(1_000_000);
        cfg.set_initial_max_stream_data_bidi_local(100_000);
        cfg.set_initial_max_stream_data_bidi_remote(100_000);
        cfg.set_initial_max_stream_data_uni(100_000);
        cfg.set_initial_max_streams_bidi(20);
        cfg.set_initial_max_streams_uni(10);
        let id = uuid::Uuid::now_v7();
        let conn = quiche::connect(
            Some("localhost"),
            &quiche::ConnectionId::from_ref(id.as_bytes()),
            socket.local_addr().unwrap(),
            address,
            &mut cfg,
        )
        .unwrap();
        Self {
            socket,
            conn,
            http: None,
        }
    }
    fn pump(&mut self) {
        let mut buf = [0; 65535];
        loop {
            match self.socket.recv_from(&mut buf) {
                Ok((len, from)) => {
                    self.conn
                        .recv(
                            &mut buf[..len],
                            quiche::RecvInfo {
                                from,
                                to: self.socket.local_addr().unwrap(),
                            },
                        )
                        .unwrap();
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => panic!("UDP: {e}"),
            }
        }
        if self.conn.timeout().is_some_and(|t| t.is_zero()) {
            self.conn.on_timeout();
        }
        assert!(
            !self.conn.is_closed(),
            "connection closed: {:?}",
            self.conn.peer_error()
        );
        if self.conn.is_established() && self.http.is_none() {
            self.http = Some(
                h3::Connection::with_transport(&mut self.conn, &h3::Config::new().unwrap())
                    .unwrap(),
            );
        }
        self.flush();
    }
    fn flush(&mut self) {
        let mut buf = [0; 1350];
        while let Ok((len, info)) = self.conn.send(&mut buf) {
            self.socket.send_to(&buf[..len], info.to).unwrap();
        }
    }
    fn request(&mut self, path: &[u8], authorized: bool) -> u64 {
        let mut headers = vec![
            Header::new(b":method", b"GET"),
            Header::new(b":scheme", b"https"),
            Header::new(b":authority", b"localhost"),
            Header::new(b":path", path),
        ];
        if authorized {
            headers.push(Header::new(b"authorization", b"local-fixture"));
        }
        let id = self
            .http
            .as_mut()
            .unwrap()
            .send_request(&mut self.conn, &headers, true)
            .unwrap();
        self.flush();
        id
    }
}
fn run_case(case: &str, expected: u16) {
    let base = PathBuf::from(
        std::env::var("FACES_TEST_ARTIFACT_DIR").expect("explicit local test artifact root"),
    );
    let root = base
        .join("tmp")
        .join(format!("stream-open-{case}-{}", uuid::Uuid::now_v7()));
    assert!(!root.exists(), "use an unused owned fixture directory");
    std::fs::create_dir_all(root.join("staging")).unwrap();
    let tls = root.clone();
    assert!(Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=localhost",
            "-addext",
            "subjectAltName=DNS:localhost"
        ])
        .arg("-keyout")
        .arg(tls.join("key.pem"))
        .arg("-out")
        .arg(tls.join("cert.pem"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success());
    let reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let log = std::fs::File::create(root.join("server.log")).unwrap();
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "stream_open_tests::stream_server_child",
            "--ignored",
            "--nocapture",
        ])
        .env_clear()
        .env("RUST_LOG", "info")
        .env("FACES_STREAM_ADDRESS", address.to_string())
        .env("FACES_STREAM_ROOT", &root)
        .env("FACES_STREAM_TLS", &tls)
        .env("FACES_STREAM_CASE", case)
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap();
    let mut server = OwnedServer { child, root };
    let mut client = WireClient::new(address, &tls);
    let deadline = Instant::now() + Duration::from_secs(5);
    while client.http.is_none() {
        assert!(Instant::now() < deadline, "handshake deadline");
        assert!(server.child.try_wait().unwrap().is_none(), "server exited");
        client.pump();
        std::thread::sleep(Duration::from_millis(2));
    }
    let path = if case == "missing" {
        b"/missing".as_slice()
    } else {
        b"/stream".as_slice()
    };
    let stream = client.request(path, case != "rejected");
    let mut final_status = None;
    let mut payload = Vec::new();
    let mut ack = None;
    let mut ack_finished = false;
    let mut ack_status = None;
    let mut completed_at = None;
    loop {
        assert!(Instant::now() < deadline, "stream deadline case={case}");
        client.pump();
        let http = client.http.as_mut().unwrap();
        loop {
            match http.poll(&mut client.conn) {
                Ok((id, h3::Event::Headers { list, .. })) => {
                    let status = list
                        .iter()
                        .find(|h| h.name() == b":status")
                        .map(|h| {
                            std::str::from_utf8(h.value())
                                .unwrap()
                                .parse::<u16>()
                                .unwrap()
                        })
                        .unwrap();
                    if Some(id) == ack && status >= 200 {
                        ack_status = Some(status);
                    }
                    if id == stream && status >= 200 {
                        assert_eq!(status, expected);
                        assert!(
                            final_status.replace(status).is_none(),
                            "duplicate final headers"
                        );
                    }
                }
                Ok((id, h3::Event::Data)) => {
                    if id == stream {
                        assert_eq!(final_status, Some(200), "DATA arrived before final 200");
                    }
                    let mut buf = [0; 4096];
                    while let Ok(len) = http.recv_body(&mut client.conn, id, &mut buf) {
                        if id == stream {
                            payload.extend_from_slice(&buf[..len]);
                        }
                    }
                }
                Ok((id, h3::Event::Finished)) if id == stream => {
                    assert_ne!(expected, 200, "downstream closed prematurely");
                    assert_eq!(final_status, Some(expected));
                    assert!(!server.root.join("called").exists());
                    return;
                }
                Ok((id, h3::Event::Finished)) if Some(id) == ack => {
                    ack_finished = true;
                }
                Ok((id, h3::Event::Reset(code))) if id == stream && case == "callback-error" => {
                    assert_eq!(code, 0x102);
                    assert!(server.root.join("called").exists());
                    return;
                }
                Ok((_, h3::Event::Reset(code))) => panic!("unexpected reset {code}"),
                Ok(_) => {}
                Err(h3::Error::Done) => break,
                Err(e) => panic!("h3 poll {e:?}"),
            }
        }
        let frames: Vec<_> = payload
            .clone()
            .parse_frame_header(None, None)
            .unwrap()
            .into_iter()
            .filter_map(|v| {
                if let ParsedStreamData::Completed(data) = v {
                    Some(data)
                } else {
                    None
                }
            })
            .collect();
        if !frames.is_empty() && ack.is_none() {
            assert_eq!(frames[0], b"first");
            ack = Some(client.request(b"/ack", false));
        }
        assert!(frames.len() <= 2, "duplicate stream delivery");
        if frames.len() == 2 && ack_finished {
            if case == "blocked" {
                assert!(server.root.join("blocked").exists());
            }
            assert_eq!(frames[1], b"second");
            assert_eq!(ack_status, Some(200));
            assert!(!client.conn.stream_finished(stream));
            let completed_at = completed_at.get_or_insert_with(Instant::now);
            if completed_at.elapsed() >= Duration::from_millis(50) {
                return;
            }
        }
        client.flush();
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[test]
#[ignore = "requires explicit local fixture root and loopback"]
fn accepted_stream_final_headers_precede_immediate_data_and_ack() {
    run_case("accepted", 200);
}
#[test]
#[ignore = "requires explicit local fixture root and loopback"]
fn rejected_stream_does_not_call_handler() {
    run_case("rejected", 403);
}
#[test]
#[ignore = "requires explicit local fixture root and loopback"]
fn unknown_stream_does_not_call_handler() {
    run_case("missing", 404);
}

static OPENING_ATTEMPTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub(crate) fn block_opening_once() -> bool {
    if std::env::var("FACES_STREAM_CASE").as_deref() != Ok("blocked") {
        return false;
    }
    let first = OPENING_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
    if first {
        let root = PathBuf::from(std::env::var("FACES_STREAM_ROOT").unwrap());
        std::fs::write(root.join("blocked"), b"opening blocked once").unwrap();
    }
    first
}
pub(crate) fn opening_was_injected() -> bool {
    OPENING_ATTEMPTS.load(std::sync::atomic::Ordering::SeqCst) == 1
}
#[test]
#[ignore = "requires explicit local fixture root and loopback"]
fn blocked_opening_resumes_before_data_without_duplication() {
    run_case("blocked", 200);
}
#[test]
#[ignore = "requires explicit local fixture root and loopback"]
fn callback_error_resets_stream_instead_of_silent_success() {
    run_case("callback-error", 200);
}
