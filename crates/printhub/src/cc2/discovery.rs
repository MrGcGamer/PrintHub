//! Directed UDP discovery. A broadcast would not leave a Docker bridge network, but a unicast
//! probe to the printer's own address does and still returns its serial number.

use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use serde::Deserialize;
use thiserror::Error;
use tokio::{
    net::{UdpSocket, lookup_host},
    time::{Instant, timeout_at},
};

pub const DISCOVERY_PORT: u16 = 52700;

const REQUEST: &[u8] = br#"{"id":0,"method":7000}"#;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DiscoveryInfo {
    #[serde(default)]
    pub host_name: String,
    #[serde(default)]
    pub machine_model: String,
    pub sn: String,
    #[serde(default)]
    pub token_status: i64,
    #[serde(default)]
    pub lan_status: i64,
}

impl DiscoveryInfo {
    /// Local MQTT control only works in LAN Only Mode.
    pub fn lan_only(&self) -> bool {
        self.lan_status == 1
    }

    /// When set, the access code replaces the default MQTT password.
    pub fn access_code_set(&self) -> bool {
        self.token_status == 1
    }
}

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("could not resolve {host}: {source}")]
    Resolve {
        host: String,
        source: std::io::Error,
    },
    #[error("{0} did not resolve to an IPv4 address")]
    NoIpv4(String),
    #[error("discovery socket: {0}")]
    Io(#[from] std::io::Error),
    #[error("no discovery reply from {target} after {attempts} attempt(s)")]
    NoReply { target: SocketAddr, attempts: u32 },
}

/// Resolves `host` to the IPv4 address the printer is reached on.
pub async fn resolve(host: &str) -> Result<IpAddr, DiscoveryError> {
    let addrs = lookup_host((host, DISCOVERY_PORT))
        .await
        .map_err(|source| DiscoveryError::Resolve {
            host: host.to_owned(),
            source,
        })?;
    addrs
        .map(|addr| addr.ip())
        .find(IpAddr::is_ipv4)
        .ok_or_else(|| DiscoveryError::NoIpv4(host.to_owned()))
}

pub async fn discover(
    host: &str,
    attempt_timeout: Duration,
    attempts: u32,
) -> Result<(IpAddr, DiscoveryInfo), DiscoveryError> {
    let ip = resolve(host).await?;
    let info = discover_at(
        SocketAddr::new(ip, DISCOVERY_PORT),
        attempt_timeout,
        attempts,
    )
    .await?;
    Ok((ip, info))
}

pub async fn discover_at(
    target: SocketAddr,
    attempt_timeout: Duration,
    attempts: u32,
) -> Result<DiscoveryInfo, DiscoveryError> {
    #[derive(Deserialize)]
    struct Reply {
        result: DiscoveryInfo,
    }

    let bind: SocketAddr = if target.is_ipv4() {
        ([0, 0, 0, 0], 0).into()
    } else {
        (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let socket = UdpSocket::bind(bind).await?;
    let mut buf = vec![0u8; 8192];

    for attempt in 1..=attempts {
        socket.send_to(REQUEST, target).await?;
        let deadline = Instant::now() + attempt_timeout;
        loop {
            let (len, from) = match timeout_at(deadline, socket.recv_from(&mut buf)).await {
                Err(_elapsed) => break,
                Ok(received) => received?,
            };
            if from.ip() != target.ip() {
                continue;
            }
            match serde_json::from_slice::<Reply>(&buf[..len]) {
                Ok(reply) => return Ok(reply.result),
                Err(err) => {
                    tracing::debug!(%from, attempt, %err, "ignoring unparseable discovery reply");
                }
            }
        }
    }
    Err(DiscoveryError::NoReply { target, attempts })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn responder(reply: &'static [u8], ignore_first: usize) -> SocketAddr {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 256];
            let mut seen = 0;
            loop {
                let (len, from) = socket.recv_from(&mut buf).await.unwrap();
                assert_eq!(&buf[..len], REQUEST);
                seen += 1;
                if seen > ignore_first {
                    socket.send_to(reply, from).await.unwrap();
                }
            }
        });
        addr
    }

    #[tokio::test]
    async fn parses_reply() {
        let target = responder(
            br#"{"id":0,"result":{"host_name":"Centauri Carbon 2","machine_model":"Centauri Carbon 2","sn":"CC2ABC","token_status":0,"lan_status":1}}"#,
            0,
        )
        .await;
        let info = discover_at(target, Duration::from_millis(500), 1)
            .await
            .unwrap();
        assert_eq!(info.sn, "CC2ABC");
        assert!(info.lan_only());
        assert!(!info.access_code_set());
    }

    #[tokio::test]
    async fn retries_after_a_lost_reply() {
        let target = responder(br#"{"id":0,"result":{"sn":"CC2XYZ"}}"#, 1).await;
        let info = discover_at(target, Duration::from_millis(200), 2)
            .await
            .unwrap();
        assert_eq!(info.sn, "CC2XYZ");
    }

    #[tokio::test]
    async fn gives_up_with_no_reply() {
        let target = responder(b"{}", usize::MAX).await;
        let err = discover_at(target, Duration::from_millis(100), 2)
            .await
            .unwrap_err();
        assert!(matches!(err, DiscoveryError::NoReply { attempts: 2, .. }));
    }
}
