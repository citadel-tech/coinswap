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
    global_rate: Mutex<(u32, Instant)>,
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
            global_rate: Mutex::new((0, Instant::now())),
            ip_active: Mutex::new(HashMap::new()),
            ip_rate: Mutex::new(HashMap::new()),
        })
    }

    /// Attempt to register a new accepted connection from `ip`.
    ///
    /// Returns a [`ConnectionGuard`] on success. Returning the guard from this
    /// method ensures a permit can only be minted for successfully admitted
    /// connections.
    pub(crate) fn try_accept(self: &Arc<Self>, ip: IpAddr) -> Option<ConnectionGuard> {
        let now = Instant::now();
        let total_active = self.active_count.load(Ordering::Acquire);
        if total_active >= self.max_connections {
            return None;
        }

        if !ip.is_loopback() {
            // Enforce per-IP active connections.
            {
                let ip_active = self.ip_active.lock().expect("ip_active mutex poisoned");
                let active_for_ip = ip_active.get(&ip).copied().unwrap_or(0);
                if active_for_ip >= self.max_per_ip {
                    return None;
                }
            }

            // Enforce per-IP connection rate window.
            {
                let mut ip_rate = self.ip_rate.lock().expect("ip_rate mutex poisoned");

                // Opportunistically prune stale entries to bound map growth.
                ip_rate.retain(|_, (_, window_start)| {
                    now.duration_since(*window_start) < self.rate_window
                });

                let (count, window_start) = ip_rate.get(&ip).copied().unwrap_or((0, now));
                let effective_count = if now.duration_since(window_start) >= self.rate_window {
                    0
                } else {
                    count
                };
                if effective_count >= self.rate_limit_per_ip {
                    return None;
                }
            }
        }

        // Always enforce a global accept-rate limit so loopback traffic (Tor
        // forwarding in production) is still throttled even without per-IP identity.
        {
            let mut global_rate = self.global_rate.lock().expect("global_rate mutex poisoned");
            if now.duration_since(global_rate.1) >= self.rate_window {
                *global_rate = (0, now);
            }
            if global_rate.0 >= self.rate_limit_per_ip {
                return None;
            }
            global_rate.0 += 1;
        }

        if !ip.is_loopback() {
            // Register accepted connection in per-IP rate and active maps.
            {
                let mut ip_rate = self.ip_rate.lock().expect("ip_rate mutex poisoned");
                let entry = ip_rate.entry(ip).or_insert((0, now));
                if now.duration_since(entry.1) >= self.rate_window {
                    *entry = (0, now);
                }
                entry.0 += 1;
            }
            {
                let mut ip_active = self.ip_active.lock().expect("ip_active mutex poisoned");
                *ip_active.entry(ip).or_insert(0) += 1;
            }
        }

        self.active_count.fetch_add(1, Ordering::Release);
        Some(ConnectionGuard {
            limiter: Arc::clone(self),
            ip,
        })
    }

    /// Release a previously accepted connection.
    ///
    /// This is intentionally private and must only be called from
    /// [`ConnectionGuard::drop`] to prevent forged releases.
    fn release(&self, ip: IpAddr) {
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
pub(crate) struct ConnectionGuard {
    limiter: Arc<ConnectionLimiter>,
    ip: IpAddr,
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
        let _g1 = limiter.try_accept(remote_ip(1)).expect("first accept");
        let _g2 = limiter.try_accept(remote_ip(2)).expect("second accept");
        assert!(limiter.try_accept(remote_ip(3)).is_none());
    }

    #[test]
    fn enforces_per_ip_limit_for_remote_peers() {
        let limiter = ConnectionLimiter::new(10, 1, 100, 60);
        let ip = remote_ip(4);
        let _g1 = limiter.try_accept(ip).expect("first accept");
        assert!(limiter.try_accept(ip).is_none());
    }

    #[test]
    fn enforces_connection_rate_for_remote_peers() {
        let limiter = ConnectionLimiter::new(10, 10, 2, 60);
        let ip = remote_ip(5);
        let g1 = limiter.try_accept(ip).expect("first accept");
        drop(g1);
        let g2 = limiter.try_accept(ip).expect("second accept");
        drop(g2);
        assert!(limiter.try_accept(ip).is_none());
    }

    #[test]
    fn guard_releases_connection_on_drop() {
        let limiter = ConnectionLimiter::new(1, 10, 10, 60);
        let ip = remote_ip(6);
        {
            let _guard = limiter.try_accept(ip).expect("first accept");
            assert!(limiter.try_accept(remote_ip(7)).is_none());
        }
        assert!(limiter.try_accept(remote_ip(7)).is_some());
    }

    #[test]
    fn loopback_skips_per_ip_limits() {
        let limiter = ConnectionLimiter::new(3, 1, 10, 60);
        let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let _g1 = limiter.try_accept(loopback).expect("first accept");
        let _g2 = limiter.try_accept(loopback).expect("second accept");
        let _g3 = limiter.try_accept(loopback).expect("third accept");
    }

    #[test]
    fn loopback_enforces_global_rate_limit() {
        let limiter = ConnectionLimiter::new(10, 1, 2, 60);
        let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let _g1 = limiter.try_accept(loopback).expect("first accept");
        let _g2 = limiter.try_accept(loopback).expect("second accept");
        assert!(limiter.try_accept(loopback).is_none());
    }
}
