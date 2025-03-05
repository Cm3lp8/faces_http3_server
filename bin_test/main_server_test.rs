use std::io::{BufRead, BufReader, Cursor};
use std::ops::Add;
use std::thread;

use faces_quic_server::{
    BodyStorage, ContentType, H3Method, Http3Server, RequestResponse, RouteConfig, RouteForm,
};
use faces_quic_server::{RequestType, RouteManager, RouteManagerBuilder, ServerConfig};
use log::info;
fn main() {
    env_logger::init();
    let addr = "192.168.1.22:3000";

    let _server = Http3Server::new(addr)
        .add_key_path("/home/camille/Documents/rust/faces_http3_server/key.pem")
        .add_cert_path("/home/camille/Documents/rust/faces_http3_server/cert.pem")
        .set_file_storage_path("/home/camille/.temp_server/")
        .route_post(
            "/large_data",
            RouteConfig::new(BodyStorage::File),
            |route_builder| {
                route_builder
                    .set_route_callback(|req_event| {
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
        )
        .run_blocking();
}
