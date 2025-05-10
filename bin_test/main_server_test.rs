#![allow(warnings)]
use std::collections::HashMap;
use std::hash::Hash;
use std::io::{BufRead, BufReader, Cursor};
use std::ops::Add;
use std::process::Output;
use std::sync::{Arc, Mutex};
use std::{thread, usize};

use faces_quic_server::prelude::*;
use log::{error, info, warn};
use quiche::h3;
fn main() {
    env_logger::init();
    let addr = "192.168.1.22:3000";

    let app_state = ();

    #[derive(Clone)]
    pub struct AppStateTest;

    type UserID = usize;
    type StreamIdent = (Vec<u8>, u64);
    pub struct StreamSessions {
        register: Arc<Mutex<HashMap<UserID, StreamIdent>>>,
    }
    impl StreamSessions {
        pub fn register(&self, user_id: UserID, stream_id: StreamIdent) {
            let guard = &mut *self.register.lock().unwrap();

            guard.entry(user_id).or_insert(stream_id);
        }
    }

    impl UserSessions for StreamSessions {
        type Output = StreamSessions;
        type Key = UserID;
        fn new() -> Self::Output {
            StreamSessions {
                register: Arc::new(Mutex::new(HashMap::new())),
            }
        }
        fn user_sessions(&self) -> &Self::Output {
            self
        }

        fn broadcast_to_streams(&self, keys: &[Self::Key]) -> Vec<impl ToStreamIdent> {
            let mut dst: Vec<StreamIdent> = vec![];

            let guard = &*self.register.lock().unwrap();
            for key in keys {
                if let Some(stream_ident) = guard.get(&key) {
                    dst.push(stream_ident.to_owned())
                }
            }

            dst
        }
    }

    let mut router = RouteManager::<_, StreamSessions>::new_with_app_state(AppStateTest);

    let streams = StreamSessions::new();

    let middle_ware_0 = router.middleware(&|headers, app_state| MiddleWareFlow::Continue(headers));
    let handler_0 = router.handler(&|event, app_state, current_status_response| {
        info!(
            "Received Data on file path [{}] on [{:?}] ",
            event.bytes_written(),
            event.path()
        );
        Response::ok_200_with_data(event, vec![9; 23])
    });

    let handler_1 = router.handler(&|event, app_state, current_status_response| {
        info!(
            "Received Data on file path [{}] on [{:?}] ",
            event.bytes_written(),
            event.path()
        );
        Response::ok_200_with_data(event, vec![0; 1888835])
    });
    router.route_post(
        "/large_data",
        RouteConfig::new(DataManagement::Storage(BodyStorage::File)),
        |route_builder| {
            route_builder.middleware(&middle_ware_0);
            route_builder.handler(&handler_0);
        },
    );
    router.route_get("/test", RouteConfig::default(), |route_builder| {
        route_builder.handler(&handler_1);
        route_builder.middleware(&middle_ware_0);
    });
    router.route_get("/test_mini", RouteConfig::default(), |route_builder| {
        route_builder.handler(&handler_0);
        route_builder.middleware(&middle_ware_0);
    });

    router.set_error_handler(ErrorType::Error404, |error_buidler| {
        error_buidler.header(&[h3::Header::new(b"type", b"bad request")]);
    });
    let _server = Http3Server::new(addr)
        .add_key_path("/home/camille/Documents/rust/faces_http3_server/key.pem")
        .add_cert_path("/home/camille/Documents/rust/faces_http3_server/cert.pem")
        .set_file_storage_path("/home/camille/.temp_server/")
        .run_blocking(router);
}
