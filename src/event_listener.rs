pub use event_listener_trait::RouteEventListener;
pub use event_loop::EventLoop;

use crate::{EventResponseChannel, RequestResponse, ResponseBuilderSender, RouteEvent};
mod event_listener_trait {
    use crate::{
        route_events::{DataEvent, RouteEvent},
        RequestResponse,
    };

    pub trait RouteEventListener {
        fn on_finished(&self, event: RouteEvent) -> Option<RequestResponse> {
            None
        }
        fn on_data(&self, event: RouteEvent) -> Option<RequestResponse> {
            None
        }
        fn on_event(&self, event: RouteEvent) -> Option<RequestResponse> {
            None
        }
    }
}

mod event_loop {

    use std::sync::Arc;

    use super::*;
    pub struct EventLoop {
        channel: (
            crossbeam_channel::Sender<(RouteEvent, ResponseBuilderSender)>,
            crossbeam_channel::Receiver<(RouteEvent, ResponseBuilderSender)>,
        ),
    }
    impl EventLoop {
        pub fn new() -> Arc<Self> {
            let channel = crossbeam_channel::bounded::<(RouteEvent, ResponseBuilderSender)>(100);

            Arc::new(Self { channel })
        }
        pub fn run(&self, cb: impl Fn(RouteEvent, ResponseBuilderSender) + Send + Sync + 'static) {
            let recv = self.channel.1.clone();
            std::thread::spawn(move || {
                while let Ok((event, response_builder)) = recv.recv() {
                    cb(event, response_builder)
                }
            });
        }
    }
    impl RouteEventListener for EventLoop {
        fn on_event(&self, event: RouteEvent) -> Option<RequestResponse> {
            let (sender, receiver) = EventResponseChannel::new();
            if let Err(e) = self.channel.0.try_send((event, sender)) {
                log::error!("Failed to send event to event loop");
            }
            if let Ok(res) = receiver.wait() {
                Some(res)
            } else {
                None
            }
        }
    }
}
