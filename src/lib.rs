#[macro_use]
extern crate log;

pub use server_config::{
    H3Method, RequestForm, RequestManager, RequestManagerBuilder, RequestType, ServerConfig,
};
mod request_events;
mod request_handler;
mod request_manager;
mod request_response;
mod server_config;
mod server_init;
pub use server_init::Http3Server;
#[cfg(test)]
mod tests {
    use super::*;

    /*
    #[test]
    fn it_works() {
        let result = add(2, 2);
        assert_eq!(result, 4);
    }
    */
}
