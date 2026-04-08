//! Connection limiter for basic maker DoS resistance.
//!
//! The limiter enforces:
//! - Global concurrent connection cap.
//! - Per-IP concurrent connection cap (for non-loopback peers).
//! - Per-IP connection attempt rate cap in a fixed time window (for non-loopback peers).
//!
//! In production the maker listens on localhost and receives traffic via Tor port
//! forwarding, so all peers often appear as loopback. Per-IP limits are therefore
//! skipped for loopback addresses while global limits remain active.

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

/// Connection limiter shared by the maker accept loop and connection handlers.
pub struct ConnectionLimiter {
    max_connections: usize,
    max_per_ip: usize,
    rate_limit_per_ip: u32,
    rate_window: Duration,
    active_count: AtomicUsize,
    ip_active: Mutex<HashMap<IpAddr, usize>>,
    ip_rate: Mutex<HashMap<IpAddr, (u32, Instant)>>,
}

impl ConnectionLimiter {
    /// Create a new limiter instance.
    pub fn new(
        max_connections: usize,
        max_per_ip: usize,
        rate_limit_per_ip: u32,
        rate_window_secs: u64,
    ) -> Arc<Self> {
        Arc::new(Self {
            max_connections,
            max_per_ip,
            rate_limit_per_ip,
            rate_window: Duration::from_secs(rate_window_secs),
            active_count: AtomicUsize::new(0),
            ip_active: Mutex::new(HashMap::new()),
            ip_rate: Mutex::new(HashMap::new()),
        })
    }

    /// Attempt to register a new accepted connection from `ip`.
    ///
    /// Returns `true` when accepted, `false` when one of the limits is exceeded.
    pub fn try_accept(&self, ip: IpAddr) -> bool {
        let total_active = self.active_count.load(Ordering::Acquire);
        if total_active >= self.max_connections {
            return false;
        }

        if !ip.is_loopback() {
            // Enforce per-IP active connections.
            {
                let ip_active = self.ip_active.lock().expect("ip_active mutex poisoned");
                let active_for_ip = ip_active.get(&ip).copied().unwrap_or(0);
                if active_for_ip >= self.max_per_ip {
                    return false;
                }
            }

            // Enforce per-IP connection rate window.
            {
                let mut ip_rate = self.ip_rate.lock().expect("ip_rate mutex poisoned");
                let now = Instant::now();
                let entry = ip_rate.entry(ip).or_insert((0, now));

                if now.duration_since(entry.1) >= self.rate_window {
                    *entry = (0, now);
                }

                if entry.0 >= self.rate_limit_per_ip {
                    return false;
                }
                entry.0 += 1;
            }

            // Register active connection for this IP.
            {
                let mut ip_active = self.ip_active.lock().expect("ip_active mutex poisoned");
                *ip_active.entry(ip).or_insert(0) += 1;
            }
        }

        self.active_count.fetch_add(1, Ordering::Release);
        true
    }

    /// Release a previously accepted connection.
    pub fn release(&self, ip: IpAddr) {
        self.active_count.fetch_sub(1, Ordering::Release);

        if !ip.is_loopback() {
            let mut ip_active = self.ip_active.lock().expect("ip_active mutex poisoned");
            if let Some(active) = ip_active.get_mut(&ip) {
                if *active > 1 {
                    *active -= 1;
                } else {
                    ip_active.remove(&ip);
                }
            }
        }
    }
}

/// RAII guard that releases a connection from the limiter when dropped.
pub struct ConnectionGuard {
    limiter: Arc<ConnectionLimiter>,
    ip: IpAddr,
}

impl ConnectionGuard {
    /// Construct a new guard for a connection already accepted by the limiter.
    pub fn new(limiter: Arc<ConnectionLimiter>, ip: IpAddr) -> Self {
        Self { limiter, ip }
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.limiter.release(self.ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn remote_ip(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, last))
    }

    #[test]
    fn enforces_global_limit() {
        let limiter = ConnectionLimiter::new(2, 10, 100, 60);
        assert!(limiter.try_accept(remote_ip(1)));
        assert!(limiter.try_accept(remote_ip(2)));
        assert!(!limiter.try_accept(remote_ip(3)));
    }

    #[test]
    fn enforces_per_ip_limit_for_remote_peers() {
        let limiter = ConnectionLimiter::new(10, 1, 100, 60);
        let ip = remote_ip(4);
        assert!(limiter.try_accept(ip));
        assert!(!limiter.try_accept(ip));
    }

    #[test]
    fn enforces_connection_rate_for_remote_peers() {
        let limiter = ConnectionLimiter::new(10, 10, 2, 60);
        let ip = remote_ip(5);
        assert!(limiter.try_accept(ip));
        limiter.release(ip);
        assert!(limiter.try_accept(ip));
        limiter.release(ip);
        assert!(!limiter.try_accept(ip));
    }

    #[test]
    fn guard_releases_connection_on_drop() {
        let limiter = ConnectionLimiter::new(1, 10, 10, 60);
        let ip = remote_ip(6);
        assert!(limiter.try_accept(ip));
        {
            let _guard = ConnectionGuard::new(Arc::clone(&limiter), ip);
            assert!(!limiter.try_accept(remote_ip(7)));
        }
        assert!(limiter.try_accept(remote_ip(7)));
    }

    #[test]
    fn loopback_skips_per_ip_limits() {
        let limiter = ConnectionLimiter::new(3, 1, 1, 60);
        let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert!(limiter.try_accept(loopback));
        assert!(limiter.try_accept(loopback));
        assert!(limiter.try_accept(loopback));
    }
}
