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
        fn on_header(&self) {}
    }
}

mod event_loop {

    use std::sync::Arc;

    use crate::handler_dispatcher::RouteEventDispatcher;

    use super::*;
    pub struct EventLoop<S: Send + Sync + 'static + Clone> {
        channel: (
            crossbeam_channel::Sender<(RouteEvent, ResponseBuilderSender)>,
            crossbeam_channel::Receiver<(RouteEvent, ResponseBuilderSender)>,
        ),
        route_event_dispatcher: RouteEventDispatcher<S>,
        app_state: S,
    }
    impl<S: Send + Sync + 'static + Clone> EventLoop<S> {
        pub fn new(route_event_dispatcher: RouteEventDispatcher<S>, app_state: S) -> Arc<Self> {
            let channel = crossbeam_channel::bounded::<(RouteEvent, ResponseBuilderSender)>(100);

            Arc::new(Self {
                channel,
                route_event_dispatcher,
                app_state,
            })
        }
        pub fn run(
            &self,
            cb: impl Fn(RouteEvent, &S, &RouteEventDispatcher<S>, ResponseBuilderSender)
                + Send
                + Sync
                + 'static,
        ) {
            let recv = self.channel.1.clone();
            let route_event_dispatcher = self.route_event_dispatcher.clone();
            let app_state = self.app_state.clone();
            std::thread::spawn(move || {
                while let Ok((event, response_builder)) = recv.recv() {
                    cb(event, &app_state, &route_event_dispatcher, response_builder)
                }
            });
        }
    }
    impl<S: Send + Sync + 'static + Clone> RouteEventListener for EventLoop<S> {
        fn on_event(&self, event: RouteEvent) -> Option<RequestResponse> {
            let (sender, receiver) = EventResponseChannel::new(&event);
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
