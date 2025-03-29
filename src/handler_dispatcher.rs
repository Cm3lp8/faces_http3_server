pub use dispatcher::RouteEventDispatcher;
pub use handler_trait::RouteHandle;
mod handler_trait {
    use crate::route_events::FinishedEvent;

    use super::dispatcher::Response;

    pub trait RouteHandle {
        fn call(&self, event: FinishedEvent) -> Response {
            Response::new(event)
        }
    }
}
mod dispatcher {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use crate::{route_events::FinishedEvent, H3Method, RouteEvent, RouteForm, RouteHandle};

    pub struct Response {
        finished_event: Option<FinishedEvent>,
    }
    impl Response {
        pub fn new(from_event: FinishedEvent) -> Response {
            Response {
                finished_event: Some(from_event),
            }
        }
        pub fn retake_event(&mut self) -> FinishedEvent {
            let event = std::mem::replace(&mut self.finished_event, None);
            event.unwrap()
        }
    }

    pub struct RouteEventDispatcher {
        inner: Arc<
            Mutex<HashMap<(String, H3Method), Vec<Arc<dyn RouteHandle + Sync + Send + 'static>>>>,
        >,
    }
    type ReqPath = &'static str;
    impl RouteEventDispatcher {
        pub fn new() -> Self {
            Self {
                inner: Arc::new(Mutex::new(HashMap::new())),
            }
        }
        pub fn set_handles<S: Sync + Send + 'static>(
            &mut self,
            routes_formats: &HashMap<ReqPath, Vec<RouteForm<S>>>,
        ) {
            let guard = &mut *self.inner.lock().unwrap();

            for (path, route_coll) in routes_formats.iter() {
                for route in route_coll.iter() {
                    guard
                        .entry((path.to_string(), *route.method()))
                        .and_modify(|coll| coll.extend(route.handles()))
                        .or_insert(route.handles());
                }
            }
        }
        pub fn dispatch_finished(&self, mut event: FinishedEvent) {
            let path = event.path();
            let method = event.method();
            if let Some(entry) = self.inner.lock().unwrap().get(&(path.to_string(), method)) {
                for handle in entry {
                    event = handle.call(event).retake_event();
                }
            }
        }
    }
    impl Clone for RouteEventDispatcher {
        fn clone(&self) -> Self {
            Self {
                inner: self.inner.clone(),
            }
        }
    }
}
