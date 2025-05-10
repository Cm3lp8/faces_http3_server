pub use job::MiddleWareJob;
pub use thread_pool::ThreadPool;
mod thread_pool {
    use std::{marker::PhantomData, sync::Arc, thread::JoinHandle};

    use mio::Waker;
    use quiche::h3;

    use crate::{
        file_writer::{self, FileWriterChannel},
        request_response::ChunkingStation,
        response_queue_processing::SignalNewRequest,
        server_config,
        stream_sessions::UserSessions,
        MiddleWareFlow, MiddleWareResult, RouteHandler, ServerConfig,
    };

    use super::job::MiddleWareJob;

    pub struct ThreadPool<S: Send + Sync + 'static + Clone, T: UserSessions<Output = T>> {
        workers: Vec<Worker>,
        job_channel: (
            crossbeam_channel::Sender<MiddleWareJob<S>>,
            crossbeam_channel::Receiver<MiddleWareJob<S>>,
        ),
        user_sessions: PhantomData<T>,
    }

    impl<S: Send + Sync + 'static + Clone, T: UserSessions<Output = T>> ThreadPool<S, T> {
        pub fn new(
            amount: usize,
            app_state: S,
            route_handler: &RouteHandler<S, T>,
            server_config: &Arc<ServerConfig>,
            file_writer_channel: &FileWriterChannel,
            response_signal_sender: &SignalNewRequest,
            chunking_station: &ChunkingStation,
            waker: &Arc<Waker>,
        ) -> Self {
            let mut workers = Vec::with_capacity(amount);
            let job_channel = crossbeam_channel::unbounded::<MiddleWareJob<S>>();

            for i in 0..amount {
                workers.push(Worker::new(
                    i,
                    job_channel.1.clone(),
                    app_state.clone(),
                    route_handler,
                    server_config,
                    file_writer_channel,
                    response_signal_sender,
                    chunking_station.clone(),
                    waker,
                ));
            }
            Self {
                workers,
                job_channel,
                user_sessions: PhantomData::<T>,
            }
        }
        pub fn execute(&self, middleware_job: MiddleWareJob<S>) {
            let _ = self.job_channel.0.send(middleware_job);
        }
    }

    pub struct Worker {
        id: usize,
        thread: JoinHandle<()>,
    }
    impl Worker {
        fn new<S: Send + Sync + 'static + Clone, T: UserSessions<Output = T>>(
            id: usize,
            receiver: crossbeam_channel::Receiver<MiddleWareJob<S>>,
            app_state: S,
            route_handler: &RouteHandler<S, T>,
            server_config: &Arc<ServerConfig>,
            file_writer_channel: &crate::file_writer::FileWriterChannel,
            response_signal_sender: &SignalNewRequest,
            chunking_station: ChunkingStation,
            waker: &Arc<mio::Waker>,
        ) -> Self {
            let server_config = server_config.clone();
            let file_writer_channel = file_writer_channel.clone();
            let route_handler = route_handler.clone();
            let response_signal_sender = response_signal_sender.clone();
            let waker = waker.clone();
            Self {
                id,
                thread: std::thread::spawn(move || {
                    'injection: while let Ok(mut middleware_job) = receiver.recv() {
                        let mut headers = middleware_job.take_headers();
                        let stream_id = middleware_job.stream_id();
                        let scid = middleware_job.scid();
                        let conn_id: String = middleware_job.conn_id();
                        let has_more_frames: bool = middleware_job.has_more_frames();
                        let content_length: Option<usize> = middleware_job.content_length();

                        let path = middleware_job.path();
                        let method = middleware_job.method();
                        let mut temps_headers: Vec<h3::Header> = headers.clone();

                        for mdw in &middleware_job.middleware_collection() {
                            match (mdw.callback())(temps_headers, &app_state) {
                                MiddleWareFlow::Continue(headers) => temps_headers = headers,
                                MiddleWareFlow::Abort(error_response) => {
                                    if let Err(r) =
                                        middleware_job.send_done(MiddleWareResult::Abort {
                                            error_response,
                                            stream_id,
                                            scid,
                                        })
                                    {
                                        error!("Failed sending middleware result")
                                    }

                                    continue 'injection;
                                } //middleware execution
                            }
                        }

                        headers = temps_headers;

                        //if the thread reached here,create an entry in partial response table
                        if route_handler.is_request_set_in_table(stream_id, conn_id.as_str()) {
                            let (data_management_type, event_listener) =
                                route_handler.get_additionnal_attributes(path.as_str(), method);

                            route_handler.complete_request_entry_in_table(
                                stream_id,
                                conn_id.as_str(),
                                method,
                                path.as_str(),
                                &headers,
                                content_length,
                                data_management_type,
                                event_listener,
                            );
                        } else {
                            route_handler.create_new_request_in_table(
                                path.as_str(),
                                stream_id,
                                conn_id.as_str(),
                                method,
                                &headers,
                                content_length,
                                has_more_frames,
                                &server_config,
                                &file_writer_channel,
                            );
                        }

                        if let Err(_) = route_handler.send_reception_status_first(
                            stream_id,
                            &scid,
                            conn_id.as_str(),
                            &chunking_station.get_chunking_dispatch_channel(),
                        ) {
                            error!("Failed to send progress response status")
                        } else {
                        }

                        let _ = waker.wake();
                        if let Err(e) =
                            response_signal_sender.send_signal((stream_id, conn_id.clone()))
                        {
                            error!("Failed sending response signal");
                        }

                        // Every middleware have been processed successfully
                        if let Err(r) = middleware_job.send_done(MiddleWareResult::Success {
                            path,
                            method,
                            headers,
                            stream_id,
                            scid,
                            conn_id,
                            has_more_frames,
                            content_length,
                        }) {
                            error!("Failed sending MiddleWareResult Success")
                        }
                    }
                }),
            }
        }
    }
}

