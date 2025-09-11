use std::io;
use std::net::{SocketAddr, TcpListener};
use socket2::{Domain, Socket, Type};

pub struct TcpListenerOpts {
    pub addr: SocketAddr,
    pub backlog: i32,          // e.g., 1024
    pub reuse_port: bool,      // best-effort on Unix
    pub nonblocking: bool,     // true for tokio
}

impl Default for TcpListenerOpts {
    fn default() -> Self {
        Self {
            addr: "0.0.0.0:0".parse().unwrap(),
            backlog: 1024,
            reuse_port: true,
            nonblocking: true,
        }
    }
}

pub fn build_tcp_listener(opts: TcpListenerOpts) -> io::Result<TcpListener> {
    let domain = match opts.addr {
        SocketAddr::V4(_) => Domain::IPV4,
        SocketAddr::V6(_) => Domain::IPV6,
    };
    let sock = Socket::new(domain, Type::STREAM, None)?;

    sock.set_reuse_address(true)?;

    #[cfg(unix)]
    if opts.reuse_port {
        let _ = sock.set_reuse_port(true);
    }
    if opts.nonblocking {
        sock.set_nonblocking(true)?;
    }

    sock.bind(&opts.addr.into())?;
    sock.listen(opts.backlog)?;
    Ok(sock.into())
}
