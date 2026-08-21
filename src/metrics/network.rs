use std::net::IpAddr;
use std::time::Instant;

use sysinfo::{InterfaceOperationalState, IpNetwork, NetworkData, Networks};

#[derive(Clone, Debug, Default)]
pub struct NetworkStatus {
    pub rx_per_sec: u64,
    pub tx_per_sec: u64,
    pub total_rx: u64,
    pub total_tx: u64,
    /// One entry per up, addressed interface: "en0 192.168.1.20".
    pub interfaces: Vec<String>,
}

pub struct NetworkWatch {
    networks: Networks,
    last_refresh: Instant,
}

impl NetworkWatch {
    pub fn new() -> Self {
        Self {
            networks: Networks::new_with_refreshed_list(),
            last_refresh: Instant::now(),
        }
    }

    pub fn poll(&mut self) -> NetworkStatus {
        self.networks.refresh(true);
        let now = Instant::now();
        // The first tick is shorter than the refresh interval, and a paused or
        // descheduled thread makes it longer; measure instead of assuming 1 s.
        let elapsed = now
            .duration_since(self.last_refresh)
            .as_secs_f64()
            .max(0.001);
        self.last_refresh = now;

        // ponytail: every non-loopback interface is summed, so VPN traffic is
        // counted on both the tunnel and the physical link. Attributing it to
        // one needs routing-table lookups for a number nobody reads twice.
        let mut status = NetworkStatus::default();
        for (name, data) in self.networks.list() {
            if is_loopback(name) {
                continue;
            }
            status.rx_per_sec += (data.received() as f64 / elapsed) as u64;
            status.tx_per_sec += (data.transmitted() as f64 / elapsed) as u64;
            status.total_rx += data.total_received();
            status.total_tx += data.total_transmitted();
            if let Some(label) = describe(name, data) {
                status.interfaces.push(label);
            }
        }
        status.interfaces.sort();
        status
    }
}

fn is_loopback(name: &str) -> bool {
    name.starts_with("lo")
}

fn describe(name: &str, data: &NetworkData) -> Option<String> {
    (data.operational_state() != InterfaceOperationalState::Down)
        .then(|| address(name, data.ip_networks()))
        .flatten()
}

/// An interface is only worth naming once it carries a routable address;
/// link-local and loopback addresses mean "up but going nowhere".
fn address(name: &str, ips: &[IpNetwork]) -> Option<String> {
    let routable = |ip: &&IpNetwork| match ip.addr {
        IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_link_local(),
        IpAddr::V6(v6) => !v6.is_loopback() && !is_link_local_v6(v6),
    };
    // Prefer IPv4: it is what people recognise, and v6 privacy addresses churn.
    let ip = ips
        .iter()
        .filter(routable)
        .find(|ip| ip.addr.is_ipv4())
        .or_else(|| ips.iter().find(routable))?;
    Some(format!("{name} {}", ip.addr))
}

/// `Ipv6Addr::is_unicast_link_local` is still unstable, so test `fe80::/10` here.
fn is_link_local_v6(addr: std::net::Ipv6Addr) -> bool {
    addr.segments()[0] & 0xffc0 == 0xfe80
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(addr: &str) -> IpNetwork {
        IpNetwork {
            addr: addr.parse().unwrap(),
            prefix: 24,
        }
    }

    #[test]
    fn prefers_routable_ipv4() {
        assert_eq!(
            address(
                "en0",
                &[ip("fe80::1"), ip("2001:db8::5"), ip("192.168.1.20")]
            ),
            Some("en0 192.168.1.20".to_string())
        );
    }

    #[test]
    fn falls_back_to_ipv6_when_that_is_all_there_is() {
        assert_eq!(
            address("en1", &[ip("fe80::1"), ip("2001:db8::5")]),
            Some("en1 2001:db8::5".to_string())
        );
    }

    #[test]
    fn unaddressed_interfaces_are_skipped() {
        assert_eq!(address("en2", &[]), None);
        assert_eq!(address("en2", &[ip("169.254.3.4"), ip("fe80::1")]), None);
        assert_eq!(address("lo0", &[ip("127.0.0.1"), ip("::1")]), None);
    }

    #[test]
    fn loopback_names() {
        assert!(is_loopback("lo0"));
        assert!(!is_loopback("en0"));
        assert!(!is_loopback("utun4"));
    }
}
