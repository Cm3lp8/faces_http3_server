use std::io::{BufRead, BufReader, Cursor};
use std::ops::Add;
use std::thread;

use faces_quic_server::{
    ContentType, H3Method, Http3Server, RequestResponse, RouteConfig, RouteForm,
};
use faces_quic_server::{RequestType, RouteManager, RouteManagerBuilder, ServerConfig};
use log::info;
fn main() {
    env_logger::init();
    let addr = "192.168.1.22:3000";

    let server = Http3Server::new(addr)
        .add_key_path("/home/camille/Documents/rust/faces_http3_server/key.pem")
        .add_cert_path("/home/camille/Documents/rust/faces_http3_server/cert.pem")
        .add_route("/large_data", RouteConfig::default(), |route_builder| {
            route_builder
                .set_path("/large_data")
                .set_method(H3Method::POST)
                .set_scheme("https")
                .set_request_callback(|mut req_event| {
                    println!("Large data received len [{:?}]", req_event.as_body().len());
                    let reader = BufReader::new(Cursor::new(req_event.take_body()));

                    for line in reader.lines() {
                        println!("{}", line.unwrap());
                    }

                    info!("{:#?}", req_event.headers());

                    let response = RequestResponse::new()
                        .set_status(faces_quic_server::Status::Ok(200))
                        .set_body(vec![9; 30_000])
                        .set_content_type(ContentType::Text)
                        .build();
                    response
                })
                .set_request_type(RequestType::Ping);
        })
        .run();

    thread::park();
}
