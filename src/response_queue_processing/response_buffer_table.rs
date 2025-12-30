pub use response_buff::{ResponseInjectionBuffer, SignalNewRequest};
type ReqId = (u64, String);
mod response_buff {
    use mio::Waker;

    use super::*;
    use std::{
        collections::{HashMap, HashSet},
        hash::Hash,
        sync::{mpsc::channel, Arc, Mutex},
    };

    use crate::{
        file_writer::FileWriterChannel,
        request_response::{ChunkingStation, HeaderPriority},
        response_queue_processing::ResponseInjection,
        route_events::EventType,
        route_handler::response_preparation_with_route_handler,
        stream_sessions::UserSessions,
        RouteHandler, ServerConfig,
    };

    pub struct ResponseInjectionBuffer<S: Send + Sync + 'static, T: UserSessions<Output = T>> {
        channel: (
            crossbeam_channel::Sender<ReqId>,
            crossbeam_channel::Receiver<ReqId>,
        ),
        table: Arc<Mutex<HashMap<ReqId, ResponseInjection>>>,
        route_handler: RouteHandler<S, T>,
        file_writer_channel: FileWriterChannel,
        chunking_station: ChunkingStation,
        signal: Arc<Mutex<HashSet<ReqId>>>,
        waker: Arc<Waker>,
    }
    impl<S: Send + Sync + 'static, T: UserSessions<Output = T>> Clone
        for ResponseInjectionBuffer<S, T>
    {
        fn clone(&self) -> Self {
            Self {
                channel: self.channel.clone(),
                table: self.table.clone(),
                signal: self.signal.clone(),
                file_writer_channel: self.file_writer_channel.clone(),
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
            file_writer_channel: FileWriterChannel,
            chunking_station: ChunkingStation,
            waker: &Arc<Waker>,
        ) -> Self {
            let channel = crossbeam_channel::unbounded();
            let signal: Arc<Mutex<HashSet<ReqId>>> = Arc::new(Mutex::new(HashSet::new()));
            let table = Arc::new(Mutex::new(HashMap::new()));

            signal_receiver::run(
                channel.1.clone(),
                signal.clone(),
                table.clone(),
                route_handler.clone(),
                chunking_station.clone(),
                waker,
            );
            Self {
                channel,
                table,
                signal,
                file_writer_channel,
                chunking_station,
                route_handler,
                waker: waker.clone(),
            }
        }
        pub fn register(&self, response_injection: ResponseInjection) {
            info!(
                "NEW injection for  stream_id [{:?}]",
                response_injection.stream_id()
            );
            if self.is_request_signal_in_queue(response_injection.req_id()) {
                //Send immediatly if all the necessary middleware validation and data process is
                //done
                let stream_id = response_injection.stream_id();
                let scid = response_injection.scid();
                let conn_id = response_injection.conn_id();

                response_preparation_with_route_handler(
                    &self.route_handler,
                    &self.waker,
                    &self.chunking_station,
                    conn_id.as_str(),
                    &scid,
                    stream_id,
                    EventType::OnFinished,
                    HeaderPriority::SendAdditionnalHeader,
                );
            } else {
                // If req process (middleware or async data processing) is not finished, wait for
                // it in the table
                //

                info!(
                    "injection B path  stream_id [{:?}]",
                    response_injection.stream_id()
                );
                self.table
                    .lock()
                    .unwrap()
                    .insert(response_injection.req_id(), response_injection);
            }
            ()
        }
        pub fn get_signal_sender(&self) -> SignalNewRequest {
            SignalNewRequest::new(self.channel.0.clone())
        }
        fn is_request_signal_in_queue(&self, response_injection_id: ReqId) -> bool {
            let guard = &mut *self.signal.lock().unwrap();
            if guard.contains(&response_injection_id) {
                guard.remove(&response_injection_id);
                return true;
            }
            false
        }
    }

    mod response_injection_buffer_implementation {
        //
    }

    #[derive(Clone)]
    pub struct SignalNewRequest {
        sender: crossbeam_channel::Sender<ReqId>,
    }

    impl SignalNewRequest {
        pub fn new(sender: crossbeam_channel::Sender<ReqId>) -> Self {
            Self { sender }
        }
        pub fn send_signal(
            &self,
            signal: ReqId,
        ) -> Result<(), crossbeam_channel::SendError<ReqId>> {
            self.sender.send(signal)
        }
    }
}
mod signal_receiver {
    use mio::Waker;

    use crate::{
        request_response::{ChunkingStation, HeaderPriority},
        response_queue_processing::ResponseInjection,
        route_events::EventType,
        route_handler::response_preparation_with_route_handler,
        stream_sessions::UserSessions,
        RouteHandler,
    };

    use super::*;
    use std::{
        collections::{HashMap, HashSet},
        hash::Hash,
        sync::{Arc, Mutex},
        task::Wake,
    };

    pub fn run<S: Send + Sync + 'static + Clone, T: UserSessions<Output = T>>(
        receiver: crossbeam_channel::Receiver<ReqId>,
        signal_set: Arc<Mutex<HashSet<ReqId>>>,
        table: Arc<Mutex<HashMap<ReqId, ResponseInjection>>>,
        route_handler: RouteHandler<S, T>,
        chunking_station: ChunkingStation,
        waker: &Arc<Waker>,
    ) {
        let waker = waker.clone();
        std::thread::spawn(move || {
            while let Ok(signal) = receiver.recv() {
                if let Some(entry) = table.lock().unwrap().get(&signal) {
                    let stream_id = entry.stream_id();
                    let scid = entry.scid();
                    let conn_id = entry.conn_id();

                    response_preparation_with_route_handler(
                        &route_handler,
                        &waker,
                        &chunking_station,
                        conn_id.as_str(),
                        &scid,
                        stream_id,
                        EventType::OnFinished,
                        HeaderPriority::SendAdditionnalHeader,
                    );
                } else {
                    info!("NEW signal_set injection for  stream_id [{:?}]", signal.0);
                    signal_set.lock().unwrap().insert(signal);
                }
            }
        });
    }
}
