use std::io;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::sync::Arc;

use net2;
use tokio_proto::BindServer;
use futures::stream::Stream;
use futures::future::{self, Then, Future};
use tokio_core::net::{TcpStream, TcpListener};
use tokio_core::reactor::{Core, Handle};
use tokio_service::{NewService, Service};
use log::{error, trace};

// TODO: Add more options, e.g.:
// - max concurrent requests
// - request timeout
// - read timeout
// - write timeout
// - max idle time
// - max lifetime

/// A builder for TCP servers.
///
/// Setting up a server needs, at minimum:
///
/// - A server protocol implementation
/// - An address
/// - A service to provide
///
/// In addition to those basics, the builder provides some additional
/// configuration, which is expected to grow over time.
///
/// See the crate docs for an example.
#[derive(Debug)]
pub struct TcpServer<Kind, P> {
    _kind: PhantomData<Kind>,
    proto: Arc<P>,
    threads: usize,
    addr: SocketAddr,
}

impl<Kind, P> TcpServer<Kind, P> where
    P: BindServer<Kind, TcpStream> + Send + Sync + 'static
{
    /// Starts building a server for the given protocol and address, with
    /// default configuration.
    ///
    /// Generally, a protocol is implemented *not* by implementing the
    /// `BindServer` trait directly, but instead by implementing one of the
    /// protocol traits:
    ///
    /// - `pipeline::ServerProto`
    /// - `multiplex::ServerProto`
    /// - `streaming::pipeline::ServerProto`
    /// - `streaming::multiplex::ServerProto`
    ///
    /// See the crate documentation for more details on those traits.
    pub fn new(protocol: P, addr: SocketAddr) -> TcpServer<Kind, P> {
        TcpServer {
            _kind: PhantomData,
            proto: Arc::new(protocol),
            threads: 1,
            addr: addr,
        }
    }

    /// Set the address for the server.
    pub fn addr(&mut self, addr: SocketAddr) {
        self.addr = addr;
    }

    /// Set the number of threads running simultaneous event loops (Unix only).
    pub fn threads(&mut self, threads: usize) {
        assert!(threads > 0);
        if cfg!(unix) {
            self.threads = threads;
        }
    }

    /// Start up the server, providing the given service on it.
    ///
    /// This method will block the current thread until the server is shut down.
    pub fn serve<S, Sd>(&self, new_service: S) where
        S: NewService + Send + Sync + 'static,
        S::Instance: 'static,
        P::ServiceError: 'static,
        P::ServiceResponse: 'static,
        P::ServiceRequest: 'static,
        S::Request: From<P::ServiceRequest>,
        S::Response: Into<P::ServiceResponse>,
        S::Error: Into<P::ServiceError>,
    {
        self.serve_until(new_service, future::empty())
    }

    /// Start up the server, providing the given service on it.
    ///
    /// This method will block the current thread until the server is shut down or until the given future, `shutdown_signal` resolves.
    pub fn serve_until<S, Sd>(&self, new_service: S, shutdown_signal: Sd) where
        S: NewService + Send + Sync + 'static,
        S::Instance: 'static,
        Sd: Future<Item = (), Error = std::io::Error> + Send + Sync + 'static,
        P::ServiceError: 'static,
        P::ServiceResponse: 'static,
        P::ServiceRequest: 'static,
        S::Request: From<P::ServiceRequest>,
        S::Response: Into<P::ServiceResponse>,
        S::Error: Into<P::ServiceError>,
    {
        let new_service = Arc::new(new_service);
        self.with_handle(move |_| new_service.clone(), shutdown_signal)
    }



    /// Start up the server, providing the given service on it, and providing
    /// access to the event loop handle.
    ///
    /// The `new_service` argument is a closure that is given an event loop
    /// handle, and produces a value implementing `NewService`. That value is in
    /// turn used to make a new service instance for each incoming connection.
    ///
    /// This method will block the current thread until the server is shut down.
    pub fn with_handle<F, S, Sd>(&self, new_service: F, shutdown_signal: Sd) where
        F: Fn(&Handle) -> S + Send + Sync + 'static,
        S: NewService + Send + Sync + 'static,
        Sd: Future<Item = (), Error = std::io::Error> + Send + Sync + 'static,
        S::Instance: 'static,
        P::ServiceError: 'static,
        P::ServiceResponse: 'static,
        P::ServiceRequest: 'static,
        S::Request: From<P::ServiceRequest>,
        S::Response: Into<P::ServiceResponse>,
        S::Error: Into<P::ServiceError>,
    {
        let proto = self.proto.clone();
        let new_service = Arc::new(new_service);
        let addr = self.addr;
        let workers = self.threads;

        // let threads = (0..self.threads - 1).map(|i| {
        //     let proto = proto.clone();
        //     let new_service = new_service.clone();

        //     thread::Builder::new().name(format!("worker{}", i)).spawn(move || {
        //         serve(proto, addr, workers, &*new_service, shutdown_signal)
        //     }).unwrap()
        // }).collect::<Vec<_>>();

        serve_until(proto, addr, workers, &*new_service, shutdown_signal);

        // for thread in threads {
        //     thread.join().unwrap();
        // }
    }
}

