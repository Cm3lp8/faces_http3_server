pub use job::MiddleWareJob;
pub use thread_pool::ThreadPool;
mod thread_pool {
    use std::thread::JoinHandle;

    use crate::MiddleWareResult;

    use super::job::MiddleWareJob;

    pub struct ThreadPool {
        workers: Vec<Worker>,
        job_channel: (
            crossbeam_channel::Sender<MiddleWareJob>,
            crossbeam_channel::Receiver<MiddleWareJob>,
        ),
    }

    impl ThreadPool {
        pub fn new(amount: usize) -> Self {
            let mut workers = Vec::with_capacity(amount);
            let job_channel = crossbeam_channel::unbounded::<MiddleWareJob>();

            for i in 0..workers.len() {
                workers.push(Worker::new(i, job_channel.1.clone()));
            }
            Self {
                workers,
                job_channel,
            }
        }
        pub fn execute(&self, middleware_job: MiddleWareJob) {
            let _ = self.job_channel.0.send(middleware_job);
        }
    }

    pub struct Worker {
        id: usize,
        thread: JoinHandle<()>,
    }
    impl Worker {
        fn new(id: usize, receiver: crossbeam_channel::Receiver<MiddleWareJob>) -> Self {
            Self {
                id,
                thread: std::thread::spawn(move || {
                    while let Ok(middleware_job) = receiver.recv() {
                        let header = middleware_job.take_headers();

                        for mdw in middleware_job.middleware_collection() {
                            if let MiddleWareResult::Abort = (mdw)(&mut header) {
                                middleware_job.abort();
                            }
                        }
                    }
                }),
            }
        }
    }
}

mod job {
    use quiche::h3;

    use crate::{HeadersColl, MiddleWareResult};

    pub struct MiddleWareJob {
        headers: Vec<h3::Header>,
        middleware_collection: Vec<Box<dyn FnMut(()) -> MiddleWareResult + Send + Sync + 'static>>,
        task_done_sender: crossbeam_channel::Sender<MiddleWareResult>,
    }

    impl MiddleWareJob {
        pub fn new(
            headers: Vec<h3::Header>,
            middleware_collection: Vec<
                Box<dyn FnMut(()) -> MiddleWareResult + Send + Sync + 'static>,
            >,
            task_done_sender: crossbeam_channel::Sender<MiddleWareResult>,
        ) -> Self {
            Self {
                headers,
                middleware_collection,
                task_done_sender,
            }
        }
    }
}
