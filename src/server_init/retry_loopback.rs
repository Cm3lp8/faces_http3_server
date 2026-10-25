//! Real UDP/QUIC/HTTP3 coverage with an owned, disposable loopback server.
use std::{
    io::ErrorKind,
    net::{SocketAddr, UdpSocket},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use quiche::h3::{self, Header, NameValue};

const CHILD_TEST: &str = "server_init::retry_loopback::retry_server_child";

#[test]
#[ignore = "subprocess entry point for retry_rejections_leave_http3_available"]
fn retry_server_child() {
    let Ok(address) = std::env::var("FACES_RETRY_TEST_ADDRESS") else {
        return;
    };
    let address: SocketAddr = address.parse().unwrap();
    assert!(address.ip().is_loopback());
    let root = PathBuf::from(std::env::var("FACES_RETRY_TEST_ROOT").unwrap());
    let mut router =
        crate::RouteManager::<_, crate::test_support::TestSessions>::new_with_app_state(());
    let handler = router
        .handler(&|event, _, _| crate::Response::ok_200_with_data(event, b"retry-ok".to_vec()));
    router.route_get("/retry-probe", crate::RouteConfig::default(), |route| {
        route.handler(&handler);
    });
    crate::Http3Server::new(&address.to_string())
        .add_cert_path(root.join("cert.pem"))
        .add_key_path(root.join("key.pem"))
        .set_file_storage_path(root.join("staging"))
        .run_blocking(router);
}

struct Server {
    child: Option<Child>,
    root: PathBuf,
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

fn initial(dcid: &[u8], token: &[u8]) -> Vec<u8> {
    let mut packet = vec![0xc0];
    packet.extend_from_slice(&quiche::PROTOCOL_VERSION.to_be_bytes());
    packet.push(dcid.len().try_into().unwrap());
    packet.extend_from_slice(dcid);
    packet.push(16);
    packet.extend_from_slice(&[0x24; 16]);
    let length: u16 = token.len().try_into().unwrap();
    assert!(length < 16384);
    packet.extend_from_slice(&(0x4000 | length).to_be_bytes());
    packet.extend_from_slice(token);
    packet.extend_from_slice(&[0x40, 0x14]);
    packet.resize(1200, 0);
    packet
}

fn expect_silence(socket: &UdpSocket, address: SocketAddr, packet: &[u8]) {
    socket.send_to(packet, address).unwrap();
    let mut buffer = [0; 2048];
    match socket.recv_from(&mut buffer) {
        Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
        other => panic!("invalid Initial unexpectedly received a response: {other:?}"),
    }
}

#[test]
#[ignore = "requires loopback UDP and openssl; run explicitly with the robustness bench"]
fn retry_rejections_leave_http3_available() {
    let mut random = [0; 16];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut random).unwrap();
    let name = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let root = std::env::temp_dir().join(format!("faces-retry-{name}"));
    std::fs::create_dir_all(root.join("staging")).unwrap();
    let mut server = Server { child: None, root };
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
        .arg(server.root.join("key.pem"))
        .arg("-out")
        .arg(server.root.join("cert.pem"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success());
    let reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let log = std::fs::File::create(server.root.join("server.log")).unwrap();
    server.child = Some(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", CHILD_TEST, "--ignored", "--nocapture"])
            .env("FACES_RETRY_TEST_ADDRESS", address.to_string())
            .env("FACES_RETRY_TEST_ROOT", &server.root)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut buffer = [0; 2048];
    // Receiving a Retry is the startup barrier and confirms this server minted
    // the token subsequently mutated in the negative wire probes.
    let retry = loop {
        assert!(Instant::now() < deadline, "server did not issue a Retry");
        assert!(server.child.as_mut().unwrap().try_wait().unwrap().is_none());
        socket.send_to(&initial(&[0x35; 16], &[]), address).unwrap();
        match socket.recv_from(&mut buffer) {
            Ok((len, from)) => {
                assert_eq!(from, address);
                break quiche::Header::from_slice(&mut buffer[..len], quiche::MAX_CONN_ID_LEN)
                    .unwrap();
            }
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(error) => panic!("Retry receive: {error}"),
        }
    };
    assert_eq!(retry.ty, quiche::Type::Retry);
    assert_eq!(retry.scid.len(), 20);
    let token = retry.token.unwrap();
    assert_eq!(token.len(), 58);
    for length in [0, 1, 7, 21] {
        expect_silence(&socket, address, &initial(&vec![0x35; length], &[]));
    }
    let mut forged = b"quiche".to_vec();
    forged.extend_from_slice(&[127, 0, 0, 1]);
    forged.extend_from_slice(&[0x35; 16]);
    expect_silence(&socket, address, &initial(&retry.scid, &forged));
    for index in [0, 1, 9, 10, token.len() - 1] {
        let mut changed = token.clone();
        changed[index] ^= 1;
        expect_silence(&socket, address, &initial(&retry.scid, &changed));
    }
    for length in [1, 49, token.len() - 1] {
        expect_silence(&socket, address, &initial(&retry.scid, &token[..length]));
    }
    let mut suffix = token.clone();
    suffix.push(0);
    expect_silence(&socket, address, &initial(&retry.scid, &suffix));
    let mut other_cid = retry.scid.to_vec();
    other_cid[0] ^= 1;
    expect_silence(&socket, address, &initial(&other_cid, &token));
    let other_port = UdpSocket::bind("127.0.0.1:0").unwrap();
    other_port
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    assert_ne!(
        other_port.local_addr().unwrap(),
        socket.local_addr().unwrap()
    );
    expect_silence(&other_port, address, &initial(&retry.scid, &token));
    // Silence alone does not prove non-allocation. Unit tests and the production
    // validation-before-accept branch establish that; this checks availability.
    http3_probe(address, &server.root.join("cert.pem"));
    assert!(server.child.as_mut().unwrap().try_wait().unwrap().is_none());
    println!("Retry wire probes rejected; fresh QUIC Retry + verified loopback TLS + HTTP3 200 completed");
}

fn http3_probe(address: SocketAddr, certificate: &Path) {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let mut config = quiche::Config::new(quiche::PROTOCOL_VERSION).unwrap();
    config.verify_peer(true);
    config
        .load_verify_locations_from_file(certificate.to_str().unwrap())
        .unwrap();
    config
        .set_application_protos(h3::APPLICATION_PROTOCOL)
        .unwrap();
    config.set_max_idle_timeout(5000);
    config.set_max_recv_udp_payload_size(1350);
    config.set_max_send_udp_payload_size(1350);
    config.set_initial_max_data(1_000_000);
    config.set_initial_max_stream_data_bidi_local(100_000);
    config.set_initial_max_stream_data_bidi_remote(100_000);
    config.set_initial_max_stream_data_uni(100_000);
    config.set_initial_max_streams_bidi(10);
    config.set_initial_max_streams_uni(10);
    let mut conn = quiche::connect(
        Some("localhost"),
        &quiche::ConnectionId::from_ref(&[0x19; 16]),
        socket.local_addr().unwrap(),
        address,
        &mut config,
    )
    .unwrap();
    let mut http = None;
    let mut stream = None;
    let mut status = None;
    let mut body = Vec::new();
    let mut saw_retry = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut buffer = [0; 65_535];
    loop {
        assert!(Instant::now() < deadline, "HTTP3 probe timed out");
        loop {
            match socket.recv_from(&mut buffer) {
                Ok((len, from)) => {
                    assert_eq!(from, address);
                    if let Ok(header) =
                        quiche::Header::from_slice(&mut buffer[..len], quiche::MAX_CONN_ID_LEN)
                    {
                        saw_retry |= header.ty == quiche::Type::Retry;
                    }
                    conn.recv(
                        &mut buffer[..len],
                        quiche::RecvInfo {
                            from,
                            to: socket.local_addr().unwrap(),
                        },
                    )
                    .unwrap();
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => panic!("UDP receive: {error}"),
            }
        }
        if conn.timeout().is_some_and(|timeout| timeout.is_zero()) {
            conn.on_timeout();
        }
        assert!(!conn.is_closed(), "QUIC closed: {:?}", conn.peer_error());
        if conn.is_established() && http.is_none() {
            http = Some(
                h3::Connection::with_transport(&mut conn, &h3::Config::new().unwrap()).unwrap(),
            );
        }
        if let Some(http) = &mut http {
            if stream.is_none() {
                stream = Some(
                    http.send_request(
                        &mut conn,
                        &[
                            Header::new(b":method", b"GET"),
                            Header::new(b":scheme", b"https"),
                            Header::new(b":authority", b"localhost"),
                            Header::new(b":path", b"/retry-probe"),
                        ],
                        true,
                    )
                    .unwrap(),
                );
            }
            loop {
                match http.poll(&mut conn) {
                    Ok((id, h3::Event::Headers { list, .. })) if Some(id) == stream => {
                        status = list
                            .iter()
                            .find(|header| header.name() == b":status")
                            .map(|header| header.value().to_vec());
                    }
                    Ok((id, h3::Event::Data)) if Some(id) == stream => {
                        while let Ok(len) = http.recv_body(&mut conn, id, &mut buffer) {
                            body.extend_from_slice(&buffer[..len]);
                        }
                    }
                    Ok((id, h3::Event::Finished)) if Some(id) == stream => {
                        assert!(saw_retry);
                        assert_eq!(status.as_deref(), Some(b"200".as_slice()));
                        assert_eq!(body, b"retry-ok");
                        return;
                    }
                    Ok((_, h3::Event::Reset(code))) => panic!("HTTP3 reset: {code}"),
                    Ok(_) => {}
                    Err(h3::Error::Done) => break,
                    Err(error) => panic!("HTTP3 poll: {error:?}"),
                }
            }
        }
        loop {
            match conn.send(&mut buffer[..1350]) {
                Ok((len, info)) => {
                    socket.send_to(&buffer[..len], info.to).unwrap();
                }
                Err(quiche::Error::Done) => break,
                Err(error) => panic!("QUIC send: {error:?}"),
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
