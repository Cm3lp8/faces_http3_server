pub use job::MiddleWareJob;
pub use thread_pool::ThreadPool;
mod thread_pool {
    use std::thread::JoinHandle;

    use crate::{MiddleWareFlow, MiddleWareResult};

    use super::job::MiddleWareJob;

    pub struct ThreadPool<S: Send + Sync + 'static + Clone> {
        workers: Vec<Worker>,
        job_channel: (
            crossbeam_channel::Sender<MiddleWareJob<S>>,
            crossbeam_channel::Receiver<MiddleWareJob<S>>,
        ),
    }

    impl<S: Send + Sync + 'static + Clone> ThreadPool<S> {
        pub fn new(amount: usize, app_state: S) -> Self {
            let mut workers = Vec::with_capacity(amount);
            let job_channel = crossbeam_channel::unbounded::<MiddleWareJob<S>>();

            for i in 0..workers.len() {
                workers.push(Worker::new(i, job_channel.1.clone(), app_state.clone()));
            }
            Self {
                workers,
                job_channel,
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
        fn new<S: Send + Sync + 'static + Clone>(
            id: usize,
            receiver: crossbeam_channel::Receiver<MiddleWareJob<S>>,
            app_state: S,
        ) -> Self {
            Self {
                id,
                thread: std::thread::spawn(move || {
                    'injection: while let Ok(mut middleware_job) = receiver.recv() {
                        let mut headers = middleware_job.take_headers();
                        let stream_id = middleware_job.stream_id();
                        let scid = middleware_job.scid();

                        for mdw in &mut middleware_job.middleware_collection() {
                            if let MiddleWareFlow::Abort(error_response) =
                                mdw(&mut headers, &app_state)
                            {
                                if let Err(r) = middleware_job.send_done(MiddleWareResult::Abort {
                                    error_response,
                                    stream_id,
                                    scid,
                                }) {
                                    error!("Failed sending middleware result")
                                }

                                continue 'injection;
                            }
                        }

                        // Every middleware have been processed successfully
                        if let Err(r) = middleware_job.send_done(MiddleWareResult::Success {
                            headers,
                            stream_id,
                            scid,
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
    use quiche::h3::{self, Header};

    use crate::{HeadersColl, MiddleWare, MiddleWareFlow, MiddleWareResult};

    pub struct MiddleWareJob<S: Send + Clone + Sync + 'static> {
        stream_id: u64,
        scid: Vec<u8>,
        headers: Vec<h3::Header>,
        middleware_collection:
            Vec<Box<dyn FnMut(&mut [Header], &S) -> MiddleWareFlow + Send + Sync + 'static>>,
        task_done_sender: crossbeam_channel::Sender<MiddleWareResult>,
    }

    impl<S: Send + Sync + 'static + Clone> MiddleWareJob<S> {
        pub fn new(
            stream_id: u64,
            scid: Vec<u8>,
            headers: Vec<h3::Header>,
            middleware_collection: Vec<
                Box<dyn FnMut(&mut [Header], &S) -> MiddleWareFlow + Send + Sync + 'static>,
            >,
            task_done_sender: crossbeam_channel::Sender<MiddleWareResult>,
        ) -> Self {
            Self {
                stream_id,
                scid,
                headers,
                middleware_collection,
                task_done_sender,
            }
        }
        pub fn stream_id(&self) -> u64 {
            self.stream_id
        }
        pub fn scid(&self) -> Vec<u8> {
            self.scid.to_vec()
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
        ) -> Vec<Box<dyn FnMut(&mut [Header], &S) -> MiddleWareFlow + Send + Sync + 'static>>
        {
            std::mem::replace(&mut self.middleware_collection, vec![])
        }
    }
}
