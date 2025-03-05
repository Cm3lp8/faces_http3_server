pub use event_listener_trait::RouteEventListener;
mod event_listener_trait {
    use crate::request_events::{DataEvent, RouteEvent};

    pub trait RouteEventListener {
        fn on_finished(&self, event: RouteEvent) {}
        fn on_data(&self, event: DataEvent) {}
    }
}
