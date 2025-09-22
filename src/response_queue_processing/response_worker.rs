pub use reponse_process_thread_pool::ResponseThreadPool;
mod reponse_process_thread_pool {
    use std::{sync::Arc, thread::JoinHandle};

    use crate::{
        response_queue_processing::{
            response_pool_processing::ResponseInjection, ResponseInjectionBuffer,
            ResponsePoolProcessingSender,
        },
        stream_sessions::UserSessions,
    };

    pub struct ResponseThreadPool {
        workers: Vec<ResponseWorker>,
        job_channel: (
            crossbeam_channel::Sender<ResponseInjection>,
            crossbeam_channel::Receiver<ResponseInjection>,
        ),
    }

    impl ResponseThreadPool {
        pub fn new<S: Sync + Send + 'static + Clone, T: UserSessions<Output = T>>(
            amount: usize,
            job_channel: (
                crossbeam_channel::Sender<ResponseInjection>,
                crossbeam_channel::Receiver<ResponseInjection>,
            ),
            app_state: S,
            worker_cb: Arc<
                impl Fn(ResponseInjection, &ResponseInjectionBuffer<S, T>) + Send + Sync + 'static,
            >,
            response_injection_buffer: &ResponseInjectionBuffer<S, T>,
        ) -> Self {
            let mut workers = Vec::with_capacity(amount);

            for i in 0..amount {
                workers.push(ResponseWorker::new(
                    i,
                    job_channel.1.clone(),
                    app_state.clone(),
                    worker_cb.clone(),
                    response_injection_buffer,
                ));
            }
            Self {
                workers,
                job_channel,
            }
        }
        pub fn execute(&self, response_job: ResponseInjection) {
            let _ = self.job_channel.0.send(response_job);
        }
    }

    pub struct ResponseWorker {
        id: usize,
        thread: JoinHandle<()>,
    }
    impl ResponseWorker {
        pub fn new<S: Send + Clone + Sync + 'static, T: UserSessions<Output = T>>(
            id: usize,
            injection_channel: crossbeam_channel::Receiver<ResponseInjection>,
            app_state: S,
            worker_cb: Arc<
                impl Fn(ResponseInjection, &ResponseInjectionBuffer<S, T>) + Send + Sync + 'static,
            >,
            response_injection_buffer: &ResponseInjectionBuffer<S, T>,
        ) -> Self {
            let response_injection_buffer_clone = response_injection_buffer.clone();
            let worker = std::thread::spawn(move || {
                while let Ok(response_injection) = injection_channel.recv() {
                    worker_cb(response_injection, &response_injection_buffer_clone);
                }
            });

            ResponseWorker { id, thread: worker }
        }
    }
}
