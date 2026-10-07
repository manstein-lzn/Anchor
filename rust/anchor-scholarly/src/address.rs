use std::{net::IpAddr, sync::OnceLock};

use ipnet::IpNet;
use reqwest::Url;

use crate::Error;

pub(crate) fn validate_url(value: &str) -> Result<Url, Error> {
    if value.chars().any(char::is_control) || value.contains('\\') {
        return Err(Error::input("research URL contains forbidden characters"));
    }
    let url = Url::parse(value).map_err(|_| Error::input("invalid research URL"))?;
    let authority = value
        .split_once("://")
        .map(|(_, remainder)| remainder.split(['/', '?', '#']).next().unwrap_or(""))
        .unwrap_or("");
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || authority.contains('@')
        || url.port_or_known_default() != Some(443)
    {
        return Err(Error::input(
            "research URLs must be public HTTPS URLs without credentials or custom ports",
        ));
    }
    if url
        .host_str()
        .and_then(|hostname| hostname.trim_matches(['[', ']']).parse::<IpAddr>().ok())
        .is_some_and(|address| !public_ip(address))
    {
        return Err(Error::input(
            "research URLs must resolve exclusively to public Internet addresses",
        ));
    }
    Ok(url)
}

pub(crate) fn public_ip(address: IpAddr) -> bool {
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

pub(crate) fn select_address(mut addresses: Vec<IpAddr>) -> Result<IpAddr, Error> {
    if addresses.is_empty() || addresses.iter().any(|address| !public_ip(*address)) {
        return Err(Error::input(
            "research URLs must resolve exclusively to public Internet addresses",
        ));
    }
    addresses.sort_by_key(|address| (address.is_ipv6(), address.to_string()));
    Ok(addresses[0])
}

#[cfg(test)]
mod tests {
    use super::{public_ip, select_address, validate_url};

    #[test]
    fn non_public_and_tunnel_addresses_are_rejected() {
        for address in [
            "0.1.2.3",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "192.0.0.8",
            "192.0.2.1",
            "192.88.99.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:8.8.8.8",
            "64:ff9b::a00:1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "2001::1",
            "2001:db8::1",
            "2002:7f00:1::",
            "3fff::1",
        ] {
            assert!(!public_ip(address.parse().unwrap()), "{address}");
        }
        for address in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.216.34",
            "2606:4700:4700::1111",
            "2001:4860:4860::8888",
        ] {
            assert!(public_ip(address.parse().unwrap()), "{address}");
        }
    }

    #[test]
    fn mixed_and_empty_dns_answers_fail_closed() {
        assert!(select_address(Vec::new()).is_err());
        assert!(
            select_address(vec![
                "8.8.8.8".parse().unwrap(),
                "127.0.0.1".parse().unwrap()
            ])
            .is_err()
        );
        assert_eq!(
            select_address(vec![
                "2606:4700:4700::1111".parse().unwrap(),
                "8.8.8.8".parse().unwrap()
            ])
            .unwrap()
            .to_string(),
            "8.8.8.8"
        );
    }

    #[test]
    fn url_credentials_ports_schemes_and_ip_obfuscation_fail_closed() {
        for value in [
            "http://example.org",
            "file:///etc/passwd",
            "https://user:pass@example.org",
            "https://@example.org",
            "https://example.org:444",
            "https://example.org\\@127.0.0.1",
            "https://example.org\n",
            "https://127.1",
            "https://2130706433",
            "https://0x7f000001",
            "https://[::1]",
            "https://[::ffff:127.0.0.1]",
            "https://169.254.169.254/latest/meta-data",
        ] {
            assert!(validate_url(value).is_err(), "{value:?}");
        }
        assert!(validate_url("https://example.org:443/path").is_ok());
    }
}
