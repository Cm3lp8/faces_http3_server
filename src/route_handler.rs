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
        fs::File,
        io::BufWriter,
        path::PathBuf,
        sync::{Arc, Mutex, MutexGuard},
        time::Duration,
    };

    use crate::{
        file_writer::{FileWritable, FileWriterChannel, FileWriterHandle},
        handler_dispatcher::RouteEventDispatcher,
        header_queue_processing::{HeaderMessage, MiddleWareJob, RouteType},
        middleware,
        request_response::{
            BodyRequest, ChunkSender, ChunkingStation, ChunksDispatchChannel, HeaderPriority,
            HeaderRequest,
        },
        response_queue_processing::{self, ResponseInjection, ResponsePoolProcessingSender},
        route_events::{self, EventType, RouteEvent},
        route_manager::{DataManagement, RouteManagerInner},
        server_config,
        server_init::quiche_http3_server::{self, Client},
        stream_sessions::{StreamSessions, UserSessions},
        BodyStorage, FinishedEvent, HeadersColl, MiddleWare, MiddleWareFlow, MiddleWareResult,
        RequestResponse, RouteEventListener, RouteResponse, ServerConfig,
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

    pub struct RouteHandler<S: Sync + Send + 'static, T: UserSessions<Output = T>> {
        inner: Arc<RouteManagerInner<S, T>>,
    }
    impl<S: Sync + Send + 'static, T: UserSessions<Output = T>> Clone for RouteHandler<S, T> {
        fn clone(&self) -> Self {
            Self {
                inner: self.inner.clone(),
            }
        }
    }

    impl<S: Send + Sync + 'static + Clone, T: UserSessions<Output = T>> RouteHandler<S, T> {
        pub fn new(route_mngr_inner: Arc<RouteManagerInner<S, T>>) -> Self {
            RouteHandler {
                inner: route_mngr_inner,
            }
        }
        pub fn app_state(&self) -> S {
            let guard = &*self.inner;
            guard.app_state().clone()
        }
        pub fn get_routes_from_path_and_method<'b>(
            &self,
            path: &'b str,
            methode: H3Method,
        ) -> Option<(Arc<RouteForm<S>>, Option<ReqArgs>)> {
            //if param in path
            //

            let (path, req_args) = ReqArgs::parse_args(&path);

            if let Some(route_coll) = self.inner.get_routes_from_path(path.as_str()) {
                if let Some(found_route) = route_coll.iter().find(|item| item.method() == &methode)
                {
                    return Some((found_route.clone(), req_args));
                }
                None
            } else {
                None
            }
        }
        pub fn send_header_work(
            &self,
            path: &str,
            route_type: RouteType,
            method: H3Method,
            content_length: Option<usize>,
            middleware_collection: Vec<Arc<dyn MiddleWare + Send + Sync + 'static>>,
            msg: HeaderMessage,
            middleware_result_sender: crossbeam_channel::Sender<MiddleWareResult>,
        ) -> Option<MiddleWareJob> {
            // let guard = &*self.inner.lock().unwrap();

            let stream_id = msg.stream_id();
            let scid = msg.scid();
            let conn_id = msg.conn_id();
            let has_more_frames = msg.more_frames();
            let headers = msg.headers();
            let middleware_job = MiddleWareJob::new(
                path,
                method,
                route_type,
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
            let guard = &*self.inner;
            let conn_id = client.conn_ref().trace_id();
            guard
                .routes_states()
                .set_intermediate_headers_send(stream_id, conn_id.to_string());
        }
        pub fn stream_sessions(&self) -> Option<StreamSessions<T>> {
            let guard = &*self.inner;

            if let Some(stream_sessions) = guard.stream_sessions() {
                Some(stream_sessions.clone())
            } else {
                info!("no stream_sessions");
                None
            }
        }
        pub fn process_handler(
            &self,
            stream_id: u64,
            conn_id: &str,
            scid: &[u8],
            event: FinishedEvent,
        ) -> Option<RequestResponse> {
            let inner = &self.inner;
            match inner
                .route_event_dispatcher()
                .dispatch_finished(event, &inner.app_state())
            {
                RouteResponse::OK200 => Some(RequestResponse::new_ok_200(stream_id, scid, conn_id)),
                RouteResponse::OK200_FILE(path) => Some(RequestResponse::new_200_with_file(
                    stream_id, scid, conn_id, path,
                )),
                RouteResponse::OK200_DATA(buf) => Some(RequestResponse::new_200_with_data(
                    stream_id, scid, conn_id, buf,
                )),
                RouteResponse::OK200_JSON(buf) => Some(RequestResponse::new_200_with_json(
                    stream_id, scid, conn_id, buf,
                )),
                RouteResponse::ERROR409(buf) => Some(RequestResponse::new_409_with_data(
                    stream_id, scid, conn_id, buf,
                )),
                RouteResponse::ERROR503(buf) => Some(RequestResponse::new_503_with_data(
                    stream_id, scid, conn_id, buf,
                )),
                RouteResponse::ERROR401(buf) => Some(RequestResponse::new_401_with_data(
                    stream_id, scid, conn_id, buf,
                )),
                RouteResponse::ERROR422(buf) => Some(RequestResponse::new_422_with_data(
                    stream_id, scid, conn_id, buf,
                )),
                RouteResponse::ERROR428(buf) => Some(RequestResponse::new_428_with_data(
                    stream_id, scid, conn_id, buf,
                )),
            }
        }
        pub fn send_reception_status_first(
            &self,
            stream_id: u64,
            scid: &[u8],
            conn_id: &str,
            chunk_dispatch_channel: &ChunksDispatchChannel,
        ) -> Result<usize, ()> {
            let inner = &*self.inner;
            let sender = chunk_dispatch_channel.insert_new_channel(stream_id, &scid);

            let headers = vec![
                h3::Header::new(b":status", b"100"),
                h3::Header::new(b"x-for", stream_id.to_string().as_bytes()),
                h3::Header::new(b"x-progress", b"0"),
            ];

            //if !header_send {
            /*
                                        guard
                                            .routes_states()
                                            .set_intermediate_headers_send(stream_id, conn_id.to_string());
            */

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
            //}
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
            let inner = &*self.inner;
            let scid = client.conn().source_id().as_ref().to_vec();
            let reception_status = inner
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
            let inner = &self.inner;
            let written_data = inner.routes_states().write_body_packet(
                conn_id.to_owned(),
                scid,
                stream_id,
                packet,
                end,
            );
            written_data
        }
        pub fn print_entries(&self) {
            let inner = &*self.inner;
            for i in inner.routes_formats().iter() {
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
            let inner = &self.inner;
            cb(inner.routes_formats().get(path));
        }
        pub fn handle_finished_stream(
            &self,
            conn_id: &str,
            scid: &[u8],
            stream_id: u64,
            waker: &Arc<Waker>,
            response_injection_sender: &ResponsePoolProcessingSender,
        ) {
            if let Err(e) = response_injection_sender.send(
                stream_id, scid, conn_id, false, // content_length,
                waker,
            ) {
                error!("failed response injection on handle finished stream send ");
            }
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
            let inner = &self.inner;
            inner.routes_states().add_partial_request(
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
        pub fn inner(&self, cb: impl FnOnce(&RouteManagerInner<S, T>)) {
            let inner = &self.inner;
            cb(inner);
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
        pub fn get_additionnal_attributes(
            &self,
            path: &str,
            method: H3Method,
        ) -> (
            Option<DataManagement>,
            Option<Arc<dyn RouteEventListener + Send + Sync + 'static>>,
        ) {
            let inner = &self.inner;

            if let Some((route_form, _)) = inner.get_routes_from_path_and_method(path, method) {
                return (
                    route_form.data_management_type(),
                    route_form.event_subscriber(),
                );
            }
            (None, None)
        }

        pub fn complete_request_entry_in_table(
            &self,
            server_config: &Arc<ServerConfig>,
            stream_id: u64,
            conn_id: &str,
            method: H3Method,
            path: &str,
            headers: &[h3::Header],
            content_length: Option<usize>,
            data_management_type: Option<DataManagement>,
            event_subscriber: Option<Arc<dyn RouteEventListener + 'static + Send + Sync>>,
        ) {
            let inner = &self.inner;

            let mut file_storage = None::<PathBuf>;
            let mut file_open = None::<FileWriterHandle<std::fs::File>>;
            match response_queue_processing::utils::build_temp_stage_file_storage_path(
                server_config,
                headers,
                &data_management_type,
            ) {
                Some(file_s) => {
                    file_storage = Some(file_s.0);
                    file_open = Some(file_s.1)
                }
                None => {}
            };

            inner.routes_states().complete_request_entry_in_table(
                stream_id,
                conn_id,
                method,
                path,
                headers,
                content_length,
                data_management_type,
                event_subscriber,
                file_storage,
                file_open,
            )
        }
        pub fn create_new_request_in_table(
            &self,
            path: &str,
            stream_id: u64,
            conn_id: &str,
            method: H3Method,
            headers: &[h3::Header],
            content_length: Option<usize>,
            more_frames: bool,
            server_config: &Arc<ServerConfig>,
            file_writer_channel: &FileWriterChannel,
        ) {
            let inner = &self.inner;
            let mut data_management: Option<DataManagement> =
                Some(DataManagement::Storage(BodyStorage::InMemory));
            let mut event_subscriber: Option<Arc<dyn RouteEventListener + Sync + Send>> = None;
            if let Some((route_form, req_args)) =
                inner.get_routes_from_path_and_method(path, method)
            {
                data_management = route_form.data_management_type();
                event_subscriber = route_form.event_subscriber();
                //   event_subscriber.as_ref().unwrap().on_header();
            }

            inner.routes_states().add_partial_request(
                server_config,
                conn_id.to_string(),
                stream_id,
                method,
                data_management,
                event_subscriber.clone(),
                &headers,
                path,
                content_length,
                !more_frames,
                file_writer_channel.clone(),
            );
        }
        pub fn is_request_set_in_table(&self, stream_id: u64, conn_id: &str) -> bool {
            let inner = &self.inner;

            inner
                .routes_states()
                .is_entry_partial_reponse_set(stream_id, conn_id)
        }
        pub fn routes_states(&self) -> &RequestsTable {
            self.inner.routes_states()
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
                crate::request_response::BodyType::StreamData {
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
        stream_sessions::{StreamHandleCallback, StreamManagement, UserSessions},
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

    pub fn send_error<S: Send + Sync + 'static + Clone, T: UserSessions<Output = T>>(
        error_type: ErrorResponse,
        guard: &RouteManagerInner<S, T>,
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
                log::info!("in send error");
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
                log::info!("in send error");
                let headers = vec![h3::Header::new(b":status", b"403")];
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
                log::info!("in send error");
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
    pub fn send_404<S: Send + Sync + 'static + Clone, T: UserSessions<Output = T>>(
        req_path: &str,
        guard: &RouteManagerInner<S, T>,
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
        } else {
            let header_sender = chunk_dispatch_channel.get_high_priority_sender(stream_id, &scid);
            chunk_dispatch_channel.insert_new_channel(stream_id, &scid);
            let (_recv_send_confirmation, header_req) = HeaderRequest::new(
                stream_id,
                &scid,
                vec![h3::Header::new(b":status", b"404")],
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
    pub fn response_preparation_with_route_handler<
        S: Send + Sync + 'static + Clone,
        T: UserSessions<Output = T>,
    >(
        route_handler: &RouteHandler<S, T>,
        waker: &mio::Waker,
        chunking_station: &ChunkingStation,
        conn_id: &str,
        scid: &[u8],
        stream_id: u64,
        event_type: EventType,
        header_priority: HeaderPriority,
    ) {
        let chunk_dispatch_channel = chunking_station.get_chunking_dispatch_channel();
        let mut route_event: Option<RouteEvent> = None;
        if let Ok(rt_event) = route_handler
            .routes_states()
            .build_route_event(conn_id, scid, stream_id, event_type)
        {
            route_event = Some(rt_event);
        }
        chunk_dispatch_channel.insert_new_channel(stream_id, &scid);
        let header_sender = chunk_dispatch_channel.get_high_priority_sender(stream_id, &scid);
        let body_sender = chunk_dispatch_channel.get_low_priority_sender(stream_id, &scid);

        let mut path = String::new();
        let route_form = if let Some(ref route_event) = route_event {
            path = route_event.path().to_string();
            let route_form = if let Some((route_form, _)) = route_handler
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

        // If route forme none, maybe path will match in streams table
        if route_form.is_none() {
            if let Some(stream_sessions) = route_handler.stream_sessions() {
                if let Some(route_event) = route_event {
                    let app_state = route_handler.app_state();
                    let Ok(callback) =
                        stream_sessions.get_stream_from_path(path.as_str(), |stream| {
                            match route_event {
                                RouteEvent::OnFinished(event) => {
                                    let callback = stream.stream_handler_callback().clone();

                                    Some((event, callback))
                                }
                                _ => None,
                            }
                        })
                    else {
                        return;
                    };

                    if let Some((event, callback)) = callback {
                        if let Ok(res) = callback.call(event, &stream_sessions, &app_state) {
                        } else {
                            error!("Stream sessions not called ! []");

                            // send error + close mecanism
                        }
                    }
                }
            };
        } else {
            let mut is_end = true;

            info!(
                "before handler process stream_id [{:?}] route_event is_some [{:?}]",
                stream_id,
                route_event.is_some()
            );
            let mut response = if let Some(route_event) = route_event {
                match route_event {
                    RouteEvent::OnFinished(event) => {
                        is_end = event.is_end();
                        route_handler.process_handler(stream_id, conn_id, scid, event)
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
                                    if let Err(e) = waker.wake() {
                                        error!("Failed to wake poll [{:?}]", e);
                                    };
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
                                    if let Err(e) = waker.wake() {
                                        error!("Failed to wake poll [{:?}]", e);
                                    };
                                }
                            }
                        }
                    }
                    H3Method::DELETE => {
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
                                    if let Err(e) = waker.wake() {
                                        error!("Failed to wake poll [{:?}]", e);
                                    };
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    pub fn response_preparation<S: Send + Sync + 'static + Clone, T: UserSessions<Output = T>>(
        guard: &RouteManagerInner<S, T>,
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
