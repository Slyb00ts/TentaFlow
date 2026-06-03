# =============================================================================
# Plik: server.py
# Opis: FastAPI wrapper na Playwright Chromium z izolowanymi profilami per user.
# Przykład: POST /render {"url":"https://example.com","user_id":"alice"}
# =============================================================================

import asyncio
import base64
import ipaddress
import os
import re
import shutil
import socket
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, Literal, Optional
from urllib.parse import urlparse

from fastapi import FastAPI, HTTPException
from pydantic import BaseModel, Field
from playwright.async_api import BrowserContext, Error as PlaywrightError, Route, async_playwright


DEFAULT_PORT = 8092
DEFAULT_TIMEOUT_MS = 30_000
DEFAULT_VIEWPORT_WIDTH = 1365
DEFAULT_VIEWPORT_HEIGHT = 768
MAX_CONTEXTS = int(os.environ.get("BROWSER_RENDERER_MAX_CONTEXTS", "8"))
CONTEXT_TTL_SECONDS = int(os.environ.get("BROWSER_RENDERER_CONTEXT_TTL_SECONDS", "900"))
MAX_HTML_CHARS = int(os.environ.get("BROWSER_RENDERER_MAX_HTML_CHARS", str(2 * 1024 * 1024)))
MAX_TEXT_CHARS = int(os.environ.get("BROWSER_RENDERER_MAX_TEXT_CHARS", "200000"))
PROFILE_ROOT = Path(os.environ.get(
    "BROWSER_RENDERER_PROFILE_DIR",
    Path.home() / ".cache" / "tentaflow" / "browser-renderer" / "profiles",
))


class RenderRequest(BaseModel):
    url: str
    user_id: str = Field(default="default", min_length=1, max_length=128)
    wait_until: Literal["commit", "domcontentloaded", "load", "networkidle"] = "domcontentloaded"
    timeout_ms: int = Field(default=DEFAULT_TIMEOUT_MS, ge=1_000, le=120_000)
    settle_ms: int = Field(default=300, ge=0, le=10_000)
    max_scrolls: int = Field(default=2, ge=0, le=20)
    viewport_width: int = Field(default=DEFAULT_VIEWPORT_WIDTH, ge=320, le=3840)
    viewport_height: int = Field(default=DEFAULT_VIEWPORT_HEIGHT, ge=240, le=2160)
    include_html: bool = False
    include_screenshot: bool = False
    reset_context: bool = False


class ContextInfo(BaseModel):
    user_id: str
    profile_dir: str
    last_used_at: float
    locked: bool


@dataclass
class ContextSlot:
    user_id: str
    profile_dir: Path
    context: BrowserContext
    lock: asyncio.Lock
    last_used_at: float


app = FastAPI(title="TentaFlow Browser Renderer")
_playwright = None
_contexts: Dict[str, ContextSlot] = {}
_contexts_lock = asyncio.Lock()
_host_public_cache: Dict[str, bool] = {}

BLOCKED_RESOURCE_TYPES = {"image", "media", "font", "stylesheet"}


def sanitize_user_id(user_id: str) -> str:
    clean = re.sub(r"[^a-zA-Z0-9_.@-]+", "_", user_id.strip())
    clean = clean.strip("._-")
    if not clean:
        clean = "default"
    return clean[:128]


def validate_public_url(raw_url: str) -> str:
    parsed = urlparse(raw_url)
    if parsed.scheme not in ("http", "https") or not parsed.hostname:
        raise HTTPException(status_code=400, detail="only public http/https URLs are allowed")
    if not host_is_public(parsed.hostname):
        raise HTTPException(status_code=403, detail="private, local or metadata hosts are blocked")
    return raw_url


def host_is_public(hostname: str) -> bool:
    try:
        ip = ipaddress.ip_address(hostname.strip("[]"))
        return ip_is_public(ip)
    except ValueError:
        pass
    try:
        infos = socket.getaddrinfo(hostname, None, proto=socket.IPPROTO_TCP)
    except socket.gaierror:
        return False
    addresses = {info[4][0] for info in infos}
    if not addresses:
        return False
    return all(ip_is_public(ipaddress.ip_address(addr)) for addr in addresses)


