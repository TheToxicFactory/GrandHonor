use url::Url;

pub fn is_within_domain(host: &str, domain: &str) -> bool {
    let host = host.trim_end_matches('.');
    host.eq_ignore_ascii_case(domain)
        || (host.len() > domain.len()
            && host[..host.len() - domain.len()].ends_with('.')
            && host[host.len() - domain.len()..].eq_ignore_ascii_case(domain))
}

pub fn is_trusted_package_host(host: &str) -> bool {
    is_within_domain(host, "honor.com")
        || is_within_domain(host, "hihonorcloud.com")
        || is_within_domain(host, "hihonor.com")
}

pub fn parse_package_url(raw: &str) -> Option<Url> {
    let mut url = Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let host = url.host_str()?.to_string();
    if !is_trusted_package_host(&host) {
        return None;
    }
    url.set_scheme("https").ok()?;

    let path = url.path().to_string();
    let last_segment = path.rsplit('/').next().unwrap_or("");
    let has_extension = last_segment.contains('.') && !last_segment.ends_with('.');
    if !has_extension && is_within_domain(&host, "hihonorcloud.com") {
        let trimmed = path.trim_end_matches('/');
        url.set_path(&format!("{trimmed}/full/update.zip"));
    }
    Some(url)
}

/// GrandHonor's own updates come only from its GitHub releases: the API, the release page and
/// the two CDN hosts GitHub redirects asset downloads to.
pub fn is_trusted_update_host(host: &str) -> bool {
    ["api.github.com", "github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"]
        .iter()
        .any(|h| host.trim_end_matches('.').eq_ignore_ascii_case(h))
}

/// Same rules as `validate_redirect_url`, for GrandHonor update downloads.
pub fn validate_update_redirect(base: &Url, location: &str) -> Option<Url> {
    let next = base.join(location).ok()?;
    if next.scheme() != "https"
        || next.port().is_some()
        || !next.username().is_empty()
        || next.password().is_some()
        || !is_trusted_update_host(next.host_str()?)
    {
        return None;
    }
    Some(next)
}

pub fn validate_redirect_url(base: &Url, location: &str) -> Option<Url> {
    let next = base.join(location).ok()?;
    if next.scheme() != "https"
        || next.port().is_some()
        || !next.username().is_empty()
        || next.password().is_some()
    {
        return None;
    }
    let host = next.host_str()?;
    if !is_trusted_package_host(host) {
        return None;
    }
    Some(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforces_domain_boundaries_and_https_redirects() {
        assert!(is_trusted_package_host("update.platform.hihonorcloud.com"));
        assert!(is_trusted_package_host("selfservice-eu.honor.com"));
        assert!(!is_trusted_package_host("evilhonor.com"));
        assert!(!is_trusted_package_host("honor.com.attacker.test"));

        let parsed = parse_package_url("http://update.platform.hihonorcloud.com/pkg/123").unwrap();
        assert_eq!(
            parsed.as_str(),
            "https://update.platform.hihonorcloud.com/pkg/123/full/update.zip"
        );
        assert!(parse_package_url("https://user:pass@honor.com/a.exe").is_none());
        assert!(parse_package_url("https://honor.com:8443/a.exe").is_none());

        assert!(
            validate_redirect_url(&parsed, "https://cdn.hihonor.com/a.zip").is_some()
        );
        assert!(
            validate_redirect_url(&parsed, "http://cdn.hihonor.com/a.zip").is_none()
        );
        assert!(
            validate_redirect_url(&parsed, "https://attacker.com/a.zip").is_none()
        );
    }

    #[test]
    fn update_downloads_stay_on_github() {
        let base = Url::parse("https://github.com/o/r/releases/download/v1/s.exe").unwrap();
        assert!(validate_update_redirect(&base, "https://release-assets.githubusercontent.com/x").is_some());
        assert!(validate_update_redirect(&base, "https://objects.githubusercontent.com/x").is_some());
        assert!(validate_update_redirect(&base, "http://objects.githubusercontent.com/x").is_none());
        assert!(validate_update_redirect(&base, "https://evilgithub.com/x").is_none());
        assert!(validate_update_redirect(&base, "https://github.com.attacker.test/x").is_none());
        assert!(!is_trusted_update_host("gist.githubusercontent.com"));
    }
}
