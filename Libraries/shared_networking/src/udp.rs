use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket},
    time::Duration,
};

fn bind_multicast(sock: &Socket, addr: &SocketAddr) -> io::Result<()> {
    #[cfg(windows)]
    {
        // Windows must bind ANY:port, not GROUP:port
        let any = match addr {
            SocketAddr::V4(v4) => SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), v4.port()),
            SocketAddr::V6(v6) => SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), v6.port()),
        };
        sock.bind(&SockAddr::from(any))
    }
    #[cfg(unix)]
    {
        // Unix can bind to the group to kernel-filter multicast
        sock.bind(&SockAddr::from(*addr))
    }
}

pub struct UdpRxOpts {
    pub group: SocketAddr,       // multicast group:port
    pub read_timeout: Duration,  // e.g., 200ms
    pub recv_buf_bytes: usize,   // e.g., 8*1024*1024
    pub reuse_port: bool,        // allow multi listeners where supported
    pub v6_ifindex: Option<u32>, // Some(idx) if needed (macOS/Windows), else None
    pub disable_loop: bool,      // disable seeing own packets
}
impl Default for UdpRxOpts {
    fn default() -> Self {
        Self {
            group: "239.0.2.1:1234".parse().unwrap(),
            read_timeout: Duration::from_millis(200),
            recv_buf_bytes: 8 * 1024 * 1024,
            reuse_port: true,
            v6_ifindex: None,
            disable_loop: false,
        }
    }
}

pub fn build_multicast_receiver(opts: UdpRxOpts) -> io::Result<UdpSocket> {
    let domain = if opts.group.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let sock = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;

    sock.set_reuse_address(true)?;

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly",
        target_os = "macos"
    ))]
    if opts.reuse_port {
        let _ = sock.set_reuse_port(true);
    }

    match opts.group.ip() {
        IpAddr::V4(g) => {
            sock.join_multicast_v4(&g, &Ipv4Addr::UNSPECIFIED)?;
            if opts.disable_loop {
                sock.set_multicast_loop_v4(false)?;
            }
        }
        IpAddr::V6(g) => {
            sock.set_only_v6(true)?;
            sock.join_multicast_v6(&g, opts.v6_ifindex.unwrap_or(0))?;
            if opts.disable_loop {
                sock.set_multicast_loop_v6(false)?;
            }
        }
    }

    bind_multicast(&sock, &opts.group)?;
    sock.set_read_timeout(Some(opts.read_timeout))?;
    if opts.recv_buf_bytes > 0 {
        let _ = sock.set_recv_buffer_size(opts.recv_buf_bytes);
    }

    Ok(sock.into())
}

pub struct UdpTxOpts {
    pub dst: SocketAddr,         // multicast dst:port
    pub ttl_v4: Option<u32>,     // Some(2) etc.
    pub hops_v6: Option<u32>,    // Some(2) etc.
    pub v4_if: Option<Ipv4Addr>, // select NIC for v4
    pub v6_ifindex: Option<u32>, // select NIC for v6
    pub snd_buf_bytes: usize,    // e.g., 8*1024*1024
    pub disable_loop: bool,      // disable local loopback of sent packets
}
impl Default for UdpTxOpts {
    fn default() -> Self {
        Self {
            dst: "239.0.2.1:1234".parse().unwrap(),
            ttl_v4: Some(2),
            hops_v6: Some(2),
            v4_if: None,
            v6_ifindex: None,
            snd_buf_bytes: 8 * 1024 * 1024,
            disable_loop: false,
        }
    }
}

pub fn build_multicast_sender(opts: UdpTxOpts) -> io::Result<UdpSocket> {
    let domain = if opts.dst.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let sock = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;

    let any = if opts.dst.is_ipv4() {
        SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0)
    } else {
        SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 0)
    };
    sock.bind(&SockAddr::from(any))?;

    match opts.dst.ip() {
        IpAddr::V4(_) => {
            if let Some(ip) = opts.v4_if {
                sock.set_multicast_if_v4(&ip)?;
            }
            if let Some(ttl) = opts.ttl_v4 {
                sock.set_multicast_ttl_v4(ttl)?;
            }
            if opts.disable_loop {
                sock.set_multicast_loop_v4(false)?;
            }
        }
        IpAddr::V6(_) => {
            sock.set_only_v6(true)?;
            if let Some(idx) = opts.v6_ifindex {
                sock.set_multicast_if_v6(idx)?;
            }
            if let Some(hops) = opts.hops_v6 {
                sock.set_multicast_hops_v6(hops)?;
            }
            if opts.disable_loop {
                sock.set_multicast_loop_v6(false)?;
            }
        }
    }

    if opts.snd_buf_bytes > 0 {
        let _ = sock.set_send_buffer_size(opts.snd_buf_bytes);
    }

    let udp: UdpSocket = sock.into();
    udp.connect(opts.dst)?;
    Ok(udp)
}
