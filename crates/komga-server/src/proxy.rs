//! Trusted reverse-proxy boundary and canonical client-address resolution.

use std::net::{IpAddr, SocketAddr};

use axum::{extract::ConnectInfo, http::HeaderMap};
use ipnet::IpNet;

pub(crate) const FORWARDED_HEADERS: [&str; 7] = [
    "forwarded",
    "x-forwarded-host",
    "x-forwarded-port",
    "x-forwarded-proto",
    "x-forwarded-prefix",
    "x-forwarded-ssl",
    "x-forwarded-for",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClientAddress(pub(crate) IpAddr);

pub(crate) fn prepare_request(
    headers: &mut HeaderMap,
    peer: Option<ConnectInfo<SocketAddr>>,
    trusted: &[IpNet],
) -> Option<ClientAddress> {
    let peer_ip = peer.map(|peer| peer.0.ip());
    let peer_is_trusted = peer_ip.is_some_and(|ip| contains(trusted, ip));
    let client = if peer_is_trusted {
        peer_ip.map(|peer| resolve_forwarded_client(headers, peer, trusted))
    } else {
        for name in FORWARDED_HEADERS {
            headers.remove(name);
        }
        peer_ip
    };
    client.map(ClientAddress)
}

fn resolve_forwarded_client(headers: &HeaderMap, peer: IpAddr, trusted: &[IpNet]) -> IpAddr {
    let chain = forwarded_chain(headers).or_else(|| x_forwarded_for_chain(headers));
    let mut current = peer;
    for hop in chain.into_iter().flatten().rev() {
        if !contains(trusted, current) {
            break;
        }
        current = hop;
    }
    current
}

fn contains(networks: &[IpNet], address: IpAddr) -> bool {
    networks.iter().any(|network| network.contains(&address))
}

fn forwarded_chain(headers: &HeaderMap) -> Option<Vec<IpAddr>> {
    let addresses = headers
        .get_all("forwarded")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|element| {
            element.split(';').find_map(|parameter| {
                let (name, value) = parameter.trim().split_once('=')?;
                name.eq_ignore_ascii_case("for")
                    .then(|| parse_forwarded_address(value.trim().trim_matches('"')))
                    .flatten()
            })
        })
        .collect::<Vec<_>>();
    (!addresses.is_empty()).then_some(addresses)
}

fn x_forwarded_for_chain(headers: &HeaderMap) -> Option<Vec<IpAddr>> {
    let addresses = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|value| parse_forwarded_address(value.trim()))
        .collect::<Vec<_>>();
    (!addresses.is_empty()).then_some(addresses)
}

fn parse_forwarded_address(value: &str) -> Option<IpAddr> {
    if value.is_empty() || value.eq_ignore_ascii_case("unknown") || value.starts_with('_') {
        return None;
    }
    value
        .parse::<IpAddr>()
        .ok()
        .or_else(|| value.parse::<SocketAddr>().ok().map(|address| address.ip()))
        .or_else(|| {
            value
                .strip_prefix('[')
                .and_then(|value| value.split_once(']'))
                .and_then(|(ip, _)| ip.parse().ok())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn networks(values: &[&str]) -> Vec<IpNet> {
        values.iter().map(|value| value.parse().unwrap()).collect()
    }

    #[test]
    fn untrusted_peers_cannot_supply_forwarding_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("forwarded", "for=192.0.2.1;proto=https".parse().unwrap());
        headers.insert("x-forwarded-prefix", "/komga".parse().unwrap());

        let client = prepare_request(
            &mut headers,
            Some(ConnectInfo("203.0.113.9:1234".parse().unwrap())),
            &networks(&["127.0.0.0/8"]),
        );

        assert_eq!(client, Some(ClientAddress("203.0.113.9".parse().unwrap())));
        assert!(!headers.contains_key("forwarded"));
        assert!(!headers.contains_key("x-forwarded-prefix"));
    }

    #[test]
    fn trusted_chains_stop_at_the_first_untrusted_hop_from_the_right() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "198.51.100.99, 192.0.2.20, 172.16.0.5".parse().unwrap(),
        );

        let client = prepare_request(
            &mut headers,
            Some(ConnectInfo("127.0.0.1:1234".parse().unwrap())),
            &networks(&["127.0.0.0/8", "172.16.0.0/12"]),
        );

        assert_eq!(client, Some(ClientAddress("192.0.2.20".parse().unwrap())));
        assert!(headers.contains_key("x-forwarded-for"));
    }

    #[test]
    fn missing_peer_metadata_is_not_a_trust_bypass() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "192.0.2.1".parse().unwrap());

        assert_eq!(prepare_request(&mut headers, None, &networks(&["0.0.0.0/0"])), None);
        assert!(!headers.contains_key("x-forwarded-for"));
    }
}
