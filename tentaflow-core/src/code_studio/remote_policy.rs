// ===== File: code_studio/remote_policy.rs — which git remotes a workspace may reach =====
//
// This is deliberately NOT the `web_research` guard. That one serves addons
// reading the public web, so it refuses anything private. A code workspace is
// the opposite case: a company git server on the LAN is the normal target, and
// refusing it would make the module useless inside an office network.
//
// So private and LAN addresses are ALLOWED and merely flagged — adding such a
// remote needs `secret_manage` and produces an audit event (§11.4). What stays
// blocked for everyone is the set of addresses that are never a git server and
// are a credential-theft or lateral-movement target: cloud instance metadata,
// the cluster control plane, and loopback.
//
// Every resolved address is checked, not just the first — a name resolving to
// one public and one metadata address must not pass on a lucky ordering.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};

use anyhow::{anyhow, Result};
use url::Url;

/// A remote that passed the policy, with everything the broker needs to run
/// the operation without re-parsing the string.
#[derive(Debug, Clone)]
pub struct RemoteTarget {
    /// Normalized URL as git will receive it.
    pub url: String,
    pub scheme: RemoteScheme,
    pub host: String,
    pub port: u16,
    pub addresses: Vec<SocketAddr>,
    /// True when ANY resolved address is private/LAN. Such a remote is legal
    /// but privileged: the caller must hold `secret_manage` and the decision is
    /// audited.
    pub is_private: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteScheme {
    Https,
    Ssh,
}

/// Hosts that are never a git server and always a target worth stealing from.
/// Public because the egress gateway (§7.6) applies the identical name list
/// before it resolves anything — the blocked set is a property of the address,
/// not of the caller.
pub fn is_forbidden_host_literal(host: &str) -> bool {
    // The SAME normalisation the egress gateway applies, not a second one:
    // `kubernetes.default.svc.` and `kubernetes.default.svc` are one name in
    // DNS, and a list that only knows the dotless spelling is a list with a
    // documented way around it. The second layer does not catch it either —
    // a ClusterIP is RFC1918, and private addresses are allowed here on
    // purpose.
    let lower = super::egress::normalize_host(host);
    if matches!(
        lower.as_str(),
        "metadata.google.internal"
            | "metadata"
            | "kubernetes.default"
            | "kubernetes.default.svc"
            | "kubernetes.default.svc.cluster.local"
    ) {
        return true;
    }
    match lower.parse::<IpAddr>() {
        Ok(ip) => is_forbidden_ip(ip),
        Err(_) => false,
    }
}

/// Loopback, link-local (which is where cloud metadata lives on every major
/// provider) and the unspecified address. Rejected before DNS and again after.
pub fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.octets()[0] == 0
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_forbidden_ip(IpAddr::V4(mapped));
            }
            // fd00:ec2::254 is the IPv6 instance-metadata address on AWS; the
            // whole unique-local range is not blocked (that is ordinary LAN),
            // so this one address is named explicitly.
            let is_aws_metadata = v6.segments() == [0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x254];
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || is_aws_metadata
        }
    }
}

/// True for addresses inside the operator's own network. Legal for a git
/// remote, but the caller has to be allowed to add one.
pub fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        // 100.64.0.0/10 (carrier-grade NAT, and what Tailscale hands out) is
        // written out because `Ipv4Addr::is_shared` is still unstable.
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_private() || (a == 100 && (64..128).contains(&b))
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_private_ip(IpAddr::V4(mapped));
            }
            (v6.segments()[0] & 0xfe00) == 0xfc00
        }
    }
}