def ip_is_public(ip) -> bool:
    return not (
        ip.is_loopback
        or ip.is_private
        or ip.is_link_local
        or ip.is_multicast
        or ip.is_reserved
        or ip.is_unspecified
    )


async def ensure_chromium_installed() -> None:
    if os.environ.get("BROWSER_RENDERER_SKIP_BROWSER_INSTALL") == "1":
        return
    marker = PROFILE_ROOT.parent / "chromium-installed.marker"
    if marker.exists():
        return
    PROFILE_ROOT.parent.mkdir(parents=True, exist_ok=True)
    proc = await asyncio.create_subprocess_exec(
        sys.executable,
        "-m",
        "playwright",
        "install",
        "chromium",
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.STDOUT,
    )
    stdout, _ = await proc.communicate()
    if proc.returncode != 0:
        output = stdout.decode("utf-8", errors="replace")[-4000:]
        raise RuntimeError(f"playwright install chromium failed: {output}")
    marker.write_text(str(time.time()), encoding="utf-8")


@app.on_event("startup")
async def startup() -> None:
    global _playwright
    await ensure_chromium_installed()
    PROFILE_ROOT.mkdir(parents=True, exist_ok=True)
    _playwright = await async_playwright().start()


@app.on_event("shutdown")
async def shutdown() -> None:
    for slot in list(_contexts.values()):
        await slot.context.close()
    _contexts.clear()
    if _playwright is not None:
        await _playwright.stop()


@app.get("/health")
async def health() -> Dict[str, Any]:
    return {
        "ok": True,
        "engine": "browser-renderer",
        "active_contexts": len(_contexts),
        "max_contexts": MAX_CONTEXTS,
    }


@app.get("/contexts")
async def contexts() -> Dict[str, Any]:
    return {"contexts": [slot_info(slot).model_dump() for slot in _contexts.values()]}


@app.delete("/contexts/{user_id}")
async def delete_context(user_id: str) -> Dict[str, Any]:
    key = sanitize_user_id(user_id)
    async with _contexts_lock:
        slot = _contexts.pop(key, None)
    if slot is None:
        return {"ok": True, "deleted": False}
    async with slot.lock:
        await slot.context.close()
    shutil.rmtree(slot.profile_dir, ignore_errors=True)
    return {"ok": True, "deleted": True}


@app.post("/render")
async def render(request: RenderRequest) -> Dict[str, Any]:
    target_url = validate_public_url(request.url)
    key = sanitize_user_id(request.user_id)
    if request.reset_context:
        await delete_context(key)
    slot = await get_or_create_context(key, request)
    async with slot.lock:
        slot.last_used_at = time.time()
        page = await slot.context.new_page()
        started = time.time()
        try:
            await page.set_viewport_size({
                "width": request.viewport_width,
                "height": request.viewport_height,
            })
            response = await page.goto(
                target_url,
                wait_until=request.wait_until,
                timeout=request.timeout_ms,
            )
            await auto_scroll(page, request.max_scrolls, request.settle_ms)
            if request.settle_ms > 0:
                await page.wait_for_timeout(request.settle_ms)
            final_url = page.url
            validate_public_url(final_url)
            title = await page.title()
            text = await visible_text(page)
            html = await page.content() if request.include_html else ""
            screenshot_b64 = ""
            if request.include_screenshot:
                screenshot = await page.screenshot(full_page=False, type="png")
                screenshot_b64 = base64.b64encode(screenshot).decode("ascii")
            return {
                "ok": True,
                "user_id": key,
                "url": target_url,
                "final_url": final_url,
                "status": response.status if response else 0,
                "title": title,
                "text": truncate(text, MAX_TEXT_CHARS),
                "html": truncate(html, MAX_HTML_CHARS) if request.include_html else None,
                "screenshot_base64": screenshot_b64 if request.include_screenshot else None,
                "elapsed_ms": int((time.time() - started) * 1000),
                "context": slot_info(slot).model_dump(),
            }
        except HTTPException:
            raise
        except PlaywrightError as exc:
            raise HTTPException(status_code=502, detail=str(exc)) from exc
        finally:
            await page.close()
            slot.last_used_at = time.time()


