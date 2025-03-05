use std::io::{BufRead, BufReader, Cursor};
use std::ops::Add;
use std::sync::Arc;
use std::thread;

use faces_quic_server::{
    BodyStorage, ContentType, DataEvent, DataManagement, H3Method, Http3Server, RequestResponse,
    RouteConfig, RouteEventListener, RouteForm,
};
use faces_quic_server::{RequestType, RouteManager, RouteManagerBuilder, ServerConfig};
use log::info;
fn main() {
    env_logger::init();
    let addr = "192.168.1.22:3000";

    let mut router = RouteManager::new();

    struct DataEventHandler {
        data_cb: Box<dyn Fn(DataEvent) + 'static + Send + Sync>,
    }
    impl DataEventHandler {
        pub fn new(
            cb: impl Fn(DataEvent) + Send + Sync + 'static,
        ) -> Arc<Box<dyn RouteEventListener + Send + Sync + 'static>> {
            Arc::new(Box::new(Self {
                data_cb: Box::new(cb),
            }))
        }
    }

    impl RouteEventListener for DataEventHandler {
        fn on_data(&self, event: DataEvent) {
            (self.data_cb)(event);
        }
    }

    let event_listener =
        DataEventHandler::new(|data_event| info!("data received [{:?}]", data_event.packet_len()));

    router.route_post(
        "/large_data",
        RouteConfig::new(DataManagement::Storage(BodyStorage::File)),
        |route_builder| {
            route_builder
                .subscribe_event(event_listener.clone())
                .on_finished_callback(|req_event| {
                    println!("Large data received len [{:?}]", req_event.as_body().len());

                    if let Some(file_path) = req_event.get_file_path() {
                        info!("{:#?}", file_path);
                    }

                    let response = RequestResponse::new()
                        .set_status(faces_quic_server::Status::Ok(200))
                        .set_body(vec![9; 30_000])
                        .set_content_type(ContentType::Text)
                        .build();
                    response
                })
                .set_request_type(RequestType::Ping);
        },
    );
    router.route_get("/test", RouteConfig::default(), |route_builder| {
        route_builder
            .on_finished_callback(|req_event| {
                println!("[{:#?}]", req_event.headers());
                Err(())
            })
            .set_request_type(RequestType::Ping);
    });

    let _server = Http3Server::new(addr)
        .add_key_path("/home/camille/Documents/rust/faces_http3_server/key.pem")
        .add_cert_path("/home/camille/Documents/rust/faces_http3_server/cert.pem")
        .set_file_storage_path("/home/camille/.temp_server/")
        .run_blocking(router.build());
}
