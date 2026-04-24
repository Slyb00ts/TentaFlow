# Self-hosted iroh-relay + iroh-dns-server dla TentaFlow

Stawia własną parę serwerów iroh (relay + pkarr DNS) na twojej infrastrukturze
(VPS / homelab / Portainer), żeby TentaFlow mógł działać w środowiskach gdzie
publiczna infrastruktura n0 (`*.relay.n0.iroh-canary.iroh.link`,
`dns.iroh.link`) jest zablokowana — typowo runai/k8s workspace z ograniczonym
egress DNS.

## Pliki

| Plik | Opis |
|------|------|
| `stack.yml` | Portainer stack (docker-compose v3) z dwoma usługami. |
| `relay.toml` | Konfiguracja iroh-relay (HTTPS+STUN+QUIC, Let's Encrypt). |
| `dns.toml` | Konfiguracja iroh-dns-server (HTTPS /pkarr). |
| `Dockerfile.relay` | Build iroh-relay 0.98.0 z crates.io. |
| `Dockerfile.dns` | Build iroh-dns-server 0.98.0 z github. |

## Wymagania

1. **VPS z publicznym IPv4** (Hetzner €5/mc, OVH, DO, itd.).
2. **Domena**. W przykładach `relay.example.com`. Musi mieć rekord A
   wskazujący na IP VPSa. Opcjonalnie druga subdomena `dns.example.com`
   (patrz niżej — wariant z reverse-proxy).
3. **Firewall** na VPSie otwarty dla:
   | Port | Proto | Po co |
   |------|-------|-------|
   | 80   | TCP   | ACME HTTP-01 challenge (Let's Encrypt, tylko przy wystawianiu certu) |
   | 443  | TCP   | HTTPS relay + pkarr DNS nad HTTPS |
   | 3478 | UDP   | STUN (hole-punching) |
   | 7842 | UDP   | QUIC relay tunel |
4. **Portainer** zainstalowany i podpięty do Dockera.

## Deployment — krok po kroku

### 1. Zbuduj obrazy

Na maszynie z Dockerem (może być na VPSie):

```bash
cd deploy/iroh-relay

# Możesz zmienić tag/org w komendach poniżej.
docker build -t ghcr.io/slyb00ts/iroh-relay:0.98.0     -f Dockerfile.relay .
docker build -t ghcr.io/slyb00ts/iroh-dns-server:0.98.0 -f Dockerfile.dns   .

# Jeśli rejestr prywatny — zaloguj się:
#   echo $GHCR_TOKEN | docker login ghcr.io -u slyb00ts --password-stdin
docker push ghcr.io/slyb00ts/iroh-relay:0.98.0
docker push ghcr.io/slyb00ts/iroh-dns-server:0.98.0
```

Alternatywa: zbuduj lokalnie na hoście Portainera i pomiń push — w
`stack.yml` zamień `image:` na `build: { context: ., dockerfile: ... }`.

### 2. Przygotuj pliki konfiguracyjne

Na hoście gdzie stoi Portainer (lub na samym VPSie):

```bash
mkdir -p /srv/iroh
cd /srv/iroh
```

Skopiuj `relay.toml` i `dns.toml` z tego katalogu. W `relay.toml` podmień:

```toml
hostname = "relay.example.com"   # twoja domena
contact  = "ty@example.com"      # twój email (LE ostrzeżenia)
```

### 3. Deploy w Portainerze

1. **Stacks → Add stack**.
2. **Name:** `iroh-relay`.
3. **Build method: Web editor.** Wklej zawartość `stack.yml`.
4. W sekcji `volumes` wymień montowania na ścieżki z kroku 2:
   ```yaml
   - /srv/iroh/relay.toml:/data/relay.toml:ro
   - /srv/iroh/dns.toml:/data/dns.toml:ro
   ```
5. **Deploy the stack.**

### 4. Weryfikacja

```bash
# Na VPSie (lub innym hoście z internetem):
docker logs -f iroh-relay
# Oczekiwane:  "relay started on 0.0.0.0:443"
# Oraz:        "obtained certificate for relay.example.com"

docker logs -f iroh-dns
# Oczekiwane:  "listening on 0.0.0.0:8080"

# Z dowolnego miejsca:
curl -v https://relay.example.com/
# 200 OK + iroh server banner
```

### 5. Konfiguracja TentaFlow

Na **każdym** węźle TentaFlow (mainpc, iPhone, testbench, spark-001, …)
w `config.toml`:

```toml
[mesh]
iroh_relay_url     = "https://relay.example.com/"
iroh_pkarr_dns_url = "https://dns.example.com/pkarr"   # lub jedna domena, patrz niżej
```

> ⚠️ `iroh_pkarr_dns_url` wymaga zmiany w kodzie TentaFlow — bez niej
> sam override relay nie wystarczy (iroh nadal próbuje `dns.iroh.link`
> dla address-lookup). Ta zmiana idzie w osobnym commicie — na razie
> self-hosted tylko relay działa, discovery dalej przez n0.

Restart każdego węzła i sprawdź w GUI Mesh — po sparowaniu wszystkie
powinny świecić na zielono bez względu na to czy są w restrykcyjnej
sieci.

## Wariant z jedną domeną (reverse-proxy)

Powyższy setup zakłada **dwie subdomeny**: `relay.example.com` (iroh-relay
samodzielnie, Let's Encrypt) oraz `dns.example.com` (iroh-dns-server pod
HTTPS). Jeśli chcesz mieć to na **jednej domenie** z ścieżką `/pkarr`,
postaw **Caddy** przed oboma:

```caddyfile
relay.example.com {
    # Wszystko na /pkarr/* kieruj do iroh-dns-server
    handle_path /pkarr/* {
        reverse_proxy iroh-dns:8080
    }
    # Reszta → iroh-relay na plain HTTPS
    reverse_proxy localhost:443 {
        transport http {
            tls_insecure_skip_verify
        }
    }
}
```

Wtedy iroh-relay słucha na `localhost:443` (nie na 0.0.0.0), a Caddy ma
443 na świecie z własnym certem. Nieco bardziej skomplikowane ale
chowa oba serwery za jedną domeną.

## Dlaczego w ogóle

iroh w presecie N0 zależy od:

- `*.relay.n0.iroh-canary.iroh.link` — QUIC/STUN relay (4 regiony: use1/usw1/euc1/aps1)
- `dns.iroh.link` — pkarr discovery (klient publikuje tu adresy wiązane
  z Ed25519 node_id, inny klient query-uje po node_id i dostaje adresy +
  relay URL na którym peer się trzyma)

Jeśli **którakolwiek** z tych domen nie jest osiągalna z perspektywy węzła
(DNS-block, firewall, cenzura, air-gapped sandbox), ten węzeł nie
zarejestruje się w publicznym meshu i inne węzły go nie znajdą — nawet
jeśli inne relaye są dostępne. Stawiając WŁASNE dwa serwery i wskazując
na nie wszystkie węzły, nie zależysz od niczyjej infra. Koszt: 1 VPS ~5
EUR/mc.
