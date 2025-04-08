# QuicSyncH3 🚀
![Build_Status](https://img.shields.io/badge/build-ok-green)
![dev_status](https://img.shields.io/badge/dev--status-WIP-pink
)

## Purpose 
Quicsynh3 is a lightweight framework that positions itself as an abstraction layer on top of the Quiche implementation of the QUIC protocol. Written in Rust, it aims to simplify the development of modern networking applications using QUIC and HTTP/3.

### ⚠️ This crate is currently in very early development stage. 
- It is nowhere ready for real porduction implementation. Use with care.

## 🏹 Goals
- Providing a simple abstraction on top of Quiche with a quick setup

## Installation
```bash
git clone https://github.com/Cm3lp8/faces_http3_server/
cd faces_http3_server && cargo run --release

```

## How to start

### Create the router with an application State
```rust

 #[derive(Clone)]
    pub struct AppStateTest;
    let mut router = RouteManager::new_with_app_state(AppStateTest);

```
### Instantiate the server
and attach the created router, alongside the configuration pathes.
```rust
let _server = Http3Server::new(addr)
        .add_key_path("./key.pem")
        .add_cert_path("./cert.pem")
        .set_file_storage_path("~/.temp_server/")
        .run_blocking(router);


```
### Create a route handler
``` rust
  let handler = route_handle!(
        |event: FinishedEvent, current_status_response: RouteResponse| {
            info!(
                "Received Data on file path [{}] on [{:?}] ",
                event.bytes_written(),
                event.path()
            );
            Response::ok_200_with_data(event, vec![0; 1888835])
        }
    );

```
The `route_handle!` macro takes a closure with `Event` and `RouteResponse`. It is called when a request is finished. 
You can have a collection of handlers registered to a same route so the Event and RouteResponse parameters are 
successivly passed in and owned by all the handlers processed in the iteration.

- `Event : owned by the closure, all the data about the finished request, including the payload data (as bytes or file path) if any. You have to yield it back for the rest of the iteration. It is the last processed Handler's response that is send to peer.
- `RouteResponse` : the response returned by the previous handler processed.
### Create a middleware
``` rust

let middle_ware = middleware!(AppStateTest, |headers, app_state| {
        MiddleWareFlow::Continue
        //MiddleWareFlow::Abort(ErrorResponse::Error401(None)) --> this aborts the request and send an error response to the peer.
    });



```
The `middleware!` macro takes the type registered on the router for the application state. The state can be accessed by the the second parameter of the closure.
`header` parameter is a `&mut[h3::Header]` that can be mutably borrowed by the middleware.

Same as the handlers, multiple middlewares can be registered on the same route. They are processed with the same order as their registration order on the route.

### Define a route with handler(s) and middleware(s)
Example of for a request `POST`
```rust
router.route_post(
        "/large_data",
        RouteConfig::new(DataManagement::Storage(BodyStorage::File)),
        |route_builder| {
            route_builder.middleware(&middle_ware);// can be chained
            route_builder.handler(&handler);// can be chained too.
        },
    );

```
`RouteConfig` is used to configure the route and especially here you can choose the `DataManagement` type between `BodyStorage::InMemory` and `BodyStorage::File`.
