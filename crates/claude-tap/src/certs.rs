//! Local MITM CA for forward proxy. Author: kejiqing

use parking_lot::Mutex;
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::str::FromStr;
use time::{Duration, OffsetDateTime};

const CA_VALIDITY_DAYS: i64 = 5 * 365;
const HOST_VALIDITY_DAYS: i64 = 365;

pub struct CertificateAuthority {
    ca_dir: PathBuf,
    ca_cert_pem: String,
    ca_key: KeyPair,
    /// Issuer certificate kept for signed_by.
    ca_cert: Certificate,
    host_cache: Mutex<HashMap<String, (String, String)>>,
}

impl CertificateAuthority {
    pub fn ensure(ca_dir: Option<PathBuf>) -> anyhow::Result<Self> {
        let ca_dir = ca_dir.unwrap_or_else(|| {
            dirs_home()
                .map(|h| h.join(".claude-tap"))
                .unwrap_or_else(|| PathBuf::from(".claude-tap"))
        });
        std::fs::create_dir_all(&ca_dir)?;
        let cert_path = ca_dir.join("ca.pem");
        let key_path = ca_dir.join("ca-key.pem");

        let (ca_cert_pem, ca_key, ca_cert) = if cert_path.exists() && key_path.exists() {
            let key_pem = std::fs::read_to_string(&key_path)?;
            let cert_pem = std::fs::read_to_string(&cert_path)?;
            let key = KeyPair::from_pem(&key_pem)?;
            // Rebuild a signing Certificate from stored PEM by re-creating CA params
            // and self-signing (same key). For MITM we need issuer Certificate object.
            let mut params = CertificateParams::default();
            params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
            params.key_usages = vec![
                KeyUsagePurpose::DigitalSignature,
                KeyUsagePurpose::KeyCertSign,
                KeyUsagePurpose::CrlSign,
            ];
            let mut dn = DistinguishedName::new();
            dn.push(DnType::CommonName, "claude-tap CA");
            dn.push(DnType::OrganizationName, "claude-tap");
            params.distinguished_name = dn;
            let now = OffsetDateTime::now_utc();
            params.not_before = now - Duration::days(1);
            params.not_after = now + Duration::days(CA_VALIDITY_DAYS);
            let cert = params.self_signed(&key)?;
            // Prefer on-disk PEM for clients; signing uses regenerated cert with same key.
            (cert_pem, key, cert)
        } else {
            let key = KeyPair::generate()?;
            let mut params = CertificateParams::default();
            params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
            params.key_usages = vec![
                KeyUsagePurpose::DigitalSignature,
                KeyUsagePurpose::KeyCertSign,
                KeyUsagePurpose::CrlSign,
            ];
            let mut dn = DistinguishedName::new();
            dn.push(DnType::CommonName, "claude-tap CA");
            dn.push(DnType::OrganizationName, "claude-tap");
            params.distinguished_name = dn;
            let now = OffsetDateTime::now_utc();
            params.not_before = now;
            params.not_after = now + Duration::days(CA_VALIDITY_DAYS);
            let cert = params.self_signed(&key)?;
            let cert_pem = cert.pem();
            let key_pem = key.serialize_pem();
            std::fs::write(&cert_path, &cert_pem)?;
            std::fs::write(&key_path, &key_pem)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
            }
            (cert_pem, key, cert)
        };

        Ok(Self {
            ca_dir,
            ca_cert_pem,
            ca_key,
            ca_cert,
            host_cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn ca_cert_path(&self) -> PathBuf {
        self.ca_dir.join("ca.pem")
    }

    pub fn ca_cert_pem(&self) -> &str {
        &self.ca_cert_pem
    }

    pub fn issue_host(&self, host: &str) -> anyhow::Result<(String, String)> {
        if let Some(cached) = self.host_cache.lock().get(host).cloned() {
            return Ok(cached);
        }
        let mut params = CertificateParams::new(vec![host.to_string()])?;
        params.is_ca = IsCa::NoCa;
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, host);
        params.distinguished_name = dn;
        if let Ok(ip) = IpAddr::from_str(host) {
            params.subject_alt_names = vec![SanType::IpAddress(ip)];
        } else {
            params.subject_alt_names = vec![SanType::DnsName(host.try_into()?)];
        }
        let now = OffsetDateTime::now_utc();
        params.not_before = now;
        params.not_after = now + Duration::days(HOST_VALIDITY_DAYS);

        let host_key = KeyPair::generate()?;
        let cert = params.signed_by(&host_key, &self.ca_cert, &self.ca_key)?;
        let pair = (cert.pem(), host_key.serialize_pem());
        self.host_cache.lock().insert(host.to_string(), pair.clone());
        Ok(pair)
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn ensure_and_issue_host() {
        let dir = tempdir().unwrap();
        let ca = CertificateAuthority::ensure(Some(dir.path().to_path_buf())).unwrap();
        assert!(ca.ca_cert_path().exists());
        let (cert, key) = ca.issue_host("example.com").unwrap();
        assert!(cert.contains("BEGIN CERTIFICATE"));
        assert!(key.contains("BEGIN"));
        let (cert2, _) = ca.issue_host("example.com").unwrap();
        assert_eq!(cert, cert2);
    }
}