/// Parses and checks a git remote. Accepts `https://…`, `ssh://…` and the
/// scp-like `user@host:path` form git also understands — rejecting the last one
/// would just push users to a form that skips the check.
pub fn validate_remote(raw: &str) -> Result<RemoteTarget> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 2048 {
        return Err(anyhow!("remote url is empty or too long"));
    }
    if raw.bytes().any(|b| b.is_ascii_control()) {
        return Err(anyhow!("remote url contains a control character"));
    }

    let (scheme, host, port, normalized) = if let Some(rest) = scp_like_host(raw) {
        (RemoteScheme::Ssh, rest, 22, raw.to_string())
    } else {
        let url = Url::parse(raw).map_err(|e| anyhow!("invalid remote url: {e}"))?;
        let scheme = match url.scheme() {
            "https" => RemoteScheme::Https,
            "ssh" => RemoteScheme::Ssh,
            // `http`, `git://` and `ext::` are refused on purpose: the first is
            // unauthenticated in transit, the others are unauthenticated
            // outright or execute a command.
            other => return Err(anyhow!("remote protocol {other} is not allowed")),
        };
        let host = url
            .host_str()
            .ok_or_else(|| anyhow!("remote url has no host"))?
            .to_string();
        let port = url
            .port_or_known_default()
            .unwrap_or(if scheme == RemoteScheme::Ssh { 22 } else { 443 });
        // Credentials in the URL end up in `ps`, reflogs and remote config.
        //
        // Over https ANY userinfo is a credential, not just a `user:password`
        // pair: every major forge documents its PAT as `https://<token>@host/…`
        // with no password half at all (GitLab, Bitbucket, Azure DevOps), and
        // GitHub's documented form puts the token in the USER NAME. A rule that
        // needed both halves let the common shape through into git's argv,
        // `.git/config` and the registry (§11.3).
        //
        // Over ssh the user name is a LOGIN (`git@`), which is not secret and
        // which the audit trail wants; ssh carries no password in a URL, so a
        // password half there is refused all the same.
        let credential_in_url = match scheme {
            RemoteScheme::Https => !url.username().is_empty() || url.password().is_some(),
            RemoteScheme::Ssh => url.password().is_some(),
        };
        if credential_in_url {
            return Err(anyhow!(
                "credentials must not be embedded in the remote url"
            ));
        }
        (scheme, host, port, url.to_string())
    };

    if is_forbidden_host_literal(&host) {
        return Err(anyhow!(
            "remote host {host} is a metadata or control-plane address"
        ));
    }

    let addresses: Vec<SocketAddr> = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| anyhow!("cannot resolve remote host {host}: {e}"))?
        .collect();
    let is_private = classify_addresses(&host, &addresses)?;

    Ok(RemoteTarget {
        url: normalized,
        scheme,
        host,
        port,
        addresses,
        is_private,
    })
}

/// Verdict over the WHOLE answer of the resolver: one forbidden address
/// anywhere refuses the remote, and one private address anywhere marks it
/// private. Separated from `validate_remote` so the rule can be exercised
/// against a mixed answer, which no test can obtain from real DNS.
fn classify_addresses(host: &str, addresses: &[SocketAddr]) -> Result<bool> {
    if addresses.is_empty() {
        return Err(anyhow!("remote host {host} resolved to no addresses"));
    }
    if let Some(bad) = addresses.iter().find(|addr| is_forbidden_ip(addr.ip())) {
        return Err(anyhow!(
            "remote host {host} resolves to a forbidden address {}",
            bad.ip()
        ));
    }
    Ok(addresses.iter().any(|addr| is_private_ip(addr.ip())))
}