async def get_or_create_context(user_id: str, request: RenderRequest) -> ContextSlot:
    async with _contexts_lock:
        await evict_idle_contexts()
        slot = _contexts.get(user_id)
        if slot is not None:
            return slot
        if len(_contexts) >= MAX_CONTEXTS:
            await evict_one_context()
        if len(_contexts) >= MAX_CONTEXTS:
            raise HTTPException(status_code=429, detail="no free browser context slots")
        profile_dir = PROFILE_ROOT / user_id
        profile_dir.mkdir(parents=True, exist_ok=True)
        context = await _playwright.chromium.launch_persistent_context(
            str(profile_dir),
            headless=True,
            viewport={
                "width": request.viewport_width,
                "height": request.viewport_height,
            },
            accept_downloads=False,
            ignore_https_errors=False,
            args=[
                "--disable-dev-shm-usage",
                "--disable-extensions",
                "--disable-gpu",
                "--disable-background-networking",
                "--disable-sync",
                "--no-first-run",
            ],
        )
        await context.route("**/*", route_guard)
        slot = ContextSlot(
            user_id=user_id,
            profile_dir=profile_dir,
            context=context,
            lock=asyncio.Lock(),
            last_used_at=time.time(),
        )
        _contexts[user_id] = slot
        return slot


async def evict_idle_contexts() -> None:
    now = time.time()
    victims = [
        slot for slot in _contexts.values()
        if not slot.lock.locked() and now - slot.last_used_at > CONTEXT_TTL_SECONDS
    ]
    for slot in victims:
        _contexts.pop(slot.user_id, None)
        await slot.context.close()


async def evict_one_context() -> None:
    candidates = [slot for slot in _contexts.values() if not slot.lock.locked()]
    if not candidates:
        return
    victim = min(candidates, key=lambda slot: slot.last_used_at)
    _contexts.pop(victim.user_id, None)
    await victim.context.close()


async def route_guard(route: Route) -> None:
    if route.request.resource_type in BLOCKED_RESOURCE_TYPES:
        await route.abort()
        return
    url = route.request.url
    parsed = urlparse(url)
    if parsed.scheme in ("about", "blob", "data"):
        await route.continue_()
        return
    allowed = (
        parsed.scheme in ("http", "https")
        and parsed.hostname is not None
        and await asyncio.to_thread(cached_host_is_public, parsed.hostname)
    )
    if not allowed:
        await route.abort()
        return
    await route.continue_()


def cached_host_is_public(hostname: str) -> bool:
    cached = _host_public_cache.get(hostname)
    if cached is not None:
        return cached
    allowed = host_is_public(hostname)
    _host_public_cache[hostname] = allowed
    return allowed


async def auto_scroll(page, max_scrolls: int, settle_ms: int) -> None:
    for _ in range(max_scrolls):
        before = await page.evaluate("() => document.scrollingElement ? document.scrollingElement.scrollTop : 0")
        await page.evaluate("() => window.scrollBy(0, Math.max(window.innerHeight, 600))")
        if settle_ms > 0:
            await page.wait_for_timeout(settle_ms)
        after = await page.evaluate("() => document.scrollingElement ? document.scrollingElement.scrollTop : 0")
        if after == before:
            break
    await page.evaluate("() => window.scrollTo(0, 0)")


async def visible_text(page) -> str:
    return await page.evaluate(
        """() => {
            const selectors = ['script','style','noscript','svg','canvas','iframe'];
            for (const selector of selectors) {
              for (const node of document.querySelectorAll(selector)) node.remove();
            }
            return document.body ? document.body.innerText : document.documentElement.innerText;
        }"""
    )


def slot_info(slot: ContextSlot) -> ContextInfo:
    return ContextInfo(
        user_id=slot.user_id,
        profile_dir=str(slot.profile_dir),
        last_used_at=slot.last_used_at,
        locked=slot.lock.locked(),
    )


def truncate(value: str, max_chars: int) -> str:
    if len(value) <= max_chars:
        return value
    return value[:max_chars]
