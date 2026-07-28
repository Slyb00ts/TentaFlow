# =============================================================================
# File: executor/sandbox_net.py — process-wide network allowlist guard.
# Installed inside every test subprocess BEFORE untrusted code is imported:
# wraps socket.getaddrinfo plus socket.socket connect/connect_ex/sendto so any
# egress to a host outside the run allowlist raises HostNotAllowed. An empty
# allowlist (or TF_NET_BLOCK_ALL=1) blocks all network access.
# =============================================================================

from __future__ import annotations

import ipaddress
import os
import socket
from typing import Iterable, List, Optional

ALLOWLIST_ENV = "TF_HOST_ALLOWLIST"
BLOCK_ALL_ENV = "TF_NET_BLOCK_ALL"


class HostNotAllowed(Exception):
    """Test code tried to reach a host outside the run allowlist."""


def normalize_host(host: str) -> str:
    return host.strip().strip("[]").rstrip(".").lower()


def allowlist_from_env() -> List[str]:
    if os.environ.get(BLOCK_ALL_ENV) == "1":
        return []
    raw = os.environ.get(ALLOWLIST_ENV, "")
    return [part.strip() for part in raw.split(",") if part.strip()]


class _Guard:
    def __init__(self, hosts: Iterable[str], real_getaddrinfo) -> None:
        self.hosts = frozenset(normalize_host(h) for h in hosts if normalize_host(h))
        self.allowed_ips = set()
        for host in self.hosts:
            try:
                self.allowed_ips.add(str(ipaddress.ip_address(host)))
                continue
            except ValueError:
                pass
            # Resolve allowlisted names once with the real resolver so literal-IP
            # connects to allowed hosts still pass after the guard is installed.
            try:
                for info in real_getaddrinfo(host, None, proto=socket.IPPROTO_TCP):
                    self.allowed_ips.add(str(info[4][0]))
            except (socket.gaierror, OSError):
                continue

    def check(self, host) -> None:
        if isinstance(host, bytes):
            host = host.decode("ascii", errors="replace")
        name = normalize_host(str(host))
        if name in self.hosts:
            return
        try:
            if str(ipaddress.ip_address(name)) in self.allowed_ips:
                return
        except ValueError:
            pass
        raise HostNotAllowed(f"host '{name}' is not in the run allowlist")


_guard: Optional[_Guard] = None
_originals: dict = {}


def check_host(host: str) -> None:
    """Shared allowlist check for httpx event hooks and Playwright route guards."""
    if _guard is None:
        raise HostNotAllowed("network guard is not installed")
    _guard.check(host)


def _check_sockaddr(address) -> None:
    # AF_UNIX addresses (str/bytes paths) are local IPC, not network egress.
    if _guard is None or not isinstance(address, tuple) or not address:
        return
    host = address[0]
    if isinstance(host, (str, bytes)) and host not in ("", b""):
        _guard.check(host)


def _guarded_getaddrinfo(host, port, *args, **kwargs):
    # host=None/"" means a local bind (wildcard), not egress — let it through.
    if _guard is not None and host not in (None, "", b""):
        _guard.check(host)
    return _originals["getaddrinfo"](host, port, *args, **kwargs)


def _guarded_connect(self, address):
    _check_sockaddr(address)
    return _originals["connect"](self, address)


def _guarded_connect_ex(self, address):
    _check_sockaddr(address)
    return _originals["connect_ex"](self, address)


def _guarded_sendto(self, *args):
    # sendto(data[, flags], address) — the address is always the last argument.
    if len(args) >= 2:
        _check_sockaddr(args[-1])
    return _originals["sendto"](self, *args)


def install_socket_guard(allowlist: Optional[Iterable[str]] = None) -> None:
    """Install (or re-arm) the guard. When ``allowlist`` is None it is read
    from TF_HOST_ALLOWLIST / TF_NET_BLOCK_ALL. Wrapping happens once; a second
    call only swaps the active allowlist, so it is safe under gevent
    monkey-patching (locust) — the originals captured are whatever the process
    currently uses for socket I/O."""
    global _guard
    if not _originals:
        _originals["getaddrinfo"] = socket.getaddrinfo
        _originals["connect"] = socket.socket.connect
        _originals["connect_ex"] = socket.socket.connect_ex
        _originals["sendto"] = socket.socket.sendto
        socket.getaddrinfo = _guarded_getaddrinfo
        socket.socket.connect = _guarded_connect
        socket.socket.connect_ex = _guarded_connect_ex
        socket.socket.sendto = _guarded_sendto
    hosts = list(allowlist) if allowlist is not None else allowlist_from_env()
    _guard = _Guard(hosts, _originals["getaddrinfo"])
