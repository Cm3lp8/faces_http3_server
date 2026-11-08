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
    let case = std::env::var("FACES_STREAM_CASE").unwrap();
    if case == "blocked-callback-error" {
        // Ensure an opening attempt really is pending before the callback fails.
        let deadline = Instant::now() + Duration::from_secs(1);
        while OPENING_ATTEMPTS.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "opening was never attempted");
            std::thread::sleep(Duration::from_millis(1));
        }
        send_frame(state, b"must-not-arrive")?;
        return Err(());
    }
    if case == "callback-error" {
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
    let case = std::env::var("FACES_STREAM_CASE").unwrap();
    if case == "downlink-loss" {
        let media = router
            .handler(&|event, _: &State, _| Response::ok_200_with_data(event, sample_media()));
        router.route_get("/sample-media", RouteConfig::default(), |route| {
            route.handler(&media);
        });
    }
    if matches!(
        case.as_str(),
        "body-security" | "preheader-security" | "file-cancellation" | "priority-security"
    ) {
        router = router.with_body_limits(BodyLimits {
            default_body_bytes: 8,
            server_requests: if matches!(case.as_str(), "preheader-security" | "priority-security")
            {
                3
            } else {
                512
            },
            max_connections: if case == "preheader-security" { 2 } else { 128 },
            receive_timeout: Duration::from_millis(if case == "priority-security" {
                3000
            } else {
                600
            }),
            ..BodyLimits::default()
        });
        let file_auth = router.middleware(&|headers, state: &State| {
            let root = state.called.parent().unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !root.join("allow-upload").exists() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(2));
            }
            MiddleWareFlow::Continue(headers)
        });
        let file = router.handler(&|event, _, _| {
            let writer = event.file_writer().unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while !writer.is_file_written().unwrap() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            let bytes = std::fs::read(event.get_file_path().unwrap()).unwrap();
            Response::ok_200_with_data(event, bytes)
        });
        router.route_post(
            "/file",
            RouteConfig::new(DataManagement::Storage(BodyStorage::File)),
            |route| {
                route.middleware(&file_auth);
                route.handler(&file);
            },
        );
        let file_failure = router.handler(&|event, state: &State, _| {
            let path = event.get_file_path().unwrap();
            if matches!(event.path(), "/publish-panic" | "/publish-reject") {
                let deadline = Instant::now() + Duration::from_secs(2);
                while !event.file_writer().unwrap().is_file_written().unwrap() {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
                let root = state.called.parent().unwrap();
                std::fs::rename(path, root.join("published")).unwrap();
                std::fs::write(path, b"different file").unwrap();
            }
            if matches!(event.path(), "/file-reject" | "/publish-reject") {
                return Response::error_422(event, b"invalid upload".to_vec());
            }
            panic!("file handler unwind");
        });
        for path in [
            "/file-panic",
            "/publish-panic",
            "/file-reject",
            "/publish-reject",
        ] {
            router.route_post(
                path,
                RouteConfig::new(DataManagement::Storage(BodyStorage::File)),
                |route| {
                    route.handler(&file_failure);
                },
            );
        }
        let wait_for_cancel = router.handler(&|event, state: &State, _| {
            let root = state.called.parent().unwrap();
            std::fs::write(root.join("file-dispatched"), b"yes").unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !root.join("return-from-handler").exists() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(2));
            }
            Response::ok_200(event)
        });
        router.route_post(
            "/cancel-file",
            RouteConfig::new(DataManagement::Storage(BodyStorage::File)),
            |route| {
                route.handler(&wait_for_cancel);
            },
        );
        let panic_middleware = router.middleware(&|_, state: &State| {
            use std::io::Write;
            let mut visits = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(state.called.parent().unwrap().join("middleware-visits"))
                .unwrap();
            visits.write_all(b"x").unwrap();
            panic!("middleware unwind");
        });
        let unreachable_handler = router.handler(&|event, state: &State, _| {
            std::fs::write(&state.called, b"called").unwrap();
            Response::ok_200(event)
        });
        router.route_get("/middleware-panic", RouteConfig::default(), |route| {
            route.middleware(&panic_middleware);
            route.handler(&unreachable_handler);
        });
        let panic = router.handler(&|_, _, _| panic!("fixture handler unwind"));
        router.route_get("/panic", RouteConfig::default(), |r| {
            r.handler(&panic);
        });
        let echo = router.handler(&|event, _, _| {
            let body = event.as_body().to_vec();
            Response::ok_200_with_data(event, body)
        });
        router.route_post("/echo", RouteConfig::default(), |r| {
            r.handler(&echo);
        });
    }
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
        self.pump_allow_close();
        assert!(
            !self.conn.is_closed(),
            "connection closed: {:?}",
            self.conn.peer_error()
        );
    }
    fn pump_allow_close(&mut self) {
        self.pump_filtered(|_| true);
    }
    fn pump_filtered(&mut self, mut accept: impl FnMut(&[u8]) -> bool) {
        let mut buf = [0; 65535];
        loop {
            match self.socket.recv_from(&mut buf) {
                Ok((len, from)) => {
                    if !accept(&buf[..len]) {
                        continue;
                    }
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
fn start_server(case: &str) -> (OwnedServer, WireClient) {
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
        .env(
            "RUST_LOG",
            if case == "file-cancellation" {
                "info,faces_quic_server::server_init::quiche_http3_server=debug"
            } else {
                "info"
            },
        )
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
    (server, client)
}

fn connect_peer(first: &WireClient, root: &std::path::Path) -> WireClient {
    let address = first.conn.path_stats().next().unwrap().peer_addr;
    let mut peer = WireClient::new(address, root);
    let deadline = Instant::now() + Duration::from_secs(3);
    while peer.http.is_none() {
        assert!(Instant::now() < deadline);
        peer.pump();
        std::thread::sleep(Duration::from_millis(2));
    }
    peer
}
fn run_case(case: &str, expected: u16) {
    let (mut server, mut client) = start_server(case);
    let deadline = Instant::now() + Duration::from_secs(5);
    let path = if case == "missing" {
        b"/missing".as_slice()
    } else {
        b"/stream".as_slice()
    };
    let stream = client.request(path, case != "rejected");
    let reset_deadline = Instant::now() + Duration::from_secs(2);
    let mut final_status = None;
    let mut payload = Vec::new();
    let mut ack = None;
    let mut ack_finished = false;
    let mut ack_status = None;
    let mut completed_at = None;
    loop {
        assert!(Instant::now() < deadline, "stream deadline case={case}");
        if case == "blocked-callback-error" {
            assert!(
                Instant::now() < reset_deadline,
                "blocked opening prevented reset within two seconds"
            );
        }
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
                Ok((id, h3::Event::Reset(code)))
                    if id == stream
                        && matches!(case, "callback-error" | "blocked-callback-error") =>
                {
                    assert_eq!(code, 0x102);
                    assert!(server.root.join("called").exists());
                    if case == "blocked-callback-error" {
                        assert!(server.root.join("blocked").exists());
                        assert_eq!(final_status, None);
                        assert!(payload.is_empty());
                        assert!(ack.is_none());
                        assert!(Instant::now() < reset_deadline);
                    }
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
pub(crate) fn block_opening_for_test() -> bool {
    let case = std::env::var("FACES_STREAM_CASE").unwrap_or_default();
    if !matches!(case.as_str(), "blocked" | "blocked-callback-error") {
        return false;
    }
    let first = OPENING_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
    if first {
        let root = PathBuf::from(std::env::var("FACES_STREAM_ROOT").unwrap());
        std::fs::write(root.join("blocked"), b"opening blocked once").unwrap();
    }
    first || case == "blocked-callback-error"
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

#[test]
#[ignore = "requires explicit local fixture root and loopback"]
fn blocked_opening_callback_error_resets_without_final_headers_or_data() {
    run_case("blocked-callback-error", 200);
}

// Await a terminal response on this stream, retaining the same verified
// connection so a reset cannot masquerade as a process or connection failure.
fn terminal(client: &mut WireClient, stream: u64) -> Result<Vec<u8>, u64> {
    terminal_with_status(client, stream, b"200")
}

fn terminal_with_status(
    client: &mut WireClient,
    stream: u64,
    expected: &[u8],
) -> Result<Vec<u8>, u64> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut payload = Vec::new();
    let mut ok = false;
    loop {
        assert!(Instant::now() < deadline, "response deadline for {stream}");
        client.pump();
        let http = client.http.as_mut().unwrap();
        loop {
            match http.poll(&mut client.conn) {
                Ok((id, h3::Event::Reset(code))) if id == stream => return Err(code),
                Ok((id, h3::Event::Headers { list, .. })) if id == stream => {
                    if list
                        .iter()
                        .any(|h| h.name() == b":status" && h.value() == expected)
                    {
                        ok = true;
                    }
                }
                Ok((id, h3::Event::Data)) => {
                    let mut part = [0; 2048];
                    while let Ok(len) = http.recv_body(&mut client.conn, id, &mut part) {
                        if id == stream {
                            payload.extend_from_slice(&part[..len]);
                        }
                    }
                }
                Ok((id, h3::Event::Finished)) if id == stream => {
                    assert!(ok);
                    return Ok(payload);
                }
                Ok(_) => {}
                Err(h3::Error::Done) => break,
                Err(error) => panic!("poll failed: {error:?}"),
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn post_to(client: &mut WireClient, path: &[u8], length: Option<&[u8]>) -> u64 {
    let mut headers = vec![
        Header::new(b":method", b"POST"),
        Header::new(b":scheme", b"https"),
        Header::new(b":authority", b"localhost"),
        Header::new(b":path", path),
    ];
    if let Some(length) = length {
        headers.push(Header::new(b"content-length", length));
    }
    client
        .http
        .as_mut()
        .unwrap()
        .send_request(&mut client.conn, &headers, false)
        .unwrap()
}

#[test]
#[ignore = "requires explicit local fixture root and loopback"]
fn constrained_bodies_and_panicking_handlers_leave_the_connection_usable() {
    let (mut server, mut client) = start_server("body-security");
    for _ in 0..5 {
        let id = post_to(&mut client, b"/echo", Some(b"9"));
        assert_eq!(terminal(&mut client, id), Err(0x107));
    }
    let id = post_to(&mut client, b"/echo", None);
    client
        .http
        .as_mut()
        .unwrap()
        .send_body(&mut client.conn, id, b"1234", false)
        .unwrap();
    client.flush();
    client
        .http
        .as_mut()
        .unwrap()
        .send_body(&mut client.conn, id, b"56789", true)
        .unwrap();
    assert_eq!(terminal(&mut client, id), Err(0x107));
    let id = post_to(&mut client, b"/echo", None);
    assert_eq!(terminal(&mut client, id), Err(0x107)); // absolute timeout without further DATA
    for _ in 0..10 {
        let id = client.request(b"/panic", false);
        assert_eq!(terminal(&mut client, id), Err(0x102));
    }
    let id = post_to(&mut client, b"/echo", Some(b"8"));
    client
        .http
        .as_mut()
        .unwrap()
        .send_body(&mut client.conn, id, b"12345678", true)
        .unwrap();
    assert_eq!(terminal(&mut client, id), Ok(b"12345678".to_vec()));
    assert!(server.child.try_wait().unwrap().is_none());
    let log = std::fs::read_to_string(server.root.join("server.log")).unwrap();
    let reports: Vec<_> = log
        .lines()
        .filter(|line| line.contains("http3_body_rejections "))
        .collect();
    let admission: Vec<_> = reports
        .iter()
        .filter(|line| line.contains("stage=admission "))
        .collect();
    assert_eq!(admission.len(), 3);
    for (line, expected) in admission.iter().zip([1, 2, 4]) {
        assert!(line.contains(&format!("stage_total={expected} ")));
    }
    for stage in ["receive", "expiration"] {
        assert!(reports
            .iter()
            .any(|line| line.contains(&format!("stage={stage} stage_total=1 "))));
    }
    assert!(reports
        .iter()
        .all(|line| line.contains(" retained_bytes=") && line.contains(" active_requests=")));
}

#[test]
#[ignore = "requires explicit local fixture root and loopback"]
fn file_prefix_reset_and_authorized_disk_stream_preserve_the_route() {
    let (server, mut client) = start_server("body-security");
    let id = post_to(&mut client, b"/file", None);
    client
        .http
        .as_mut()
        .unwrap()
        .send_body(&mut client.conn, id, b"012345678", true)
        .unwrap();
    assert_eq!(terminal(&mut client, id), Err(0x107));
    // Middleware finishing after this reset must not create a staging file.
    assert_eq!(
        std::fs::read_dir(server.root.join("staging"))
            .unwrap()
            .count(),
        0
    );
    std::fs::write(server.root.join("allow-upload"), b"yes").unwrap();
    let id = post_to(&mut client, b"/file", Some(b"16"));
    client
        .http
        .as_mut()
        .unwrap()
        .send_body(&mut client.conn, id, b"prefix", false)
        .unwrap();
    client.flush();
    let deadline = Instant::now() + Duration::from_millis(500);
    while std::fs::read_dir(server.root.join("staging"))
        .unwrap()
        .count()
        == 0
    {
        assert!(Instant::now() < deadline);
        client.pump();
        std::thread::sleep(Duration::from_millis(2));
    }
    client
        .http
        .as_mut()
        .unwrap()
        .send_body(&mut client.conn, id, b"0123456789", true)
        .unwrap();
    assert_eq!(terminal(&mut client, id), Ok(b"prefix0123456789".to_vec()));
    assert_eq!(
        std::fs::read_dir(server.root.join("staging"))
            .unwrap()
            .count(),
        1
    );
}

fn echo_byte(client: &mut WireClient) {
    let id = post_to(client, b"/echo", Some(b"1"));
    client
        .http
        .as_mut()
        .unwrap()
        .send_body(&mut client.conn, id, b"x", true)
        .unwrap();
    assert_eq!(terminal(client, id), Ok(b"x".to_vec()));
}

fn raw_resets(client: &mut WireClient, mut streams: Vec<u64>) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !streams.is_empty() {
        assert!(Instant::now() < deadline, "missing resets {streams:?}");
        client.pump();
        streams.retain(|&id| match client.conn.stream_recv(id, &mut [0; 1]) {
            Err(quiche::Error::StreamReset(0x107)) => false,
            Err(quiche::Error::Done) => true,
            other => panic!("unexpected incomplete stream result: {other:?}"),
        });
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
#[ignore = "requires explicit local fixture root and loopback"]
fn incomplete_headers_share_slots_expire_and_excessive_declaration_closes_only_its_connection() {
    let (server, mut first) = start_server("preheader-security");
    echo_byte(&mut first);
    let mut second = connect_peer(&first, &server.root);
    echo_byte(&mut second);
    let address = first.conn.path_stats().next().unwrap().peer_addr;
    let mut extra = WireClient::new(address, &server.root);
    let probe_until = Instant::now() + Duration::from_millis(120);
    while Instant::now() < probe_until {
        extra.pump();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        extra.http.is_none(),
        "third connection must wait for capacity"
    );
    // HEADERS of 32 KiB with only one payload byte. Three open streams
    // occupy all shared slots; the fourth is refused before H3 parsing.
    let fragment = [0x01, 0x80, 0x00, 0x80, 0x00, 0x00];
    for id in [8, 12] {
        first.conn.stream_send(id, &fragment, false).unwrap();
    }
    first.flush();
    second.conn.stream_send(8, &fragment, false).unwrap();
    second.flush();
    std::thread::sleep(Duration::from_millis(30));
    second.conn.stream_send(12, &fragment, false).unwrap();
    second.flush();
    raw_resets(&mut second, vec![12]);
    raw_resets(&mut first, vec![8, 12]);
    raw_resets(&mut second, vec![8]);
    echo_byte(&mut first);
    // Five bytes announce a nearly 16 MiB HEADERS payload. The upstream
    // pre-allocation guard closes the offender without waiting for a payload.
    first
        .conn
        .stream_send(16, &[0x01, 0x80, 0xff, 0xff, 0xfe], false)
        .unwrap();
    first.flush();
    let deadline = Instant::now() + Duration::from_secs(3);
    while first.conn.peer_error().is_none() {
        assert!(Instant::now() < deadline);
        first.pump_allow_close();
        std::thread::sleep(Duration::from_millis(2));
    }
    let error = first.conn.peer_error().unwrap();
    assert!(error.is_app);
    assert_eq!(error.error_code, 0x107);
    echo_byte(&mut second);
    let deadline = Instant::now() + Duration::from_secs(4);
    while extra.http.is_none() {
        assert!(Instant::now() < deadline, "connection slot not released");
        extra.pump();
        std::thread::sleep(Duration::from_millis(2));
    }
    echo_byte(&mut extra);
    let log = std::fs::read_to_string(server.root.join("server.log")).unwrap();
    assert!(log
        .lines()
        .any(|line| line.contains("stage=admission ") && line.contains("active_requests=3")));
    assert!(log.lines().any(|line| line.contains("stage=expiration ")));
    assert!(log.lines().any(|line| line.contains("stage=connection ")));
}

#[test]
#[ignore = "requires explicit local fixture root and loopback"]
fn panicking_middleware_and_file_handlers_reset_without_replay_or_unowned_deletion() {
    let (mut server, mut client) = start_server("body-security");
    for _ in 0..10 {
        let id = client.request(b"/middleware-panic", false);
        assert_eq!(terminal(&mut client, id), Err(0x102));
    }
    assert_eq!(
        std::fs::read(server.root.join("middleware-visits")).unwrap(),
        b"xxxxxxxxxx"
    );
    assert!(!server.root.join("called").exists());

    let staging = server.root.join("staging");
    let upload = |client: &mut WireClient, path| {
        let id = post_to(client, path, Some(b"4"));
        client
            .http
            .as_mut()
            .unwrap()
            .send_body(&mut client.conn, id, b"file", true)
            .unwrap();
        terminal(client, id)
    };
    assert_eq!(upload(&mut client, b"/file-panic"), Err(0x102));
    assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
    assert_eq!(upload(&mut client, b"/publish-panic"), Err(0x102));
    assert_eq!(
        std::fs::read(server.root.join("published")).unwrap(),
        b"file"
    );
    let remaining: Vec<_> = std::fs::read_dir(&staging)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(remaining.len(), 1);
    assert_eq!(std::fs::read(&remaining[0]).unwrap(), b"different file");
    assert_eq!(upload(&mut client, b"/echo"), Ok(b"file".to_vec()));
    assert!(server.child.try_wait().unwrap().is_none());
}

#[test]
#[ignore = "requires explicit local fixture root and loopback"]
fn disconnect_after_fin_cleans_staging_when_the_handler_returns() {
    let (mut server, mut client) = start_server("file-cancellation");
    let stream = post_to(&mut client, b"/cancel-file", Some(b"4"));
    client
        .http
        .as_mut()
        .unwrap()
        .send_body(&mut client.conn, stream, b"file", true)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(6);
    while !server.root.join("file-dispatched").exists() {
        assert!(Instant::now() < deadline, "file handler did not run");
        client.pump();
        std::thread::sleep(Duration::from_millis(2));
    }
    let staging = server.root.join("staging");
    assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 1);
    client.conn.close(true, 0x100, b"fixture complete").unwrap();
    client.flush();
    while !std::fs::read_to_string(server.root.join("server.log"))
        .unwrap()
        .contains("connection collected")
    {
        assert!(
            Instant::now() < deadline,
            "server did not collect closed connection"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    std::fs::write(server.root.join("return-from-handler"), b"yes").unwrap();
    while std::fs::read_dir(&staging).unwrap().count() != 0 {
        assert!(Instant::now() < deadline, "cancelled staging file retained");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(server.child.try_wait().unwrap().is_none());
}

fn priority(client: &mut WireClient, id: u64) {
    client
        .http
        .as_mut()
        .unwrap()
        .send_priority_update_for_request(&mut client.conn, id, &h3::Priority::new(3, false))
        .unwrap();
    client.flush();
}
fn wait_for_close(client: &mut WireClient, reason: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while client.conn.peer_error().is_none() {
        assert!(
            Instant::now() < deadline,
            "offending connection did not close"
        );
        client.pump_allow_close();
        std::thread::sleep(Duration::from_millis(2));
    }
    let error = client.conn.peer_error().unwrap();
    assert!(error.is_app);
    assert_eq!(error.error_code, 0x107);
    assert_eq!(error.reason, reason);
}
#[test]
#[ignore = "requires explicit local fixture root and loopback"]
fn priorities_for_future_requests_share_preheader_slots() {
    let (server, mut first) = start_server("preheader-security");
    echo_byte(&mut first);
    let mut second = connect_peer(&first, &server.root);
    echo_byte(&mut second);
    priority(&mut first, 4);
    echo_byte(&mut first); // The actual request takes over its reserved prefix.
    for id in [8, 12, 16] {
        priority(&mut first, id);
    }
    std::thread::sleep(Duration::from_millis(30));
    priority(&mut second, 4); // No request-stream bytes at all.
    wait_for_close(&mut second, b"priority admission limit");
    echo_byte(&mut first); // Reuse the announced slot on the surviving connection.
    let log = std::fs::read_to_string(server.root.join("server.log")).unwrap();
    assert!(log
        .lines()
        .any(|line| line.contains("stage=admission ") && line.contains("active_requests=3")));
}
#[test]
#[ignore = "requires explicit local fixture root and loopback"]
fn refusals_before_h3_parsing_count_towards_the_aborted_stream_limit() {
    let (server, mut first) = start_server("priority-security");
    echo_byte(&mut first);
    let mut second = connect_peer(&first, &server.root);
    echo_byte(&mut second);
    for id in [4, 8, 12] {
        priority(&mut first, id);
    }
    std::thread::sleep(Duration::from_millis(30));
    // The quota is exhausted before any of these request streams enter poll().
    for index in 1..32 {
        let id = index * 4;
        second.conn.stream_send(id, &[1, 0x40, 80], false).unwrap();
        second.flush();
        raw_resets(&mut second, vec![id]);
    }
    second.conn.stream_send(128, &[1, 0x40, 80], false).unwrap();
    second.flush();
    wait_for_close(&mut second, b"aborted stream limit");
    echo_byte(&mut first);
}

#[test]
#[ignore = "requires explicit local fixture root and loopback"]
fn ordinary_upload_rejection_cleans_only_the_original_stage_and_keeps_its_http_response() {
    let (server, mut client) = start_server("body-security");
    let staging = server.root.join("staging");
    for path in [b"/file-reject".as_slice(), b"/publish-reject"] {
        let id = post_to(&mut client, path, Some(b"4"));
        client
            .http
            .as_mut()
            .unwrap()
            .send_body(&mut client.conn, id, b"file", true)
            .unwrap();
        assert_eq!(
            terminal_with_status(&mut client, id, b"422"),
            Ok(b"invalid upload".to_vec())
        );
        if path == b"/file-reject" {
            assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
        }
    }
    assert_eq!(
        std::fs::read(server.root.join("published")).unwrap(),
        b"file"
    );
    let mut entries = std::fs::read_dir(&staging).unwrap();
    assert_eq!(
        std::fs::read(entries.next().unwrap().unwrap().path()).unwrap(),
        b"different file"
    );
    assert!(entries.next().is_none());
}

fn sample_media() -> Vec<u8> {
    (0..32 * 1024).map(|offset| (offset % 251) as u8).collect()
}

#[test]
#[ignore = "requires explicit local fixture root and loopback"]
fn get_32k_recovers_after_initial_downlink_loss() {
    let (_server, mut client) = start_server("downlink-loss");
    let stream = client.request(b"/sample-media", false);
    // Start the loss window at a payload-sized server datagram. The client
    // continues to run its QUIC timers and send packets during the outage.
    let start_deadline = Instant::now() + Duration::from_secs(3);
    let mut resume_at = None;
    while resume_at.is_none_or(|end| Instant::now() < end) {
        client.pump_filtered(|datagram| {
            if datagram.len() >= 1024 && resume_at.is_none() {
                resume_at = Some(Instant::now() + Duration::from_millis(450));
            }
            false
        });
        assert!(
            resume_at.is_some() || Instant::now() < start_deadline,
            "no payload-sized server datagram to drop"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    // terminal requires HTTP 200 and FIN. Verify every byte after retransmission.
    let body = terminal(&mut client, stream).expect("unexpected reset after loss");
    assert_eq!(body.len(), 32 * 1024);
    let expected = sample_media();
    assert!(
        body == expected,
        "recovered media differs from the response"
    );

    let control = client.request(b"/sample-media", false);
    let body = terminal(&mut client, control).expect("subsequent GET was reset");
    assert!(body == expected, "subsequent GET returned different media");
}
