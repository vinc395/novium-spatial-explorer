//! A small local certificate authority for LAN sharing.
//!
//! `gen-ca` creates a CA that colleagues install once as trusted. It carries name
//! constraints, so even its key could only ever vouch for the listed subnets/host
//! names (never for other websites). `gen-cert --ca-dir` issues the server
//! certificate from it. Server certificates follow the rules browsers apply to
//! locally trusted CAs, including Apple's (support.apple.com/103769): ECDSA P-256 with
//! SHA-256, names in subjectAltName, extendedKeyUsage serverAuth, validity
//! <= 825 days (397 by default), not a CA.

use anyhow::{anyhow, bail, Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, CidrSubnet, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, GeneralSubtree, IsCa, Issuer, KeyPair, KeyUsagePurpose,
    NameConstraints,
};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use time::{Duration, OffsetDateTime};

pub const CA_CERT: &str = "ca.crt";
pub const CA_KEY: &str = "ca.key";
/// Apple rejects locally trusted server certificates valid for more than 825 days.
pub const MAX_SERVER_DAYS: u32 = 825;

pub struct Fingerprints {
    pub sha256: String,
    pub sha1: String,
}

fn colon_hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// SHA-256 (browsers, macOS) and SHA-1 (the "Thumbprint" Windows shows) of a certificate.
pub fn fingerprints(der: &[u8]) -> Fingerprints {
    use ring::digest::{digest, SHA1_FOR_LEGACY_USE_ONLY, SHA256};
    Fingerprints {
        sha256: colon_hex(digest(&SHA256, der).as_ref()),
        sha1: colon_hex(digest(&SHA1_FOR_LEGACY_USE_ONLY, der).as_ref()),
    }
}

fn pem_der(pem: &str) -> Result<Vec<u8>> {
    Ok(::pem::parse(pem)
        .map_err(|e| anyhow!("PEM: {e}"))?
        .into_contents())
}

/// `192.168.10.0/23`, `127.0.0.1` (= /32), or a DNS name.
fn subtree(s: &str) -> Result<GeneralSubtree> {
    if let Some((ip, bits)) = s.split_once('/') {
        let ip: IpAddr = ip.parse().with_context(|| format!("bad subnet {s:?}"))?;
        let bits: u8 = bits
            .parse()
            .with_context(|| format!("bad prefix in {s:?}"))?;
        let max = if ip.is_ipv4() { 32 } else { 128 };
        if bits > max {
            bail!("prefix too long in {s:?}");
        }
        // Store the network address (192.168.11.21/23 -> 192.168.10.0/23): some TLS clients
        // compare the constraint's address without masking it.
        let net = match ip {
            IpAddr::V4(a) => IpAddr::from(
                (u32::from(a) & u32::MAX.checked_shl(32 - bits as u32).unwrap_or(0)).to_be_bytes(),
            ),
            IpAddr::V6(a) => IpAddr::from(
                (u128::from(a) & u128::MAX.checked_shl(128 - bits as u32).unwrap_or(0))
                    .to_be_bytes(),
            ),
        };
        return Ok(GeneralSubtree::IpAddress(CidrSubnet::from_addr_prefix(
            net, bits,
        )));
    }
    if let Ok(ip) = s.parse::<IpAddr>() {
        let bits = if ip.is_ipv4() { 32 } else { 128 };
        return Ok(GeneralSubtree::IpAddress(CidrSubnet::from_addr_prefix(
            ip, bits,
        )));
    }
    if s.is_empty()
        || !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    {
        bail!("bad host name {s:?}");
    }
    Ok(GeneralSubtree::DnsName(s.to_string()))
}

fn validity(days: u32) -> Result<(OffsetDateTime, OffsetDateTime)> {
    let now = OffsetDateTime::now_utc();
    // Start a day early so a client with a slightly slow clock accepts it.
    Ok((now - Duration::days(1), now + Duration::days(days as i64)))
}

pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    Ok(())
}

/// Create `ca.crt` + `ca.key` in `dir` (refuses to replace an existing CA unless
/// `force`: colleagues who trusted it would have to install the new one).
pub fn gen_ca(
    dir: &Path,
    common_name: &str,
    permitted: &[String],
    days: u32,
    force: bool,
) -> Result<Fingerprints> {
    if permitted.is_empty() {
        bail!("list the subnets/host names the CA may sign for (--permit), e.g. 192.168.10.0/23,myhost,localhost,127.0.0.1");
    }
    let (cert_path, key_path) = (dir.join(CA_CERT), dir.join(CA_KEY));
    if !force && (cert_path.exists() || key_path.exists()) {
        bail!(
            "{} already exists; colleagues may already trust it. Pass --force to replace it",
            cert_path.display()
        );
    }
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    let key = KeyPair::generate()?;
    let mut p = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, common_name);
    dn.push(
        DnType::OrganizationName,
        "xenium-web (local, not a public CA)",
    );
    p.distinguished_name = dn;
    p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    p.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    p.name_constraints = Some(NameConstraints {
        permitted_subtrees: permitted
            .iter()
            .map(|s| subtree(s))
            .collect::<Result<_>>()?,
        excluded_subtrees: vec![],
    });
    (p.not_before, p.not_after) = validity(days)?;
    let cert = p.self_signed(&key)?;
    write_private(&key_path, key.serialize_pem().as_bytes())?;
    std::fs::write(&cert_path, cert.pem())?;
    Ok(fingerprints(cert.der()))
}

