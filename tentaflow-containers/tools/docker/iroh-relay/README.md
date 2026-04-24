# Self-hosted iroh-relay + iroh-dns-server for TentaFlow

This directory contains a self-hosted iroh relay pair: the relay itself and a
pkarr DNS server. Deploy it on your own infrastructure when the public n0
infrastructure is blocked or unreliable.

## Files

| File | Description |
|------|-------------|
| `stack.yml` | Portainer stack with both services. |
| `relay.toml` | iroh-relay configuration for HTTPS, STUN, QUIC, and ACME. |
| `dns.toml` | iroh-dns-server configuration for the HTTPS `/pkarr` endpoint. |
| `Dockerfile.relay` | Builds `iroh-relay` 0.98.0 from crates.io. |
| `Dockerfile.dns` | Builds `iroh-dns-server` 0.98.0 from GitHub. |

## Requirements

1. A host with public IPv4.
2. A public hostname such as `relay.example.com` with an A record pointing to
   the host IP. You can optionally use a second hostname such as
   `dns.example.com`.
3. Firewall openings for:

   | Port | Proto | Purpose |
   |------|-------|---------|
   | 80   | TCP   | ACME HTTP-01 challenge for Let's Encrypt |
   | 443  | TCP   | HTTPS relay and pkarr over HTTPS |
   | 3478 | UDP   | STUN |
   | 7842 | UDP   | QUIC relay tunnel |

4. Docker and Portainer.

## Deployment

### 1. Build the images

```bash
cd tentaflow-containers/tools/docker/iroh-relay

# Adjust the registry and tag as needed.
docker build -t ghcr.io/slyb00ts/iroh-relay:0.98.0 -f Dockerfile.relay .
docker build -t ghcr.io/slyb00ts/iroh-dns-server:0.98.0 -f Dockerfile.dns .

# If you use a private registry, log in first:
#   echo "$GHCR_TOKEN" | docker login ghcr.io -u slyb00ts --password-stdin
docker push ghcr.io/slyb00ts/iroh-relay:0.98.0
docker push ghcr.io/slyb00ts/iroh-dns-server:0.98.0
```

If you build directly on the Portainer host, replace `image:` with a local
`build:` section in `stack.yml`.

### 2. Prepare the config files

```bash
mkdir -p /srv/iroh
cd /srv/iroh
```

Copy `relay.toml` and `dns.toml` from this directory and update the placeholders
in `relay.toml`:

```toml
hostname = "relay.example.com"
contact  = "you@example.com"
```

### 3. Deploy in Portainer

1. Open `Stacks -> Add stack`.
2. Use the name `iroh-relay`.
3. Paste `stack.yml`.
4. Update the bind mounts if your config files live elsewhere:

   ```yaml
   - /srv/iroh/relay.toml:/data/relay.toml:ro
   - /srv/iroh/dns.toml:/data/dns.toml:ro
   ```

5. Deploy the stack.

### 4. Verify the deployment

```bash
docker logs -f iroh-relay
docker logs -f iroh-dns
curl -v https://relay.example.com/
```

Expected results:
- `iroh-relay` reports that the relay started and obtained a certificate.
- `iroh-dns` reports that it listens on `0.0.0.0:8080`.
- `curl` returns HTTP 200.

### 5. Configure TentaFlow

On every TentaFlow node:

```toml
[mesh]
iroh_relay_url     = "https://relay.example.com/"
iroh_pkarr_dns_url = "https://dns.example.com/pkarr"
```

Restart each node after updating the config.

## Single-hostname variant

The setup above assumes two hostnames: `relay.example.com` for `iroh-relay`
and `dns.example.com` for `iroh-dns-server`. If you want a single hostname
with `/pkarr`, place a reverse proxy such as Caddy in front:

```caddyfile
relay.example.com {
    handle_path /pkarr/* {
        reverse_proxy iroh-dns:8080
    }
    reverse_proxy localhost:443 {
        transport http {
            tls_insecure_skip_verify
        }
    }
}
```

In that layout, Caddy owns the public certificate and exposes one HTTPS
hostname for both services.

## Why this exists

The default iroh N0 preset depends on:

- `*.relay.n0.iroh-canary.iroh.link` for relay and STUN
- `dns.iroh.link` for pkarr discovery

If either endpoint is blocked for a node, that node may fail to publish or
resolve mesh addresses. Running both services yourself removes that dependency
and makes relay and pkarr discovery fully self-hosted.
