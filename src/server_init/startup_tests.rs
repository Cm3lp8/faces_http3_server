//! Owned subprocesses; fault injection is absent from non-test transport builds.
use crate::{Http3Server, RouteManager, ServerRunError};
use std::{
    fs,
    io::{self, Write},
    net::{SocketAddr, UdpSocket},
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

const CHILD: &str = "server_init::startup_tests::startup_server_child";
const LIMIT: Duration = Duration::from_secs(10);

pub(super) fn after_network_ready(address: SocketAddr) -> Option<Result<(), ServerRunError>> {
    let mode = std::env::var("FACES_STARTUP_FAULT").ok()?;
    if !matches!(mode.as_str(), "error" | "stopped" | "panic") {
        return None;
    }
    eprintln!("fixture_worker_ready address={address}");
    let mut command = String::new();
    io::stdin().read_line(&mut command).unwrap();
    assert_eq!(
        command, "release\n",
        "parent must release the real network worker"
    );
    match mode.as_str() {
        "error" => Some(Err(ServerRunError::io(
            "injected_worker_error",
            io::Error::other("owned test fault"),
        ))),
        "stopped" => Some(Ok(())),
        "panic" => panic!("owned network worker panic"),
        _ => unreachable!(),
    }
}

#[test]
#[ignore = "owned subprocess entry point, not a standalone regression"]
fn startup_server_child() {
    let Ok(root) = std::env::var("FACES_STARTUP_ROOT") else {
        return;
    };
    env_logger::init();
    let root = PathBuf::from(root);
    let address = std::env::var("FACES_STARTUP_ADDRESS").unwrap();
    if address != "invalid-address" {
        assert!(address.parse::<SocketAddr>().unwrap().ip().is_loopback());
    }
    let mut router = RouteManager::<_, crate::test_support::TestSessions>::new_with_app_state(());
    let handler = router
        .handler(&|event, _, _| crate::Response::ok_200_with_data(event, b"startup-ok".to_vec()));
    router.route_get("/startup-probe", crate::RouteConfig::default(), |route| {
        route.handler(&handler);
    });
    let mut builder = Http3Server::new(&address);
    builder
        .add_cert_path(root.join("cert.pem"))
        .add_key_path(root.join("key.pem"))
        .set_file_storage_path(root.join("staging"));
    if std::env::var_os("FACES_STARTUP_LEGACY").is_some() {
        builder.run_blocking(router);
    } else {
        match builder.try_run_blocking(router) {
            Err(error) => {
                eprintln!("server_failed {error}");
                std::process::exit(1);
            }
            Ok(()) => std::process::exit(0),
        }
    }
}

struct OwnedChild(Option<Child>);
impl OwnedChild {
    fn wait(&mut self, limit: Duration) -> ExitStatus {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(status) = self.0.as_mut().unwrap().try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "owned child exceeded its deadline"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Fixture {
    root: PathBuf,
    address: SocketAddr,
    reservation: Option<UdpSocket>,
    child: OwnedChild,
}
impl Fixture {
    fn new(case: &str) -> Self {
        let base = std::env::var_os("FACES_TEST_ARTIFACT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let root = base.join(format!("transport-startup-{case}-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(".env"), "").unwrap();
        let log = fs::File::create(root.join("openssl.log")).unwrap();
        let mut openssl = OwnedChild(Some(
            Command::new("openssl")
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
                    "subjectAltName=DNS:localhost",
                ])
                .arg("-keyout")
                .arg(root.join("key.pem"))
                .arg("-out")
                .arg(root.join("cert.pem"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()
                .unwrap(),
        ));
        assert!(openssl.wait(LIMIT).success());
        let reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
        Self {
            root,
            address: reservation.local_addr().unwrap(),
            reservation: Some(reservation),
            child: OwnedChild(None),
        }
    }
    fn start(&mut self, case: &str, fault: Option<&str>) {
        let address = if case == "invalid-address" {
            "invalid-address".to_owned()
        } else {
            self.address.to_string()
        };
        if case != "occupied-port" {
            self.reservation.take();
        }
        let log = fs::File::create(self.root.join("child.log")).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", CHILD, "--ignored", "--nocapture"])
            .env_clear()
            .env("RUST_LOG", "info")
            .env("FACES_STARTUP_ROOT", &self.root)
            .env("FACES_STARTUP_ADDRESS", address)
            .current_dir(&self.root)
            .stdin(Stdio::piped())
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        if let Some(fault) = fault {
            command.env("FACES_STARTUP_FAULT", fault);
        }
        if case == "legacy-missing-cert" {
            command.env("FACES_STARTUP_LEGACY", "1");
        }
        self.child.0 = Some(command.spawn().unwrap());
    }
    fn log(&self) -> String {
        fs::read_to_string(self.root.join("child.log")).unwrap()
    }
    fn await_barrier(&mut self) {
        let deadline = Instant::now() + LIMIT;
        while !self.log().contains("fixture_worker_ready address=") {
            assert!(
                self.child.0.as_mut().unwrap().try_wait().unwrap().is_none(),
                "child stopped before its barrier: {}",
                self.log()
            );
            assert!(
                Instant::now() < deadline,
                "worker did not reach its barrier"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert!(self.log().contains("network_ready address="));
        assert!(
            UdpSocket::bind(self.address).is_err(),
            "barrier must follow the real socket bind"
        );
        self.child
            .0
            .as_mut()
            .unwrap()
            .stdin
            .take()
            .unwrap()
            .write_all(b"release\n")
            .unwrap();
    }
    fn record(
        &mut self,
        case: &str,
        status: ExitStatus,
        elapsed: Duration,
        phase: &str,
        ready: bool,
    ) {
        self.reservation.take();
        let released = UdpSocket::bind(self.address).is_ok();
        let report = serde_json::json!({"case":case,"exit_code":status.code(),"elapsed_ms":elapsed.as_millis(),
            "address":self.address.to_string(),"phase":phase,"network_ready":ready,"port_released":released,
            "child_reaped":self.child.0.as_mut().unwrap().try_wait().unwrap().is_some()});
        fs::write(
            self.root.join("result.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
        assert!(released);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Children inherit the bench's owned process group; the harness is the final
        // timeout safety net. Here kill+wait also applies when an assertion unwinds.
        if let Some(mut child) = self.child.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_file(self.root.join("key.pem"));
        let _ = fs::remove_file(self.root.join("cert.pem"));
    }
}

#[test]
#[ignore = "requires owned loopback subprocesses and openssl"]
fn startup_prerequisite_failures_exit_without_false_readiness() {
    for (case, phase) in [
        ("invalid-address", "address"),
        ("blocked-storage", "storage"),
        ("missing-cert", "certificate"),
        ("malformed-cert", "certificate"),
        ("missing-key", "private_key"),
        ("malformed-key", "private_key"),
        ("occupied-port", "udp_bind"),
        ("legacy-missing-cert", "certificate"),
    ] {
        let mut fixture = Fixture::new(case);
        match case {
            "blocked-storage" => {
                fs::write(fixture.root.join("staging"), "not a directory").unwrap()
            }
            "missing-cert" | "legacy-missing-cert" => {
                fs::remove_file(fixture.root.join("cert.pem")).unwrap()
            }
            "malformed-cert" => {
                fs::write(fixture.root.join("cert.pem"), "invalid certificate fixture").unwrap()
            }
            "missing-key" => fs::remove_file(fixture.root.join("key.pem")).unwrap(),
            "malformed-key" => {
                fs::write(fixture.root.join("key.pem"), "invalid private key fixture").unwrap()
            }
            _ => {}
        }
        let started = Instant::now();
        fixture.start(case, None);
        let status = fixture.child.wait(LIMIT);
        let log = fixture.log();
        assert!(!status.success(), "{case}: {log}");
        assert!(log.contains(&format!("phase={phase}")), "{case}: {log}");
        assert!(!log.contains("network_ready"), "{case}: false readiness");
        fixture.record(case, status, started.elapsed(), phase, false);
    }
}

#[test]
#[ignore = "requires owned loopback subprocesses and openssl"]
fn network_worker_error_stop_and_panic_cross_the_real_join() {
    for (fault, category, phase) in [
        ("error", "io", "injected_worker_error"),
        ("stopped", "worker_stopped", "network_worker"),
        ("panic", "worker_panic", "network_worker"),
    ] {
        let mut fixture = Fixture::new(fault);
        fixture.start(fault, Some(fault));
        fixture.await_barrier();
        let started = Instant::now();
        let status = fixture.child.wait(LIMIT);
        let log = fixture.log();
        assert_eq!(status.code(), Some(1), "{fault}: {log}");
        assert!(
            log.contains(&format!("server_failed category={category} phase={phase}")),
            "{fault}: {log}"
        );
        fixture.record(fault, status, started.elapsed(), phase, true);
    }
}
