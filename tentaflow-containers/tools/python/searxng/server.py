# =============================================================================
# Plik: server.py
# Opis: Wrapper startowy SearXNG generujacy runtime settings.yml dla TentaFlow.
# Przykład: PORT=8080 python server.py
# =============================================================================

import os
import secrets
from pathlib import Path


def read_port() -> int:
    raw = os.environ.get("PORT", "8080")
    try:
        port = int(raw)
    except ValueError as exc:
        raise RuntimeError(f"PORT musi byc liczba calkowita, otrzymano: {raw}") from exc
    if port < 1 or port > 65535:
        raise RuntimeError(f"PORT poza zakresem 1..65535: {port}")
    return port


def write_settings(port: int) -> Path:
    runtime_dir = Path(os.environ.get("TENTAFLOW_SEARXNG_RUNTIME_DIR", Path.home() / ".cache" / "tentaflow" / "searxng"))
    runtime_dir.mkdir(parents=True, exist_ok=True)
    settings_path = runtime_dir / f"settings-{port}.yml"
    secret = os.environ.get("SEARXNG_SECRET") or secrets.token_hex(32)
    settings_path.write_text(
        f"""use_default_settings: true

general:
  debug: false
  instance_name: "TentaFlow SearXNG"

search:
  safe_search: 0
  autocomplete: ""
  default_lang: "auto"
  formats:
    - html
    - json

server:
  bind_address: "127.0.0.1"
  port: {port}
  secret_key: "{secret}"
  limiter: false
  image_proxy: false

outgoing:
  request_timeout: 8.0
  max_request_timeout: 15.0
  pool_connections: 100
  pool_maxsize: 20
  useragent_suffix: "TentaFlow"
""",
        encoding="utf-8",
    )
    return settings_path


def main() -> None:
    port = read_port()
    settings_path = write_settings(port)
    os.environ["SEARXNG_SETTINGS_PATH"] = str(settings_path)

    from searx.webapp import app
    from werkzeug.serving import run_simple

    run_simple("127.0.0.1", port, app, threaded=True)


if __name__ == "__main__":
    main()