pub struct ServerCert {
    pub cert: PathBuf,
    pub key: PathBuf,
    pub issued: bool,
    pub reason: String,
}

/// Why the existing certificate must be replaced, or None if it is still good:
/// signed by this CA, valid for 30+ more days, and covering every host.
fn needs_reissue(cert_pem: &str, ca_pem: Option<&str>, hosts: &[String]) -> Result<Option<String>> {
    use x509_parser::prelude::*;
    let der = pem_der(cert_pem)?;
    let (_, c) = X509Certificate::from_der(&der).map_err(|e| anyhow!("parse certificate: {e}"))?;
    if let Some(ca_pem) = ca_pem {
        let ca_der = pem_der(ca_pem)?;
        let (_, ca) = X509Certificate::from_der(&ca_der).map_err(|e| anyhow!("parse CA: {e}"))?;
        if c.verify_signature(Some(ca.public_key())).is_err() {
            return Ok(Some("not issued by this CA".into()));
        }
    } else if c.issuer() != c.subject() {
        return Ok(Some("not self-signed".into()));
    }
    let left = c
        .validity()
        .time_to_expiration()
        .map(|d| d.whole_days())
        .unwrap_or(0);
    if left < 30 {
        return Ok(Some(format!("expires in {left} days")));
    }
    let sans: Vec<String> = c
        .subject_alternative_name()
        .ok()
        .flatten()
        .map(|e| {
            e.value
                .general_names
                .iter()
                .filter_map(|g| match g {
                    GeneralName::DNSName(d) => Some(d.to_string()),
                    GeneralName::IPAddress(b) if b.len() == 4 => {
                        Some(IpAddr::from(<[u8; 4]>::try_from(*b).unwrap()).to_string())
                    }
                    GeneralName::IPAddress(b) if b.len() == 16 => {
                        Some(IpAddr::from(<[u8; 16]>::try_from(*b).unwrap()).to_string())
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    if let Some(h) = hosts.iter().find(|h| !sans.contains(h)) {
        return Ok(Some(format!("does not cover {h}")));
    }
    if !c
        .extended_key_usage()
        .ok()
        .flatten()
        .is_some_and(|e| e.value.server_auth)
    {
        return Ok(Some("no serverAuth usage (required by macOS/iOS)".into()));
    }
    Ok(None)
}

/// Issue `cert.pem` + `key.pem` in `out_dir` for `hosts`, signed by the CA in
/// `ca_dir` (or self-signed without one). With `if_needed`, an existing certificate
/// that is still good is kept.
pub fn gen_server_cert(
    out_dir: &Path,
    hosts: &[String],
    ca_dir: Option<&Path>,
    days: u32,
    if_needed: bool,
) -> Result<ServerCert> {
    if hosts.is_empty() {
        bail!("no host names");
    }
    if days == 0 || days > MAX_SERVER_DAYS {
        bail!("server certificate validity must be 1-{MAX_SERVER_DAYS} days (Apple's limit)");
    }
    let (cert_path, key_path) = (out_dir.join("cert.pem"), out_dir.join("key.pem"));
    let ca = match ca_dir {
        Some(d) => Some((
            std::fs::read_to_string(d.join(CA_CERT))
                .with_context(|| format!("read {}", d.join(CA_CERT).display()))?,
            std::fs::read_to_string(d.join(CA_KEY))
                .with_context(|| format!("read {}", d.join(CA_KEY).display()))?,
        )),
        None => None,
    };
    let reason = match std::fs::read_to_string(&cert_path) {
        Ok(old) if if_needed && key_path.exists() => {
            match needs_reissue(&old, ca.as_ref().map(|c| c.0.as_str()), hosts)? {
                None => {
                    return Ok(ServerCert {
                        cert: cert_path,
                        key: key_path,
                        issued: false,
                        reason: "still valid".into(),
                    })
                }
                Some(r) => r,
            }
        }
        Ok(_) => "replacing".into(),
        Err(_) => "new".into(),
    };
    std::fs::create_dir_all(out_dir)?;
    let key = KeyPair::generate()?;
    let mut p = CertificateParams::new(hosts.to_vec())?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, hosts[0].as_str());
    p.distinguished_name = dn;
    p.is_ca = IsCa::ExplicitNoCa;
    p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    (p.not_before, p.not_after) = validity(days)?;
    let cert = match &ca {
        Some((ca_pem, ca_key)) => {
            p.use_authority_key_identifier_extension = true;
            let issuer = Issuer::from_ca_cert_pem(ca_pem, KeyPair::from_pem(ca_key)?)?;
            p.signed_by(&key, &issuer)?
        }
        None => p.self_signed(&key)?,
    };
    write_private(&key_path, key.serialize_pem().as_bytes())?;
    std::fs::write(&cert_path, cert.pem())?;
    Ok(ServerCert {
        cert: cert_path,
        key: key_path,
        issued: true,
        reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use x509_parser::prelude::*;

    fn parse(pem: &str) -> Vec<u8> {
        pem_der(pem).unwrap()
    }

    #[test]
    fn subnets_are_stored_as_network_addresses() {
        let rcgen::GeneralSubtree::IpAddress(c) = subtree("192.168.11.21/23").unwrap() else {
            panic!()
        };
        assert_eq!(c, CidrSubnet::from_v4_prefix([192, 168, 10, 0], 23));
        let rcgen::GeneralSubtree::IpAddress(c) = subtree("127.0.0.1").unwrap() else {
            panic!()
        };
        assert_eq!(c, CidrSubnet::from_v4_prefix([127, 0, 0, 1], 32));
        assert!(matches!(
            subtree("myhost").unwrap(),
            rcgen::GeneralSubtree::DnsName(_)
        ));
        assert!(subtree("bad host").is_err() && subtree("10.0.0.0/40").is_err());
    }

    #[test]
    fn ca_signed_server_cert_meets_browser_and_apple_rules() {
        let dir = tempfile::tempdir().unwrap();
        let ca_dir = dir.path().join("ca");
        let permit: Vec<String> = ["192.168.10.0/23", "myhost", "localhost", "127.0.0.1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let fp = gen_ca(&ca_dir, "test CA", &permit, 3650, false).unwrap();
        assert_eq!(fp.sha256.len(), 32 * 3 - 1);
        assert!(
            gen_ca(&ca_dir, "again", &permit, 3650, false).is_err(),
            "never silently replaces a CA"
        );
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(ca_dir.join(CA_KEY))
                .unwrap()
                .permissions(),
        );
        assert_eq!(mode & 0o777, 0o600);

        let hosts: Vec<String> = ["192.168.11.21", "myhost", "localhost", "127.0.0.1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let tls = dir.path().join("tls");
        let s = gen_server_cert(&tls, &hosts, Some(&ca_dir), 397, true).unwrap();
        assert!(s.issued);
        let ca_der = parse(&std::fs::read_to_string(ca_dir.join(CA_CERT)).unwrap());
        let (_, ca) = X509Certificate::from_der(&ca_der).unwrap();
        let leaf_der = parse(&std::fs::read_to_string(&s.cert).unwrap());
        let (_, leaf) = X509Certificate::from_der(&leaf_der).unwrap();
        // Chain, CA flags and name constraints.
        leaf.verify_signature(Some(ca.public_key())).unwrap();
        assert!(ca.basic_constraints().unwrap().unwrap().value.ca);
        assert!(ca.name_constraints().unwrap().is_some());
        assert!(!leaf.basic_constraints().unwrap().unwrap().value.ca);
        // Apple: serverAuth EKU, <= 825 days, SANs, ECDSA/SHA-256.
        assert!(
            leaf.extended_key_usage()
                .unwrap()
                .unwrap()
                .value
                .server_auth
        );
        let days = (leaf.validity().not_after.timestamp() - leaf.validity().not_before.timestamp())
            / 86400;
        assert!(days <= 825, "{days} days");
        assert_eq!(
            leaf.signature_algorithm.algorithm,
            x509_parser::oid_registry::OID_SIG_ECDSA_WITH_SHA256
        );
        assert_eq!(
            needs_reissue(
                &std::fs::read_to_string(&s.cert).unwrap(),
                Some(&std::fs::read_to_string(ca_dir.join(CA_CERT)).unwrap()),
                &hosts
            )
            .unwrap(),
            None
        );
        // Kept when still good; reissued for a new host or a different CA.
        assert!(
            !gen_server_cert(&tls, &hosts, Some(&ca_dir), 397, true)
                .unwrap()
                .issued
        );
        let more = [hosts.clone(), vec!["192.168.11.22".into()]].concat();
        assert!(gen_server_cert(&tls, &more, Some(&ca_dir), 397, true)
            .unwrap()
            .reason
            .contains("192.168.11.22"));
        let other = dir.path().join("ca2");
        gen_ca(&other, "other", &permit, 3650, false).unwrap();
        assert_eq!(
            gen_server_cert(&tls, &more, Some(&other), 397, true)
                .unwrap()
                .reason,
            "not issued by this CA"
        );
        assert!(
            gen_server_cert(&tls, &hosts, Some(&other), 900, true).is_err(),
            "over Apple's 825-day limit"
        );
    }
}