mod job {
    use std::sync::Arc;

    use mio::Waker;
    use quiche::h3::{self, Header};

    use crate::{H3Method, HeadersColl, MiddleWare, MiddleWareFlow, MiddleWareResult};

    pub struct MiddleWareJob<S: Send + Clone + Sync + 'static> {
        path: String,
        method: H3Method,
        stream_id: u64,
        conn_id: String,
        has_more_frames: bool,
        content_length: Option<usize>,
        scid: Vec<u8>,
        headers: Vec<h3::Header>,
        middleware_collection: Vec<Arc<dyn MiddleWare<S> + Send + Sync + 'static>>,
        task_done_sender: crossbeam_channel::Sender<MiddleWareResult>,
    }

    impl<S: Send + Sync + 'static + Clone> MiddleWareJob<S> {
        pub fn new(
            path: &str,
            method: H3Method,
            stream_id: u64,
            scid: Vec<u8>,
            conn_id: String,
            has_more_frames: bool,
            content_length: Option<usize>,
            headers: Vec<h3::Header>,
            middleware_collection: Vec<Arc<dyn MiddleWare<S> + Send + Sync + 'static>>,
            task_done_sender: crossbeam_channel::Sender<MiddleWareResult>,
        ) -> Self {
            Self {
                path: path.to_string(),
                method,
                stream_id,
                scid,
                conn_id,
                has_more_frames,
                content_length,
                headers,
                middleware_collection,
                task_done_sender,
            }
        }
        pub fn path(&self) -> String {
            self.path.to_string()
        }
        pub fn method(&self) -> H3Method {
            self.method
        }
        pub fn stream_id(&self) -> u64 {
            self.stream_id
        }
        pub fn scid(&self) -> Vec<u8> {
            self.scid.to_vec()
        }
        pub fn conn_id(&self) -> String {
            self.conn_id.to_string()
        }
        pub fn has_more_frames(&self) -> bool {
            self.has_more_frames
        }
        pub fn content_length(&self) -> Option<usize> {
            self.content_length.clone()
        }
        pub fn take_headers(&mut self) -> Vec<h3::Header> {
            std::mem::replace(&mut self.headers, vec![])
        }
        pub fn send_done(
            &self,
            msg: MiddleWareResult,
        ) -> Result<(), crossbeam_channel::SendError<MiddleWareResult>> {
            self.task_done_sender.send(msg)
        }
        pub fn middleware_collection(
            &mut self,
        ) -> Vec<Arc<dyn MiddleWare<S> + Send + Sync + 'static>> {
            std::mem::replace(&mut self.middleware_collection, vec![])
        }
    }
}