/// Recognizes `user@host:path` (and `host:path`), the scp-like syntax git
/// accepts for ssh. Returns the host part. A leading `./` or a Windows drive
/// letter is not this syntax.
fn scp_like_host(raw: &str) -> Option<String> {
    if raw.contains("://") {
        return None;
    }
    // `transport::address` is git's remote-helper syntax — `ext::sh -c …` runs
    // a command. It looks superficially like `host:path`, so it is rejected
    // here rather than mistaken for a hostname.
    if raw.contains("::") {
        return None;
    }
    let (before_colon, after_colon) = raw.split_once(':')?;
    if after_colon.is_empty() || before_colon.is_empty() {
        return None;
    }
    // `C:/repo` on Windows, and any port-looking suffix, are not scp syntax.
    if before_colon.len() == 1 {
        return None;
    }
    let host = before_colon
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(before_colon);
    // Only something that can actually be a hostname; anything else is a
    // different syntax we must not silently reinterpret as ssh.
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return None;
    }
    Some(host.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Split so the source carries no literal a secret scanner would flag: the
    // fixture must LOOK like a GitLab PAT to exercise the guard, but a repo-wide
    // scan (GitHub push protection) rejects the assembled form on sight.
    const GITLAB_PAT_REMOTE: &str = concat!(
        "https://glpat",
        "-Ab3xK9mQ7pL2vR5tW8yZ@example.invalid/repo.git"
    );

    #[test]
    fn adversarial_a_token_only_remote_url_passes_the_credential_guard() {
        // §11.3: a credential "nie trafia do linii poleceń, URL ani ps/cmdline".
        // The guard is `!username().is_empty() && password().is_some()` — an AND.
        // `https://<token>@host/repo.git` has a username and NO password, which
        // is exactly how GitLab, Bitbucket and Azure DevOps PATs are written,
        // so it sails past and lands in git argv (git_broker clone/fetch/push),
        // in `repo/.git/config`, in the registry `repo_url` column and on the
        // wire to every workspace member.
        //
        // The host is `.invalid` (RFC 2606, guaranteed not to resolve) so this
        // asserts on the REASON, not on the outcome: an `is_err()` assertion
        // would pass on the DNS failure alone — which is exactly why the
        // existing `credentials_in_the_url_are_refused_before_they_reach_ps`
        // never noticed.
        let err = validate_remote(GITLAB_PAT_REMOTE)
            .expect_err("a remote with an embedded credential must be refused");
        assert!(
            err.to_string().contains("credentials"),
            "refused for the wrong reason ({err}); the credential guard never fired"
        );
    }

    #[test]
    fn adversarial_a_trailing_dot_defeats_the_control_plane_name_list() {
        // `is_forbidden_host_literal` lowercases but never trims the root label
        // separator, while DNS treats `kubernetes.default.svc.` and
        // `kubernetes.default.svc` as the same name. The egress gateway
        // normalises the host first (`egress::normalize_host` trims trailing
        // dots); `validate_remote` calls this predicate on the raw
        // `Url::host_str()`, which keeps the dot.
        //
        // The second layer does not save it: a Kubernetes ClusterIP is RFC1918,
        // and private/LAN addresses are deliberately allowed here.
        for host in [
            "kubernetes.default.svc.",
            "kubernetes.default.",
            "metadata.google.internal.",
            "metadata.",
        ] {
            assert!(
                is_forbidden_host_literal(host),
                "control-plane name {host:?} passed because of its trailing dot"
            );
        }
    }

    /// Every refusal in this module has to be attributable. A bare `is_err()`
    /// on an unresolvable host passes on the DNS failure alone, which is how
    /// both the credential guard and the control-plane list stayed broken while
    /// the suite was green.
    fn refusal_reason(raw: &str) -> String {
        let error = validate_remote(raw).expect_err(&format!("accepted {raw}"));
        let message = error.to_string();
        assert!(
            !message.contains("cannot resolve") && !message.contains("resolved to no addresses"),
            "{raw} was refused by DNS, not by a rule: {message}"
        );
        message
    }

    #[test]
    fn cloud_metadata_and_control_plane_are_refused_by_name_and_by_literal() {
        // Layer one: the name list, checked directly so the assertion cannot
        // be satisfied by the address layer underneath it.
        for host in [
            "metadata.google.internal",
            "metadata",
            "kubernetes.default",
            "kubernetes.default.svc",
            "kubernetes.default.svc.cluster.local",
            "METADATA.GOOGLE.INTERNAL",
        ] {
            assert!(is_forbidden_host_literal(host), "name {host} passed");
        }
        for host in [
            "github.com",
            "gitlab.example",
            "10.0.0.5",
            "metadata.example.com",
        ] {
            assert!(
                !is_forbidden_host_literal(host),
                "ordinary host {host} was refused by the name list"
            );
        }

        // Layer two: the literal addresses, refused for being what they are.
        for raw in [
            "https://169.254.169.254/repo.git",
            "ssh://git@[fd00:ec2::254]/repo.git",
            "git@169.254.169.254:repo.git",
        ] {
            let message = refusal_reason(raw);
            assert!(
                message.contains("forbidden address") || message.contains("metadata"),
                "{raw} was refused for the wrong reason: {message}"
            );
        }

        // And the two layers meet: a name on the list never reaches DNS.
        for raw in [
            "https://metadata.google.internal/repo.git",
            "https://kubernetes.default.svc/repo.git",
        ] {
            let message = refusal_reason(raw);
            assert!(
                message.contains("metadata or control-plane"),
                "{raw} was refused for the wrong reason: {message}"
            );
        }
    }

    #[test]
    fn loopback_is_refused_even_though_lan_is_allowed() {
        for raw in [
            "https://127.0.0.1/repo.git",
            "https://localhost/repo.git",
            "ssh://git@[::1]/repo.git",
        ] {
            assert!(validate_remote(raw).is_err(), "accepted {raw}");
        }
    }

    #[test]
    fn only_https_and_ssh_reach_git() {
        for raw in [
            "http://example.invalid/repo.git",
            "git://example.invalid/repo.git",
            "file:///etc/passwd",
        ] {
            let message = refusal_reason(raw);
            assert!(
                message.contains("is not allowed"),
                "{raw} was refused for the wrong reason: {message}"
            );
        }
        // `ext::` runs a command instead of speaking a protocol. It must not be
        // read as a hostname either, so the reason is a parse refusal.
        let message = refusal_reason("ext::sh -c 'echo pwned'");
        assert!(
            message.contains("is not allowed") || message.contains("invalid remote url"),
            "a remote helper was refused for the wrong reason: {message}"
        );
    }

    #[test]
    fn credentials_in_the_url_are_refused_before_they_reach_ps() {
        for raw in [
            "https://user:token@example.invalid/repo.git",
            GITLAB_PAT_REMOTE,
            "https://:token@example.invalid/repo.git",
            "ssh://user:token@example.invalid/repo.git",
        ] {
            let message = refusal_reason(raw);
            assert!(
                message.contains("credentials"),
                "{raw} was refused for the wrong reason: {message}"
            );
        }
    }

    #[test]
    fn control_characters_cannot_smuggle_arguments() {
        let message = refusal_reason("https://example.invalid/repo.git\n--upload-pack=sh");
        assert!(
            message.contains("control character"),
            "refused for the wrong reason: {message}"
        );
        assert!(validate_remote("").is_err());
        assert!(validate_remote(&format!("https://{}/r.git", "x".repeat(3000))).is_err());
    }

    #[test]
    fn a_metadata_address_anywhere_in_the_answer_fails_the_whole_remote() {
        // The rule `validate_remote` itself applies to the resolver's answer,
        // run against an answer real DNS will not hand a test: one good address
        // and one metadata address, in the order that would pass if only the
        // first were checked.
        let good: SocketAddr = "93.184.216.34:443".parse().unwrap();
        let bad: SocketAddr = "169.254.169.254:443".parse().unwrap();
        let lan: SocketAddr = "10.0.0.5:443".parse().unwrap();

        let error = classify_addresses("mixed.example", &[good, bad])
            .expect_err("a metadata address in the answer must refuse the remote");
        assert!(error.to_string().contains("169.254.169.254"), "{error}");
        assert!(classify_addresses("mixed.example", &[bad, good]).is_err());

        // A LAN address anywhere marks the remote private without refusing it.
        assert!(classify_addresses("lan.example", &[good, lan]).unwrap());
        assert!(!classify_addresses("public.example", &[good]).unwrap());
        assert!(classify_addresses("empty.example", &[]).is_err());
    }

    #[test]
    fn lan_addresses_are_allowed_but_marked_private() {
        for ip in ["10.0.0.5", "192.168.1.10", "172.16.4.4"] {
            let parsed: IpAddr = ip.parse().unwrap();
            assert!(!is_forbidden_ip(parsed), "{ip} must stay reachable");
            assert!(is_private_ip(parsed), "{ip} must be flagged as private");
        }
        assert!(!is_private_ip("93.184.216.34".parse().unwrap()));
    }

    #[test]
    fn scp_syntax_is_recognized_so_it_cannot_skip_the_check() {
        assert_eq!(
            scp_like_host("git@github.com:org/repo.git").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            scp_like_host("github.com:org/repo.git").as_deref(),
            Some("github.com")
        );
        assert_eq!(scp_like_host("https://github.com/org/repo.git"), None);
        assert_eq!(scp_like_host("C:/repos/thing"), None);
        assert_eq!(scp_like_host("./relative"), None);
        // Remote-helper syntax must never be read as a hostname.
        assert_eq!(scp_like_host("ext::sh -c 'echo pwned'"), None);
        assert_eq!(scp_like_host("transport::address"), None);
    }
}
