pub use response_buff::{ResponseInjectionBuffer, SignalNewRequest};
type ReqId = (u64, String);
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RequestDisposition {
    Pending,
    Ready,
    Rejected,
}
type RequestSignal = (ReqId, RequestDisposition);
mod response_buff {
    use dashmap::DashMap;
    use mio::Waker;

    use super::*;
    use std::sync::Arc;

    use crate::{
        file_writer::FileWriter,
        request_response::{ChunkingStation, HeaderPriority},
        response_queue_processing::ResponseInjection,
        route_events::EventType,
        route_handler::response_preparation_with_route_handler,
        stream_sessions::UserSessions,
        RouteHandler, ServerConfig,
    };

    pub struct ResponseInjectionBuffer<S: Send + Sync + 'static, T: UserSessions<Output = T>> {
        channel: (
            crossbeam_channel::Sender<RequestSignal>,
            crossbeam_channel::Receiver<RequestSignal>,
        ),
        route_handler: RouteHandler<S, T>,
        file_writer_manager: Arc<FileWriter>,
        chunking_station: ChunkingStation,
        signal: Arc<DashMap<ReqId, RequestDisposition>>,
        waker: Arc<Waker>,
    }
    impl<S: Send + Sync + 'static, T: UserSessions<Output = T>> Clone
        for ResponseInjectionBuffer<S, T>
    {
        fn clone(&self) -> Self {
            Self {
                channel: self.channel.clone(),
                signal: self.signal.clone(),
                file_writer_manager: self.file_writer_manager.clone(),
                chunking_station: self.chunking_station.clone(),
                waker: self.waker.clone(),
                route_handler: self.route_handler.clone(),
            }
        }
    }

    impl<S: Send + Sync + 'static + Clone, T: UserSessions<Output = T>> ResponseInjectionBuffer<S, T> {
        pub fn new(
            route_handler: RouteHandler<S, T>,
            server_config: &Arc<ServerConfig>,
            file_writer_manager: Arc<FileWriter>,
            chunking_station: ChunkingStation,
            waker: &Arc<Waker>,
        ) -> Self {
            let channel = crossbeam_channel::unbounded();
            let signal: Arc<DashMap<ReqId, RequestDisposition>> = Arc::new(DashMap::new());

            signal_receiver::run(channel.1.clone(), signal.clone(), route_handler.clone());
            Self {
                channel,
                signal,
                file_writer_manager,
                chunking_station,
                route_handler,
                waker: waker.clone(),
            }
        }
        /// [`register()`] is called on  Quiche's Finished Event. It prepares data for response
        /// and call the route handler logic. Before calling the route handler logic,
        /// make sure the middlewares have been processed.
        ///
        /// If [`register()`] can't make the response preparation, it will return the
        /// [`ResponseInjection`] . The [`register()`] caller can requeue it for retry.
        pub fn register(
            &self,
            response_injection: ResponseInjection,
        ) -> Result<(), ResponseInjection> {
            match self
                .signal
                .remove_if(&response_injection.req_id(), |_, state| {
                    *state != RequestDisposition::Pending
                })
                .map(|(_, state)| state)
            {
                Some(RequestDisposition::Ready) => {
                    response_preparation_with_route_handler(
                        &self.route_handler,
                        &self.waker,
                        &self.chunking_station,
                        &response_injection.conn_id(),
                        &response_injection.scid(),
                        response_injection.stream_id(),
                        EventType::OnFinished,
                        HeaderPriority::SendAdditionnalHeader,
                    );
                    Ok(())
                }
                Some(RequestDisposition::Rejected) => {
                    // The error response was already scheduled by header processing.
                    // Discard any body received before rejection; never run the handler.
                    self.route_handler.routes_states().discard_request(
                        &response_injection.conn_id(),
                        response_injection.stream_id(),
                    );
                    Ok(())
                }
                None | Some(RequestDisposition::Pending) => {
                    if self.signal.contains_key(&response_injection.req_id()) {
                        Err(response_injection)
                    } else {
                        // Cancelled, unknown, or already consumed: nothing to retry.
                        Ok(())
                    }
                }
            }
        }
        pub(crate) fn begin_request(&self, conn_id: &str, stream_id: u64) {
            self.signal
                .entry((stream_id, conn_id.to_owned()))
                .or_insert(RequestDisposition::Pending);
        }
        pub(crate) fn cancel_request(&self, conn_id: &str, stream_id: u64) {
            self.signal.remove(&(stream_id, conn_id.to_owned()));
            self.route_handler
                .routes_states()
                .discard_request(conn_id, stream_id);
        }
        pub(crate) fn cancel_connection(&self, conn_id: &str) {
            self.signal
                .retain(|(_, connection), _| connection != conn_id);
            self.route_handler
                .routes_states()
                .discard_connection(conn_id);
        }
        pub fn get_signal_sender(&self) -> SignalNewRequest {
            SignalNewRequest::new(self.channel.0.clone())
        }
    }

    mod response_injection_buffer_implementation {
        //
    }

    #[derive(Clone)]
    pub struct SignalNewRequest {
        sender: crossbeam_channel::Sender<RequestSignal>,
    }

    impl SignalNewRequest {
        pub fn new(sender: crossbeam_channel::Sender<RequestSignal>) -> Self {
            Self { sender }
        }
        pub fn send_signal(
            &self,
            signal: ReqId,
        ) -> Result<(), crossbeam_channel::SendError<RequestSignal>> {
            self.sender.send((signal, RequestDisposition::Ready))
        }
        pub fn send_rejected(
            &self,
            request: ReqId,
        ) -> Result<(), crossbeam_channel::SendError<RequestSignal>> {
            self.sender.send((request, RequestDisposition::Rejected))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::{test_support::TestSessions, H3Method, Response, RouteConfig, RouteManager};
        use std::{
            sync::{
                atomic::{AtomicUsize, Ordering},
                Mutex,
            },
            time::{Duration, Instant},
        };

        #[test]
        fn rejection_before_or_after_finished_is_consumed_without_dispatch_or_retained_body() {
            let calls = Arc::new(AtomicUsize::new(0));
            let mut builder = RouteManager::<_, TestSessions>::new_with_app_state(calls.clone());
            let callback = builder.handler(&|event, state: &Arc<AtomicUsize>, _| {
                state.fetch_add(1, Ordering::SeqCst);
                Response::ok_200(event)
            });
            builder.route_post("/test", RouteConfig::default(), |route| {
                route.handler(&callback);
            });
            let router = builder.build();
            let handler = router.routes_handler();
            let root =
                std::env::temp_dir().join(format!("fragment-disposition-{}", uuid::Uuid::now_v7()));
            let config = Arc::new(
                ServerConfig::new()
                    .set_address("127.0.0.1:0")
                    .set_cert_path(root.join("unused-cert"))
                    .set_key_path(root.join("unused-key"))
                    .set_file_storage_path(&root)
                    .build(),
            );
            let poll = mio::Poll::new().unwrap();
            let waker = Arc::new(Waker::new(poll.registry(), mio::Token(0)).unwrap());
            let station = ChunkingStation::new(waker.clone(), Arc::new(Mutex::new(Duration::ZERO)));
            let writers = Arc::new(FileWriter::new());
            let buffer = ResponseInjectionBuffer::new(
                handler.clone(),
                &config,
                writers.clone(),
                station,
                &waker,
            );
            let sender = buffer.get_signal_sender();
            let mut expected_calls = 0;
            for (id, rejected, finished_first) in [
                (0, true, true),
                (4, true, false),
                (8, false, false),
                (12, false, true),
            ] {
                let key = (id, "connection".to_owned());
                buffer.begin_request("connection", id);
                handler.create_new_request_in_table(
                    "/test",
                    id,
                    "connection",
                    H3Method::POST,
                    &[],
                    Some(0),
                    false,
                    &config,
                    &writers,
                );
                assert_eq!(
                    handler
                        .routes_states()
                        .write_body_packet("connection".into(), &[1], id, &[9; 1024], true)
                        .unwrap(),
                    1024
                );
                let mut injection =
                    ResponseInjection::new(id, &[1], "connection", false, waker.clone());
                if finished_first {
                    injection = match buffer.register(injection) {
                        Err(pending) => pending,
                        Ok(()) => panic!("dispatched before middleware result"),
                    };
                }
                if rejected {
                    sender.send_rejected(key.clone()).unwrap();
                } else {
                    sender.send_signal(key.clone()).unwrap();
                }
                let deadline = Instant::now() + Duration::from_secs(2);
                while buffer
                    .signal
                    .get(&key)
                    .is_none_or(|state| *state == RequestDisposition::Pending)
                {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert!(buffer.register(injection).is_ok());
                assert!(!buffer.signal.contains_key(&key));
                assert!(!handler.is_request_set_in_table(id, "connection"));
                expected_calls += usize::from(!rejected);
                assert_eq!(calls.load(Ordering::SeqCst), expected_calls);
            }
            assert!(buffer.signal.is_empty());
            // Cancellation must preserve a different connection's pending request.
            buffer.begin_request("unrelated", 0);
            handler.create_new_request_in_table(
                "/test",
                0,
                "unrelated",
                H3Method::POST,
                &[],
                Some(0),
                false,
                &config,
                &writers,
            );
            // A late middleware result must not resurrect a cancelled request.
            for (connection, id, whole_connection) in [("reset", 16, false), ("closed", 20, true)] {
                buffer.begin_request(connection, id);
                if whole_connection {
                    buffer.cancel_connection(connection);
                } else {
                    buffer.cancel_request(connection, id);
                }
                // Simulate a worker creating its partial table just after cancellation.
                handler.create_new_request_in_table(
                    "/test",
                    id,
                    connection,
                    H3Method::POST,
                    &[],
                    Some(0),
                    false,
                    &config,
                    &writers,
                );
                sender.send_signal((id, connection.to_owned())).unwrap();
                let deadline = Instant::now() + Duration::from_secs(2);
                while handler.is_request_set_in_table(id, connection) {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert!(!buffer.signal.contains_key(&(id, connection.to_owned())));
                assert!(buffer
                    .register(ResponseInjection::new(
                        id,
                        &[1],
                        connection,
                        false,
                        waker.clone()
                    ))
                    .is_ok());
            }
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            assert!(buffer.signal.contains_key(&(0, "unrelated".to_owned())));
            assert!(handler.is_request_set_in_table(0, "unrelated"));
            buffer.cancel_connection("unrelated");
            assert!(!handler.is_request_set_in_table(0, "unrelated"));
            assert!(buffer.signal.is_empty());
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
mod signal_receiver {

    use super::*;
    use dashmap::DashMap;
    use std::sync::Arc;

    pub fn run<S: Send + Sync + Clone + 'static, T: crate::UserSessions<Output = T>>(
        receiver: crossbeam_channel::Receiver<RequestSignal>,
        signal_set: Arc<DashMap<ReqId, RequestDisposition>>,
        route_handler: crate::RouteHandler<S, T>,
    ) {
        std::thread::spawn(move || {
            while let Ok((request, state)) = receiver.recv() {
                // This signal indicates that the middlewares have been processed
                if let Some(mut active) = signal_set.get_mut(&request) {
                    *active = state;
                } else {
                    // A worker can finish after reset/connection cleanup.
                    route_handler
                        .routes_states()
                        .discard_request(&request.1, request.0);
                }
            }
        });
    }
}