fn serve_until<P, Kind, F, S, Sd>(binder: Arc<P>, addr: SocketAddr, workers: usize, new_service: &F, shutdown_signal: Sd)
    where P: BindServer<Kind, TcpStream>,
          F: Fn(&Handle) -> S,
          S: NewService + Send + Sync,
          Sd: Future<Item = (), Error = std::io::Error> + Send + Sync + 'static,
          S::Instance: 'static,
          P::ServiceError: 'static,
          P::ServiceResponse: 'static,
          P::ServiceRequest: 'static,
          S::Request: From<P::ServiceRequest>,
          S::Response: Into<P::ServiceResponse>,
          S::Error: Into<P::ServiceError>,
{
    struct WrapService<S, Request, Response, Error> {
        inner: S,
        _marker: PhantomData<fn() -> (Request, Response, Error)>,
    }

    impl<S, Request, Response, Error> Service for WrapService<S, Request, Response, Error>
        where S: Service,
              S::Request: From<Request>,
              S::Response: Into<Response>,
              S::Error: Into<Error>,
    {
        type Request = Request;
        type Response = Response;
        type Error = Error;
        type Future = Then<S::Future,
                           Result<Response, Error>,
                           fn(Result<S::Response, S::Error>) -> Result<Response, Error>>;

        fn call(&self, req: Request) -> Self::Future {
            fn change_types<A, B, C, D>(r: Result<A, B>) -> Result<C, D>
                where A: Into<C>,
                      B: Into<D>,
            {
                match r {
                    Ok(e) => Ok(e.into()),
                    Err(e) => Err(e.into()),
                }
            }

            self.inner.call(S::Request::from(req)).then(change_types)
        }
    }

    let mut core = Core::new().unwrap();
    let handle = core.handle();
    let new_service = new_service(&handle);
    let listener = listener(&addr, workers, &handle).unwrap();

    let server = listener.incoming().for_each(move |(socket, _)| {
        // Create the service
        let service = new_service.new_service()?;

        // Bind it!
        binder.bind_server(&handle, socket, WrapService {
            inner: service,
            _marker: PhantomData,
        });

        Ok(())
    });

    match core.run(shutdown_signal.select(server.map_err(|e| e.into()))) {
        Ok(((), _incoming)) => { trace!("Shutdown signal received") }
        Err((e, _other)) => { error!("error: {}", e) },
    }
}

fn listener(addr: &SocketAddr,
            workers: usize,
            handle: &Handle) -> io::Result<TcpListener> {
    let listener = match *addr {
        SocketAddr::V4(_) => net2::TcpBuilder::new_v4()?,
        SocketAddr::V6(_) => net2::TcpBuilder::new_v6()?,
    };
    configure_tcp(workers, &listener)?;
    listener.reuse_address(true)?;
    listener.bind(addr)?;    
    listener.listen(1024).and_then(|l| {
        TcpListener::from_listener(l, addr, handle)
    })
}

#[cfg(unix)]
fn configure_tcp(workers: usize, tcp: &net2::TcpBuilder) -> io::Result<()> {
    use net2::unix::*;

    if workers > 1 {
        tcp.reuse_port(true)?;
    }
    
    Ok(())
}

#[cfg(windows)]
fn configure_tcp(_workers: usize, _tcp: &net2::TcpBuilder) -> io::Result<()> {
    Ok(())
}
