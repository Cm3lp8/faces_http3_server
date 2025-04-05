pub use request_hndlr::RouteHandler;
pub use route_handle_implementation::{
    response_preparation_with_route_handler, send_404, send_error,
};

use crate::request_response::QueuedRequest;
pub use crate::route_manager::{
    H3Method, RequestType, RouteForm, RouteFormBuilder, RouteManager, RouteManagerBuilder,
};
pub use request_temp_table::ReqArgs;
pub use request_temp_table::RequestsTable;
mod request_temp_table;
mod request_hndlr {

    use std::{
        collections::HashMap,
        env::args,
        fmt::Pointer,
        sync::{Arc, Mutex, MutexGuard},
        time::Duration,
    };

    use crate::{
        file_writer::{FileWritable, FileWriterChannel},
        handler_dispatcher::RouteEventDispatcher,
        header_queue_processing::{HeaderMessage, MiddleWareJob},
        middleware,
        request_response::{
            BodyRequest, ChunkSender, ChunkingStation, ChunksDispatchChannel, HeaderPriority,
            HeaderRequest,
        },
        route_events::{self, EventType, RouteEvent},
        route_manager::{DataManagement, RouteManagerInner},
        server_config,
        server_init::quiche_http3_server::{self, Client},
        BodyStorage, FinishedEvent, HeadersColl, MiddleWareFlow, MiddleWareResult, RequestResponse,
        RouteEventListener, ServerConfig,
    };
    use mio::{net::UdpSocket, Waker};
    use quiche::{
        h3::{self, NameValue},
        Connection,
    };
    use request_hndlr::route_handle_implementation::{send_404, send_error};

    use self::{
        request_temp_table::RequestsTable, route_handle_implementation::response_preparation,
    };

    use super::*;

