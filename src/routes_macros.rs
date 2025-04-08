mod router_handle {

    #[macro_export]
    macro_rules! route_handle {
        ($closure: expr) => {{
            #[derive(Clone, Copy)]
            pub struct Anon;

            impl RouteHandle for Anon {
                fn call(
                    &self,
                    event: FinishedEvent,
                    current_status_response: RouteResponse,
                ) -> Response {
                    $closure(event, current_status_response)
                }
            }

            let hndl: Arc<dyn RouteHandle + Send + Sync> = Arc::new(Anon);
            hndl
        }};
        //in case the type isn't already created
        ($id: ident, $closure: expr) => {{
            impl RouteHandle for $id {
                fn call(
                    &self,
                    event: FinishedEvent,
                    current_status_response: RouteResponse,
                ) -> Response {
                    $closure(&self, event, current_status_response)
                }
            }
        }};
    }
}

mod middleware {

    #[macro_export]
    macro_rules! middleware {
        ($state: ident,$closure: expr) => {{
            #[derive(Clone, Copy)]
            struct Anon;

            impl MiddleWare<$state> for Anon {
                fn callback(
                    &self,
                ) -> Box<
                    dyn FnMut(&mut [h3::Header], &AppStateTest) -> MiddleWareFlow
                        + Send
                        + Sync
                        + 'static,
                > {
                    Box::new($closure)
                }
            }
            let mdw: Arc<dyn MiddleWare<$state> + Send + Sync> = Arc::new(Anon {});
            mdw
        }};
    }
}
