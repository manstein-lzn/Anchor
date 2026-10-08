use std::{
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    sync::OnceLock,
    time::Duration,
};

use ipnet::IpNet;
use url::Url;

const MAX_RESOLVED_ADDRESSES: usize = 32;

pub(crate) fn validate_oauth_url(url: &Url) -> Result<(), ()> {
    let host = url.host_str().ok_or(())?;
    let loopback = is_loopback_host(host);
    if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.port_or_known_default().is_none_or(|port| port == 0)
    {
        return Err(());
    }
    if parse_ip(host).is_some_and(|address| !address.is_loopback() && !is_public_ip(address)) {
        return Err(());
    }
    Ok(())
}

pub(crate) async fn async_client(url: &Url) -> Result<reqwest::Client, ()> {
    validate_oauth_url(url)?;
    let host = url.host_str().ok_or(())?;
    let port = url.port_or_known_default().ok_or(())?;
    let addresses = if let Some(address) = parse_ip(host) {
        vec![SocketAddr::new(address, port)]
    } else {
        tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| ())?
            .collect::<Vec<_>>()
    };
    let addresses = validate_resolved_addresses(host, addresses)?;
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15));
    if parse_ip(host).is_none() {
        builder = builder.resolve_to_addrs(host, &addresses);
    }
    builder.build().map_err(|_| ())
}

pub(crate) fn blocking_client(url: &Url) -> Result<reqwest::blocking::Client, ()> {
    validate_oauth_url(url)?;
    let host = url.host_str().ok_or(())?;
    let port = url.port_or_known_default().ok_or(())?;
    let addresses = if let Some(address) = parse_ip(host) {
        vec![SocketAddr::new(address, port)]
    } else {
        (host, port).to_socket_addrs().map_err(|_| ())?.collect()
    };
    let addresses = validate_resolved_addresses(host, addresses)?;
    let mut builder = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15));
    if parse_ip(host).is_none() {
        builder = builder.resolve_to_addrs(host, &addresses);
    }
    builder.build().map_err(|_| ())
}

fn validate_resolved_addresses(
    host: &str,
    addresses: Vec<SocketAddr>,
) -> Result<Vec<SocketAddr>, ()> {
    let local_host = is_loopback_host(host);
    if addresses.is_empty() || addresses.len() > MAX_RESOLVED_ADDRESSES {
        return Err(());
    }
    let mut has_loopback = false;
    let mut approved = Vec::with_capacity(addresses.len());
    for address in addresses {
        if address.ip().is_loopback() {
            if !local_host {
                return Err(());
            }
            has_loopback = true;
        } else if !is_public_ip(address.ip()) {
            return Err(());
        }
        approved.push(address);
    }
    if has_loopback && approved.iter().any(|address| !address.ip().is_loopback()) {
        return Err(());
    }
    approved.sort_unstable();
    approved.dedup();
    Ok(approved)
}

fn parse_ip(host: &str) -> Option<IpAddr> {
    host.trim_matches(['[', ']']).parse().ok()
}

fn is_loopback_host(host: &str) -> bool {
    host.trim_end_matches('.').eq_ignore_ascii_case("localhost")
        || parse_ip(host).is_some_and(|address| address.is_loopback())
}

fn is_public_ip(address: IpAddr) -> bool {
    static DENIED: OnceLock<Vec<IpNet>> = OnceLock::new();
    let denied = DENIED.get_or_init(|| {
        [
            "0.0.0.0/8",
            "10.0.0.0/8",
            "100.64.0.0/10",
            "127.0.0.0/8",
            "169.254.0.0/16",
            "172.16.0.0/12",
            "192.0.0.0/24",
            "192.0.2.0/24",
            "192.88.99.0/24",
            "192.168.0.0/16",
            "198.18.0.0/15",
            "198.51.100.0/24",
            "203.0.113.0/24",
            "224.0.0.0/4",
            "240.0.0.0/4",
            "2001::/23",
            "2001:db8::/32",
            "2002::/16",
            "3fff::/20",
        ]
        .into_iter()
        .map(|network| network.parse().expect("constant address block"))
        .collect()
    });
    match address {
        IpAddr::V4(_) => !denied.iter().any(|network| network.contains(&address)),
        IpAddr::V6(address_v6) => {
            let first = address_v6.segments()[0];
            (0x2000..=0x3fff).contains(&first)
                && !denied.iter().any(|network| network.contains(&address))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_public_ip, validate_oauth_url, validate_resolved_addresses};
    use std::net::SocketAddr;
    use url::Url;

    #[test]
    fn oauth_urls_reject_unsafe_schemes_and_credentials() {
        for value in ["http://example.com/token", "https://user@example.com/token"] {
            let url = Url::parse(value).unwrap();
            assert!(validate_oauth_url(&url).is_err());
        }
        assert!(validate_oauth_url(&Url::parse("https://10.0.0.1/token").unwrap()).is_err());
        assert!(!is_public_ip("169.254.169.254".parse().unwrap()));
    }

    #[test]
    fn resolved_addresses_allow_public_or_explicit_loopback_only() {
        assert!(
            validate_resolved_addresses(
                "provider.example",
                vec!["1.1.1.1:443".parse::<SocketAddr>().unwrap()]
            )
            .is_ok()
        );
        assert!(
            validate_resolved_addresses(
                "provider.example",
                vec!["10.0.0.1:443".parse::<SocketAddr>().unwrap()]
            )
            .is_err()
        );
        assert!(
            validate_resolved_addresses(
                "provider.example",
                vec!["169.254.169.254:443".parse::<SocketAddr>().unwrap()]
            )
            .is_err()
        );
        assert!(
            validate_resolved_addresses(
                "localhost",
                vec!["127.0.0.1:443".parse::<SocketAddr>().unwrap()]
            )
            .is_ok()
        );
        assert!(
            validate_resolved_addresses(
                "provider.example",
                vec!["127.0.0.1:443".parse::<SocketAddr>().unwrap()]
            )
            .is_err()
        );
        assert!(
            validate_resolved_addresses(
                "provider.example",
                vec![
                    "1.1.1.1:443".parse::<SocketAddr>().unwrap(),
                    "127.0.0.1:443".parse::<SocketAddr>().unwrap(),
                ]
            )
            .is_err()
        );
    }
}
