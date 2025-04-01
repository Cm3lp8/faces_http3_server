pub use dispatcher::{Response, RouteEventDispatcher};
pub use error_response::ErrorResponse;
pub use handler_trait::RouteHandle;
pub use route_response::RouteResponse;

mod handler_trait {
    use crate::route_events::FinishedEvent;

    use super::{dispatcher::Response, route_response::RouteResponse};

    pub trait RouteHandle {
        ///
        ///___________________
        ///# Taking ownership of Event
        ///
        ///But giving it back.
        ///
        ///Call() method takes the ownership from event and from the status response that was produced
        ///by the previous registered handler on the route.
        ///Event has to be returned (after a possible modification like consumming the data it contains).
        ///
        ///# A status response has to be returned
        ///
        ///It's the last handler in the iterator that returns the response used to send to the peer.
        ///For each handler, the user can re_use the previous status response or create a new one in
        ///function of the context.
        fn call(&self, event: FinishedEvent, status_response: RouteResponse) -> Response {
            Response::ok_200(event)
        }
    }
}
mod error_response {

    use std::path::PathBuf;
    pub enum ErrorResponse {
        /// Unauthorize
        Error401(Option<Vec<u8>>),
        /// Forbidden
        Error403(Option<Vec<u8>>),
        /// Unsupported media type
        Error415(Option<Vec<u8>>),
    }
}
mod route_response {
    use std::path::PathBuf;

    pub enum RouteResponse {
        OK200,
        OK200_DATA(Vec<u8>),
        OK200_FILE(PathBuf),
    }
}
mod dispatcher {
    use std::{
        collections::HashMap,
        path::Path,
        sync::{Arc, Mutex},
    };

    use crate::{route_events::FinishedEvent, H3Method, RouteEvent, RouteForm, RouteHandle};

    use super::route_response::RouteResponse;

    pub struct Response {
        finished_event: Option<FinishedEvent>,
        response: RouteResponse,
    }
    impl Response {
        pub fn ok_200(from_event: FinishedEvent) -> Response {
            Response {
                finished_event: Some(from_event),
                response: RouteResponse::OK200,
            }
        }
        pub fn ok_200_with_file(
            from_event: FinishedEvent,
            file_path: impl AsRef<Path>,
        ) -> Response {
            Response {
                finished_event: Some(from_event),
                response: RouteResponse::OK200_FILE(file_path.as_ref().to_path_buf()),
            }
        }
        pub fn ok_200_with_data(from_event: FinishedEvent, data: Vec<u8>) -> Response {
            Response {
                finished_event: Some(from_event),
                response: RouteResponse::OK200_DATA(data),
            }
        }
        pub fn response(&mut self) -> Response {
            Self {
                finished_event: self.finished_event.take(),
                response: std::mem::replace(&mut self.response, RouteResponse::OK200),
            }
        }
        pub fn take_status_response(&mut self) -> RouteResponse {
            std::mem::replace(&mut self.response, RouteResponse::OK200)
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
        /// The handlers can be chained when registered on a route.
        /// The intermediate status reponse are given to the next handler in the collection
        /// when the dispatcher iterates on it. The last handler's reponse is send to the peer.
        pub fn dispatch_finished(&self, mut event: FinishedEvent) -> RouteResponse {
            let path = event.path();
            let method = event.method();
            let mut status_response: RouteResponse = RouteResponse::OK200;
            if let Some(entry) = self.inner.lock().unwrap().get(&(path.to_string(), method)) {
                for handle in entry {
                    let mut response = handle.call(event, status_response).response();

                    event = response.retake_event();
                    status_response = response.take_status_response();
                }
            }
            status_response
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