    pub struct RouteHandler<S> {
        inner: Arc<Mutex<RouteManagerInner<S>>>,
    }
    impl<S: Sync + Send + 'static> Clone for RouteHandler<S> {
        fn clone(&self) -> Self {
            Self {
                inner: self.inner.clone(),
            }
        }
    }

    impl<S: Send + Sync + 'static + Clone> RouteHandler<S> {
        pub fn new(route_mngr_inner: Arc<Mutex<RouteManagerInner<S>>>) -> Self {
            RouteHandler {
                inner: route_mngr_inner,
            }
        }
        pub fn app_state(&self) -> S {
            let guard = &*self.inner.lock().unwrap();
            guard.app_state().clone()
        }
        pub fn send_header_work(
            &self,
            path: &str,
            method: H3Method,
            content_length: Option<usize>,
            middleware_collection: Vec<
                Box<dyn FnMut(&mut [h3::Header], &S) -> MiddleWareFlow + Send + Sync + 'static>,
            >,
            msg: HeaderMessage,
            middleware_result_sender: crossbeam_channel::Sender<MiddleWareResult>,
        ) -> Option<MiddleWareJob<S>> {
            // let guard = &*self.inner.lock().unwrap();

            let stream_id = msg.stream_id();
            let scid = msg.scid();
            let conn_id = msg.conn_id();
            let has_more_frames = msg.more_frames();
            let headers = msg.headers();
            let middleware_job = MiddleWareJob::new(
                path,
                method,
                stream_id,
                scid.to_vec(),
                conn_id,
                has_more_frames,
                content_length,
                headers.to_vec(),
                middleware_collection,
                middleware_result_sender,
            );

            Some(middleware_job)
        }
        pub fn set_intermediate_headers_send(&self, stream_id: u64, client: &Client) {
            let guard = &*self.inner.lock().unwrap();
            let conn_id = client.conn_ref().trace_id();
            guard
                .routes_states()
                .set_intermediate_headers_send(stream_id, conn_id.to_string());
        }
        pub fn process_handler(&self, event: FinishedEvent) -> Option<RequestResponse> {
            let guard = self.inner.lock().unwrap();
            guard.route_event_dispatcher().dispatch_finished(event);
            None
        }
        pub fn send_reception_status_first(
            &self,
            stream_id: u64,
            scid: &[u8],
            conn_id: &str,
            chunk_dispatch_channel: &ChunksDispatchChannel,
        ) -> Result<usize, ()> {
            let guard = &*self.inner.lock().unwrap();
            let reception_status = guard
                .routes_states()
                .get_reception_status_infos(stream_id, conn_id.to_owned());

            if let Some((reception_status, header_send)) = reception_status {
                if reception_status.has_something_to_update() {
                    if let Some(percentage_written) =
                        reception_status.get_percentage_written_to_string()
                    {
                        let headers = vec![
                            h3::Header::new(b":status", b"100"),
                            h3::Header::new(b"x-for", stream_id.to_string().as_bytes()),
                            h3::Header::new(b"x-progress", percentage_written.as_bytes()),
                        ];

                        if !header_send {
                            /*
                                                        guard
                                                            .routes_states()
                                                            .set_intermediate_headers_send(stream_id, conn_id.to_string());
                            */

                            let sender =
                                chunk_dispatch_channel.insert_new_channel(stream_id, &scid);
                            let (_recv_send_confirmation, header_req) = HeaderRequest::new(
                                stream_id,
                                &scid,
                                headers.clone(),
                                false,
                                None,
                                None,
                                crate::request_response::HeaderPriority::SendHeader100,
                            );
                            if let Err(_) = chunk_dispatch_channel.send_to_high_priority_queue(
                                stream_id,
                                &scid,
                                QueuedRequest::new_header(header_req),
                            ) {
                                error!("Failed to send header_req")
                            }
                            /*
                                                        return quiche_http3_server::send_header(
                                                            client, stream_id, headers, false,
                                                        );
                            */
                        }
                    };
                }
            }
            Ok(0)
        }
        ///
        /// Send a reception status to the client, only if something can be updated.
        ///
        ///
        pub fn send_reception_status(
            &self,
            client: &mut quiche_http3_server::QClient,
            response_sender_high: crossbeam_channel::Sender<QueuedRequest>,
            response_sender_low: crossbeam_channel::Sender<QueuedRequest>,
            stream_id: u64,
            conn_id: &str,
            chunk_dispatch_channel: &ChunksDispatchChannel,
        ) -> Result<usize, ()> {
            let guard = &*self.inner.lock().unwrap();
            let scid = client.conn().source_id().as_ref().to_vec();
            let reception_status = guard
                .routes_states()
                .get_reception_status_infos(stream_id, conn_id.to_owned());

            if let Some((reception_status, header_send)) = reception_status {
                if reception_status.has_something_to_update() {
                    if let Some(percentage_written) =
                        reception_status.get_percentage_written_to_string()
                    {
                        let headers = vec![
                            h3::Header::new(b":status", b"100"),
                            h3::Header::new(b"x-for", stream_id.to_string().as_bytes()),
                            h3::Header::new(b"x-progress", percentage_written.as_bytes()),
                        ];

                        if !header_send {
                            return Err(());
                            /*
                                                        guard
                                                            .routes_states()
                                                            .set_intermediate_headers_send(stream_id, conn_id.to_string());
                            */

                            /*
                                                        return quiche_http3_server::send_header(
                                                            client, stream_id, headers, false,
                                                        );
                            */
                        }
                        {
                            if let Some(body) = reception_status.body() {
                                chunk_dispatch_channel.send_to_high_priority_queue(
                                    stream_id,
                                    &scid,
                                    QueuedRequest::BodyProgression(BodyRequest::new(
                                        stream_id, conn_id, &scid, 0, body, false,
                                    )),
                                );
                            }
                        }
                    };
                }
            }
            Ok(0)
        }
        pub fn write_body_packet(
            &self,
            stream_id: u64,
            scid: &[u8],
            conn_id: &str,
            packet: &[u8],
            end: bool,
        ) -> Result<usize, ()> {
            let guard = &*self.inner.lock().unwrap();
            let written_data = guard.routes_states().write_body_packet(
                conn_id.to_owned(),
                scid,
                stream_id,
                packet,
                end,
            );
            written_data
        }
        pub fn print_entries(&self) {
            let guard = &*self.inner.lock().unwrap();
            for i in guard.routes_formats().iter() {
                println!("entrie [{}]", i.0);
            }
        }
        pub fn fetch_data_stream(
            &self,
            stream_id: u64,
            conn: &mut quiche::Connection,
            h3_conn: &mut h3::Connection,
        ) {
        }
        fn get_routes_from_path(
            &self,
            path: &str,
            cb: impl FnOnce(Option<&Vec<Arc<RouteForm<S>>>>),
        ) {
            let guard = &*self.inner.lock().unwrap();
            cb(guard.routes_formats().get(path));
        }
        pub fn handle_finished_stream(
            &self,
            conn_id: &str,
            scid: &[u8],
            stream_id: u64,
            client: &mut quiche_http3_server::QClient,
            response_sender_high: crossbeam_channel::Sender<QueuedRequest>,
            response_sender_low: crossbeam_channel::Sender<QueuedRequest>,
            chunking_station: &ChunkingStation,
            waker: &Arc<Waker>,
            last_time: &Arc<Mutex<Duration>>,
        ) {
            let guard = &*self.inner.lock().unwrap();
            let chunk_dispatch_channel = chunking_station.get_chunking_dispatch_channel();

            response_preparation(
                guard,
                waker,
                chunking_station,
                conn_id,
                &scid,
                stream_id,
                EventType::OnFinished,
                HeaderPriority::SendAdditionnalHeader,
            );
        }
        pub fn parse_headers(
            &self,
            server_config: &Arc<ServerConfig>,
            chunk_dispatch_channel: &ChunksDispatchChannel,
            mut headers: Vec<h3::Header>,
            stream_id: u64,
            client: &mut quiche_http3_server::QClient,
            socket: &mut UdpSocket,
            response_sender_high: crossbeam_channel::Sender<QueuedRequest>,
            response_sender_low: crossbeam_channel::Sender<QueuedRequest>,
            more_frames: bool,
            response_head: &ChunkingStation,
            waker: &Arc<Waker>,
            last_time: &Arc<Mutex<Duration>>,
            file_writer_channel: FileWriterChannel,
        ) {
            let conn_id = client.conn().trace_id().to_string();
            let scid = client.conn().source_id().as_ref().to_vec();
            let mut method: Option<Vec<u8>> = None;
            let mut path: Option<String> = None;
            let mut content_length: Option<usize> = None;

            {
                for hdr in &headers {
                    match hdr.name() {
                        b":method" => method = Some(hdr.value().to_vec()),
                        b":path" => path = Some(String::from_utf8(hdr.value().to_vec()).unwrap()),
                        b"content-length" => {
                            if let Some(method) = &method {
                                if let Ok(method_parsed) = H3Method::parse(&method) {
                                    if H3Method::POST == method_parsed
                                        || H3Method::PUT == method_parsed
                                    {
                                        content_length = Some(
                                    std::str::from_utf8(hdr.value())
                                .unwrap_or_else(|item| {
                                    error!(
                                        "Failed to parse bytes into str, default is \"0\" length "
                                    );
                                    "0"
                                })
                                .parse::<usize>()
                                .unwrap_or_else(|_| {
                                    error!("Failed to parse digit_string to usize. Default is \"0\" length");
                                    0
                                }),
                        )
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            if method.is_none() || path.is_none() {
                return;
            }

            let guard = &*self.inner.lock().unwrap();

            let mut data_management: Option<DataManagement> = None;
            let mut event_subscriber: Option<Arc<dyn RouteEventListener + 'static + Send + Sync>> =
                None;
            if let Ok(method) = H3Method::parse(&method.unwrap()) {
                if let Some((route_form, _)) =
                    guard.get_routes_from_path_and_method(path.as_ref().unwrap().as_str(), method)
                {
                    data_management = route_form.data_management_type();
                    event_subscriber = route_form.event_subscriber();
                    event_subscriber.as_ref().unwrap().on_header();
                    let headers_coll: HeadersColl = method.get_headers_for_middleware(&mut headers);

                    //Iteration on the middleware collection.
                    if let Err(error_type) =
                        route_form.process_middlewares(&headers_coll, &guard.app_state())
                    {
                        send_error(
                            error_type,
                            guard,
                            &waker,
                            chunk_dispatch_channel,
                            response_head,
                            &scid,
                            stream_id,
                            EventType::OnFinished,
                            HeaderPriority::SendHeader,
                        );
                    }
                } else {
                    //make the differentation between bad request and bad method.
                    warn!("No route found send a 404 error");
                    send_404(
                        path.as_ref().unwrap().as_str(),
                        guard,
                        &waker,
                        chunk_dispatch_channel,
                        response_head,
                        &scid,
                        stream_id,
                        EventType::OnFinished,
                        HeaderPriority::SendHeader,
                    );
                }
                guard.routes_states().add_partial_request(
                    server_config,
                    conn_id.clone(),
                    stream_id,
                    method,
                    data_management,
                    event_subscriber.clone(),
                    &headers,
                    path.unwrap().as_str(),
                    content_length,
                    !more_frames,
                    file_writer_channel,
                );
            } else {
                return;
            }

            if more_frames {
                return;
            }

            ////// response prep
            response_preparation(
                guard,
                &waker,
                response_head,
                conn_id.as_str(),
                &scid,
                stream_id,
                EventType::OnFinished,
                HeaderPriority::SendHeader,
            );
        }
        /// Partial_request prep
        pub fn partial_req(
            &self,
            server_config: &Arc<ServerConfig>,
            event_subscriber: Option<Arc<dyn RouteEventListener + 'static + Send + Sync>>,
            conn_id: &str,
            stream_id: u64,
            method: H3Method,
            data_management: Option<DataManagement>,
            headers: &[h3::Header],
            path: String,
            content_length: Option<usize>,
            more_frames: bool,
            file_writer_channel: FileWriterChannel,
        ) {
            let guard = self.inner.lock().unwrap();
            guard.routes_states().add_partial_request(
                server_config,
                conn_id.to_string(),
                stream_id,
                method,
                data_management,
                event_subscriber.clone(),
                &headers,
                path.as_str(),
                content_length,
                !more_frames,
                file_writer_channel,
            );
        }
        pub fn inner_mut(&self, cb: impl FnOnce(&mut RouteManagerInner<S>)) {
            let guard = &mut *self.inner.lock().unwrap();
            cb(guard);
        }
        pub fn get_routes_from_path_and_method_and_request_type(
            &self,
            path: &str,
            methode: H3Method,
            request_type: RequestType,
            cb: impl FnOnce(Option<&RouteForm<S>>),
        ) {
            self.get_routes_from_path(path, |request_coll: Option<&Vec<Arc<RouteForm<S>>>>| {
                if request_coll.is_none() {
                    cb(None)
                } else {
                    let coll: &Vec<Arc<RouteForm<S>>> = request_coll.unwrap();
                    if let Some(found_request) = coll.iter().find(|item| item.method() == &methode)
                    {
                        cb(Some(found_request));
                    } else {
                        cb(None)
                    }
                }
            });
        }
        pub fn mutex_guard(&self) -> MutexGuard<RouteManagerInner<S>> {
            self.inner.lock().unwrap()
        }
    }

    impl RequestResponse {
        ///________________________________________
        ///Attach a channel sender to the body that corresponds to the associated client.
        ///Sender can be build with :
        ///client.get_response_sender()
        pub fn attach_chunk_sender(&mut self, sndr: ChunkSender) {
            match self.body_as_mut() {
                crate::request_response::BodyType::Data {
                    stream_id,
                    scid,
                    conn_id,
                    data,
                    sender,
                    conn_stats,
                } => {
                    *sender = Some(sndr);
                }
                crate::request_response::BodyType::FilePath {
                    stream_id,
                    scid,
                    conn_id,
                    file_path,
                    sender,
                    conn_stats,
                } => {
                    *sender = Some(sndr);
                }
                crate::request_response::BodyType::None => {}
            }
        }
    }
}

mod route_handle_implementation {
    use std::sync::MutexGuard;

    use quiche::h3;

    use crate::{
        request_response::{
            BodyType, ChunkSender, ChunkingStation, ChunksDispatchChannel, HeaderPriority,
            HeaderRequest,
        },
        route_events::{self, EventType},
        route_manager::{ErrorType, RouteManagerInner},
        server_init::QClient as Client,
        ErrorResponse, FinishedEvent, RouteEvent,
    };

    use super::*;

    fn send_headers(
        headers: Vec<h3::Header>,
        stream_id: u64,
        scid: &[u8],
        is_end: bool,
        body: Option<BodyType>,
        header_sender: Option<ChunkSender>,
        header_priority: HeaderPriority,
        chunking_station: &ChunkingStation,
    ) {
        let (_recv_send_confirmation, header_req) = HeaderRequest::new(
            stream_id,
            &scid,
            headers,
            true,
            None,
            header_sender,
            header_priority,
        );
        if let Err(_) = chunking_station
            .get_chunking_dispatch_channel()
            .send_to_high_priority_queue(stream_id, &scid, QueuedRequest::new_header(header_req))
        {
            error!("Failed to send header_req")
        }
    }

    pub fn send_error<S: Send + Sync + 'static + Clone>(
        error_type: ErrorResponse,
        guard: &RouteManagerInner<S>,
        waker: &mio::Waker,
        chunk_dispatch_channel: &ChunksDispatchChannel,
        chunking_station: &ChunkingStation,
        scid: &[u8],
        stream_id: u64,
        event_type: EventType,
        header_priority: HeaderPriority,
    ) {
        chunk_dispatch_channel.insert_new_channel(stream_id, &scid);
        let header_sender = chunk_dispatch_channel.get_high_priority_sender(stream_id, &scid);
        match error_type {
            ErrorResponse::Error401(content) => {
                let headers = vec![h3::Header::new(b":status", b"401")];
                send_headers(
                    headers,
                    stream_id,
                    &scid,
                    true,
                    None,
                    header_sender,
                    header_priority,
                    chunking_station,
                );
            }
            ErrorResponse::Error403(content) => {
                let headers = vec![h3::Header::new(b":status", b"401")];
                send_headers(
                    headers,
                    stream_id,
                    &scid,
                    true,
                    None,
                    header_sender,
                    header_priority,
                    chunking_station,
                );
            }
            ErrorResponse::Error415(content) => {
                let headers = vec![h3::Header::new(b":status", b"401")];
                send_headers(
                    headers,
                    stream_id,
                    &scid,
                    true,
                    None,
                    header_sender,
                    header_priority,
                    chunking_station,
                );
            }
        }
        if let Some((mut headers, body)) = guard.get_error_response(ErrorType::Error404) {
            chunk_dispatch_channel.insert_new_channel(stream_id, &scid);
            info!("found errror response is nose[{}] ", body.is_some());
            let header_sender = chunk_dispatch_channel.get_high_priority_sender(stream_id, &scid);
            let body_sender = chunk_dispatch_channel.get_low_priority_sender(stream_id, &scid);
            match body {
                Some(body) => {
                    let (_recv_send_confirmation, header_req) = HeaderRequest::new(
                        stream_id,
                        &scid,
                        headers.clone(),
                        false,
                        Some(body),
                        header_sender,
                        header_priority,
                    );

                    if let Err(_) = chunk_dispatch_channel.send_to_high_priority_queue(
                        stream_id,
                        &scid,
                        QueuedRequest::new_header(header_req),
                    ) {
                        error!("Failed to send header_req")
                    }
                    if let Err(e) = waker.wake() {
                        error!("Failed to wake poll [{:?}]", e);
                    };
                }
                None => {
                    let (_recv_send_confirmation, header_req) = HeaderRequest::new(
                        stream_id,
                        &scid,
                        headers.clone(),
                        true,
                        None,
                        header_sender,
                        header_priority,
                    );
                    if let Err(_) = chunking_station
                        .get_chunking_dispatch_channel()
                        .send_to_high_priority_queue(
                            stream_id,
                            &scid,
                            QueuedRequest::new_header(header_req),
                        )
                    {
                        error!("Failed to send header_req")
                    }
                }
            }
        }
    }
    pub fn send_404<S: Send + Sync + 'static + Clone>(
        req_path: &str,
        guard: &RouteManagerInner<S>,
        waker: &mio::Waker,
        chunk_dispatch_channel: &ChunksDispatchChannel,
        chunking_station: &ChunkingStation,
        scid: &[u8],
        stream_id: u64,
        event_type: EventType,
        header_priority: HeaderPriority,
    ) {
        if let Some((mut headers, body)) = guard.get_error_response(ErrorType::Error404) {
            headers.push(h3::Header::new(b"x-wrong-path", req_path.as_bytes()));
            chunk_dispatch_channel.insert_new_channel(stream_id, &scid);
            info!("found errror response is nose[{}] ", body.is_some());
            let header_sender = chunk_dispatch_channel.get_high_priority_sender(stream_id, &scid);
            let body_sender = chunk_dispatch_channel.get_low_priority_sender(stream_id, &scid);
            match body {
                Some(body) => {
                    let (_recv_send_confirmation, header_req) = HeaderRequest::new(
                        stream_id,
                        &scid,
                        headers.clone(),
                        false,
                        Some(body),
                        header_sender,
                        header_priority,
                    );

                    if let Err(_) = chunk_dispatch_channel.send_to_high_priority_queue(
                        stream_id,
                        &scid,
                        QueuedRequest::new_header(header_req),
                    ) {
                        error!("Failed to send header_req")
                    }
                    if let Err(e) = waker.wake() {
                        error!("Failed to wake poll [{:?}]", e);
                    };
                }
                None => {
                    let (_recv_send_confirmation, header_req) = HeaderRequest::new(
                        stream_id,
                        &scid,
                        headers.clone(),
                        true,
                        None,
                        header_sender,
                        header_priority,
                    );
                    if let Err(_) = chunking_station
                        .get_chunking_dispatch_channel()
                        .send_to_high_priority_queue(
                            stream_id,
                            &scid,
                            QueuedRequest::new_header(header_req),
                        )
                    {
                        error!("Failed to send header_req")
                    }
                }
            }
        }
    }
    pub fn response_preparation_with_route_handler<S: Send + Sync + 'static + Clone>(
        route_handler: &RouteHandler<S>,
        waker: &mio::Waker,
        method: H3Method,
        chunking_station: &ChunkingStation,
        headers: Vec<h3::Header>,
        path: &str,
        conn_id: &str,
        scid: &[u8],
        stream_id: u64,
        event_type: EventType,
        header_priority: HeaderPriority,
    ) {
        let chunk_dispatch_channel = chunking_station.get_chunking_dispatch_channel();
        let mut route_event: Option<RouteEvent> = None;
        if let Ok(rt_event) = route_handler
            .mutex_guard()
            .routes_states()
            .build_route_event(conn_id, scid, stream_id, event_type)
        {
            route_event = Some(rt_event);
        }
        chunk_dispatch_channel.insert_new_channel(stream_id, &scid);
        let header_sender = chunk_dispatch_channel.get_high_priority_sender(stream_id, &scid);
        let body_sender = chunk_dispatch_channel.get_low_priority_sender(stream_id, &scid);

        let route_form = if let Some(ref route_event) = route_event {
            let route_form = if let Some((route_form, _)) = route_handler
                .mutex_guard()
                .get_routes_from_path_and_method(route_event.path(), route_event.method())
            {
                Some(Box::new(route_form))
            } else {
                None
            };
            route_form
        } else {
            None
        };

        let mut is_end = true;

        let mut response = if let Some(route_event) = route_event {
            match route_event {
                RouteEvent::OnFinished(event) => {
                    is_end = event.is_end();
                    route_handler.process_handler(event)
                }
                _ => None,
            }
        } else {
            None
        };
        if let Some(route_form) = route_form {
            match route_form.method() {
                H3Method::GET => {
                    /*stream shutdown send response*/

                    if let Some(resp) = &mut response {
                        resp.attach_chunk_sender(body_sender.unwrap());
                    }
                    if let Ok((headers, body)) =
                        route_form.build_response(stream_id, &scid, conn_id, response)
                    {
                        match body {
                            Some(body) => {
                                let (_recv_send_confirmation, header_req) = HeaderRequest::new(
                                    stream_id,
                                    &scid,
                                    headers.clone(),
                                    false,
                                    Some(body),
                                    header_sender,
                                    header_priority,
                                );
                                if let Err(_) = chunk_dispatch_channel.send_to_high_priority_queue(
                                    stream_id,
                                    &scid,
                                    QueuedRequest::new_header(header_req),
                                ) {
                                    error!("Failed to send header_req")
                                }
                            }
                            None => {
                                let (_recv_send_confirmation, header_req) = HeaderRequest::new(
                                    stream_id,
                                    &scid,
                                    headers.clone(),
                                    is_end,
                                    None,
                                    header_sender,
                                    header_priority,
                                );
                                if let Err(_) = chunk_dispatch_channel.send_to_high_priority_queue(
                                    stream_id,
                                    &scid,
                                    QueuedRequest::new_header(header_req),
                                ) {
                                    error!("Failed to send header_req")
                                }
                            }
                        }
                    }
                }
                H3Method::POST => {
                    chunk_dispatch_channel.insert_new_channel(stream_id, scid);
                    let body_sender =
                        chunk_dispatch_channel.get_low_priority_sender(stream_id, scid);
                    let header_sender =
                        chunk_dispatch_channel.get_high_priority_sender(stream_id, scid);
                    if let Some(resp) = &mut response {
                        resp.attach_chunk_sender(body_sender.unwrap())
                    }
                    if let Ok((headers, body)) =
                        route_form.build_response(stream_id, scid, conn_id, response)
                    {
                        match body {
                            Some(body) => {
                                let (_recv_send_confirmation, header_req) = HeaderRequest::new(
                                    stream_id,
                                    &scid,
                                    headers.clone(),
                                    false,
                                    Some(body),
                                    header_sender,
                                    crate::request_response::HeaderPriority::SendAdditionnalHeader,
                                );

                                if let Err(_) = chunk_dispatch_channel.send_to_high_priority_queue(
                                    stream_id,
                                    &scid,
                                    QueuedRequest::new_header(header_req),
                                ) {
                                    error!("Failed to send header_req")
                                }
                                if let Err(e) = waker.wake() {
                                    error!("Failed to wake poll [{:?}]", e);
                                };
                            }
                            None => {
                                let (_recv_send_confirmation, header_req) = HeaderRequest::new(
                                    stream_id,
                                    &scid,
                                    headers.clone(),
                                    true,
                                    None,
                                    header_sender,
                                    crate::request_response::HeaderPriority::SendAdditionnalHeader,
                                );
                                if let Err(_) = chunking_station
                                    .get_chunking_dispatch_channel()
                                    .send_to_high_priority_queue(
                                        stream_id,
                                        &scid,
                                        QueuedRequest::new_header(header_req),
                                    )
                                {
                                    error!("Failed to send header_req")
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    pub fn response_preparation<S: Send + Sync + 'static + Clone>(
        guard: &RouteManagerInner<S>,
        waker: &mio::Waker,
        chunking_station: &ChunkingStation,
        conn_id: &str,
        scid: &[u8],
        stream_id: u64,
        event_type: EventType,
        header_priority: HeaderPriority,
    ) {
        let chunk_dispatch_channel = chunking_station.get_chunking_dispatch_channel();
        if let Ok(route_event) = guard
            .routes_states()
            .build_route_event(conn_id, scid, stream_id, event_type)
        {
            chunk_dispatch_channel.insert_new_channel(stream_id, &scid);
            let header_sender = chunk_dispatch_channel.get_high_priority_sender(stream_id, &scid);
            let body_sender = chunk_dispatch_channel.get_low_priority_sender(stream_id, &scid);
            let is_end = route_event.is_end();
            if let Some((route_form, _)) =
                guard.get_routes_from_path_and_method(route_event.path(), route_event.method())
            {
                match route_form.method() {
                    H3Method::GET => {
                        /*stream shutdown send response*/

                        let mut response =
                            if let Some(event_subscriber) = route_form.event_subscriber() {
                                event_subscriber.on_event(route_event)
                            } else {
                                error!("no event subscribre");
                                None
                            };

                        if let Some(resp) = &mut response {
                            resp.attach_chunk_sender(body_sender.unwrap());
                        }
                        /*
                                                    client
                                                        .conn()
                                                        .stream_shutdown(stream_id, quiche::Shutdown::Read, 0)
                                                        .unwrap();
                        */
                        if let Ok((headers, body)) =
                            route_form.build_response(stream_id, &scid, conn_id, response)
                        {
                            match body {
                                Some(body) => {
                                    let (_recv_send_confirmation, header_req) = HeaderRequest::new(
                                        stream_id,
                                        &scid,
                                        headers.clone(),
                                        false,
                                        Some(body),
                                        header_sender,
                                        header_priority,
                                    );
                                    if let Err(_) = chunk_dispatch_channel
                                        .send_to_high_priority_queue(
                                            stream_id,
                                            &scid,
                                            QueuedRequest::new_header(header_req),
                                        )
                                    {
                                        error!("Failed to send header_req")
                                    }
                                }
                                None => {
                                    let (_recv_send_confirmation, header_req) = HeaderRequest::new(
                                        stream_id,
                                        &scid,
                                        headers.clone(),
                                        is_end,
                                        None,
                                        header_sender,
                                        header_priority,
                                    );
                                    if let Err(_) = chunk_dispatch_channel
                                        .send_to_high_priority_queue(
                                            stream_id,
                                            &scid,
                                            QueuedRequest::new_header(header_req),
                                        )
                                    {
                                        error!("Failed to send header_req")
                                    }
                                }
                            }
                        }
                    }
                    H3Method::POST => {
                        chunk_dispatch_channel.insert_new_channel(stream_id, scid);
                        let body_sender =
                            chunk_dispatch_channel.get_low_priority_sender(stream_id, scid);
                        let header_sender =
                            chunk_dispatch_channel.get_high_priority_sender(stream_id, scid);
                        let mut response =
                            if let Some(event_subscriber) = route_form.event_subscriber() {
                                event_subscriber.on_event(route_event)
                            } else {
                                None
                            };
                        if let Some(resp) = &mut response {
                            resp.attach_chunk_sender(body_sender.unwrap())
                        }
                        if let Ok((headers, body)) =
                            route_form.build_response(stream_id, scid, conn_id, response)
                        {
                            match body {
                                Some(body) => {
                                    let (_recv_send_confirmation, header_req) = HeaderRequest::new(
                                                  stream_id,
                                                   &scid,
                                                 headers.clone(),
                                               false,
                                                Some(body),
                                                header_sender,
                                                   crate::request_response::HeaderPriority::SendAdditionnalHeader,
                                                 );

                                    if let Err(_) = chunk_dispatch_channel
                                        .send_to_high_priority_queue(
                                            stream_id,
                                            &scid,
                                            QueuedRequest::new_header(header_req),
                                        )
                                    {
                                        error!("Failed to send header_req")
                                    }
                                    if let Err(e) = waker.wake() {
                                        error!("Failed to wake poll [{:?}]", e);
                                    };
                                }
                                None => {
                                    let (_recv_send_confirmation, header_req) = HeaderRequest::new(
                                                  stream_id,
                                                   &scid,
                                                 headers.clone(),
                                               true,
                                            None,
                                            header_sender,
                                                   crate::request_response::HeaderPriority::SendAdditionnalHeader,
                                                 );
                                    if let Err(_) = chunking_station
                                        .get_chunking_dispatch_channel()
                                        .send_to_high_priority_queue(
                                            stream_id,
                                            &scid,
                                            QueuedRequest::new_header(header_req),
                                        )
                                    {
                                        error!("Failed to send header_req")
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}
