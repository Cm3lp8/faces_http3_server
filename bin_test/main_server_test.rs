#![allow(warnings)]
use std::io::{BufRead, BufReader, Cursor};
use std::ops::Add;
use std::sync::{Arc, Mutex};
use std::{thread, usize};

use faces_quic_server::prelude::*;
use log::{error, info, warn};
use quiche::h3;
fn main() {
    env_logger::init();
    let addr = "192.168.1.22:3000";

    let app_state = ();

    let handler = route_handle!(
        |event: FinishedEvent, current_status_response: RouteResponse| {
            info!(
                "Received Data on file path [{}] on [{:?}] ",
                event.bytes_written(),
                event.path()
            );
            Response::ok_200_with_data(event, vec![0; 1888835])
        }
    );
    let other_handler = route_handle!(|event: FinishedEvent, current_status_response| {
        info!(
            "Received Data on file path [{}] on [{:?}] ",
            event.bytes_written(),
            event.path()
        );
        Response::ok_200_with_data(event, vec![9; 23])
    });

    struct HandlerTestMini {
        context: Arc<Mutex<usize>>,
    }

    let handler_mini = HandlerTestMini {
        context: Arc::new(Mutex::new(0)),
    };

    route_handle!(
        HandlerTestMini,
        |context, event, current_status_response| {
            Response::ok_200_with_data(event, vec![9; 23])
        }
    );
    #[derive(Clone)]
    pub struct AppStateTest;
    let mut router = RouteManager::new_with_app_state(AppStateTest);

    let middle_ware_0 = router.middleware(&|headers, app_state| MiddleWareFlow::Continue(headers));

    router.route_post(
        "/large_data",
        RouteConfig::new(DataManagement::Storage(BodyStorage::File)),
        |route_builder| {
            route_builder.middleware(&middle_ware_0);
            route_builder.handler(&other_handler);
        },
    );
    router.route_get("/test", RouteConfig::default(), |route_builder| {
        route_builder.handler(&handler);
        route_builder.middleware(&middle_ware_0);
    });
    router.route_get("/test_mini", RouteConfig::default(), |route_builder| {
        route_builder.handler(&handler);
        route_builder.middleware(&middle_ware_0);
        //            .middleware(Arc::new(MiddleWareForcedError))
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
