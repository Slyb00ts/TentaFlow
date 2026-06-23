// =============================================================================
// File: webrtc/channel.rs
// Purpose: Generic WebRTC channel — the vendor-agnostic "dumb pipe" Core will
//          expose to addons. Owns the peer connection, one data channel and an
//          optional inbound video track. Knows NOTHING about any robot: the
//          caller drives signaling (offer out / answer in) and the data channel
//          (send / drain). Inbound messages are buffered for pull-style draining
//          (the model a WASM addon uses — it cannot hold async callbacks).
// =============================================================================

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use bytes::Bytes;
use tokio::sync::{mpsc, Mutex};

use webrtc::rtp::codecs::h264::H264Packet;
use webrtc::rtp::packetizer::Depacketizer;
use webrtc::stats::StatsReportType;

use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::setting_engine::SettingEngine;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice::mdns::MulticastDnsMode;
use webrtc::ice::network_type::NetworkType;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;
use webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection;
use webrtc::rtp_transceiver::RTCRtpTransceiverInit;
use webrtc::track::track_remote::TrackRemote;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelState {
    New,
    Connecting,
    Connected,
    Disconnected,
    Failed,
    Closed,
}

impl ChannelState {
    fn as_u8(self) -> u8 {
        match self {
            ChannelState::New => 0,
            ChannelState::Connecting => 1,
            ChannelState::Connected => 2,
            ChannelState::Disconnected => 3,
            ChannelState::Failed => 4,
            ChannelState::Closed => 5,
        }
    }
    fn from_u8(v: u8) -> ChannelState {
        match v {
            1 => ChannelState::Connecting,
            2 => ChannelState::Connected,
            3 => ChannelState::Disconnected,
            4 => ChannelState::Failed,
            5 => ChannelState::Closed,
            _ => ChannelState::New,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            ChannelState::New => "new",
            ChannelState::Connecting => "connecting",
            ChannelState::Connected => "connected",
            ChannelState::Disconnected => "disconnected",
            ChannelState::Failed => "failed",
            ChannelState::Closed => "closed",
        }
    }
}

/// One inbound data-channel message.
#[derive(Debug, Clone)]
pub enum DcMessage {
    Text(String),
    Binary(Vec<u8>),
}

/// Application-level keepalive used to measure true round-trip latency to the
/// peer (the transport ICE RTT is unreliable on LAN). The caller supplies the
/// vendor-specific ping text and a substring that identifies the peer's reply,
/// so the generic channel measures precise RTT without knowing the protocol.
#[derive(Clone)]
pub struct KeepaliveConfig {
    /// Text message sent to the peer every `interval`.
    pub text: String,
    /// How often to ping.
    pub interval: Duration,
    /// Substring that marks an inbound text message as the keepalive reply.
    pub response_marker: String,
}

/// How to build the channel. All transport policy is explicit here so no
/// vendor/LAN assumption leaks into the generic pipe.
pub struct WebRtcConfig {
    /// Data channel label the peer expects.
    pub data_channel_label: String,
    /// Add a recvonly video transceiver so the peer offers/sends video.
    pub want_video: bool,
    /// Disable ICE mDNS so the raw host IP is advertised (LAN peers that reject
    /// `.local` candidates need this). Caller decides — not a channel default.
    pub disable_mdns: bool,
    /// Max time to wait for ICE gathering to complete (non-trickle). On timeout
    /// the partial local description (host candidates) is used rather than
    /// stranding the call forever.
    pub gather_timeout: Duration,
    /// Inbound queue capacity; oldest is dropped past this (and counted).
    pub inbound_capacity: usize,
    /// Optional app-level keepalive for precise RTT (replaces the unreliable
    /// transport-stats RTT when set).
    pub keepalive: Option<KeepaliveConfig>,
    /// IPv4 addresses ICE is allowed to gather host candidates from. When
    /// non-empty the channel restricts gathering to UDP/IPv4 on exactly these
    /// local IPs — a multi-homed host would otherwise advertise candidates from
    /// docker/link-local/unrelated interfaces and fail ICE against a LAN peer.
    /// Empty = no restriction (default, so non-robot callers are unaffected).
    pub ice_ipv4_allowlist: Vec<std::net::Ipv4Addr>,
}

impl Default for WebRtcConfig {
    fn default() -> Self {
        WebRtcConfig {
            data_channel_label: "data".to_string(),
            want_video: false,
            disable_mdns: false,
            gather_timeout: Duration::from_secs(8),
            inbound_capacity: 2048,
            keepalive: None,
            ice_ipv4_allowlist: Vec::new(),
        }
    }
}

/// A generic WebRTC channel. Explicitly `close()` it; `Drop` is best-effort.
pub struct WebRtcChannel {
    pc: Arc<RTCPeerConnection>,
    dc: Arc<RTCDataChannel>,
    inbound: Arc<Mutex<VecDeque<DcMessage>>>,
    inbound_capacity: usize,
    dropped: Arc<AtomicU64>,
    state: Arc<AtomicU8>,
    dc_open: Arc<AtomicBool>,
    /// Latest transport round-trip time in milliseconds (nominated ICE pair),
    /// refreshed continuously in the background. `u64::MAX` = not yet known.
    rtt_ms: Arc<AtomicU64>,
    /// Sink for depacketized H.264 Annex-B access units. Set once via
    /// `take_h264_rx`; the on_track reader discards until it is set.
    h264_sink: Arc<StdMutex<Option<mpsc::Sender<Bytes>>>>,
    video_taken: Arc<AtomicBool>,
    has_video: bool,
}

impl WebRtcChannel {
    /// Build the peer, gather ICE (non-trickle, bounded), and return the local
    /// offer SDP. The caller ferries it to the peer via its own signaling and
    /// feeds the answer back through `set_answer`.
    pub async fn create(cfg: WebRtcConfig) -> Result<(WebRtcChannel, String)> {
        let mut m = MediaEngine::default();
        m.register_default_codecs().context("register default codecs")?;
        let mut registry = Registry::new();
        registry = register_default_interceptors(registry, &mut m)?;

        let mut se = SettingEngine::default();
        if cfg.disable_mdns {
            se.set_ice_multicast_dns_mode(MulticastDnsMode::Disabled);
        }
        // Constrain ICE gathering to the caller-chosen local IPv4s. The host
        // computes these from the same interface-selection logic the mesh uses,
        // so the offer only carries reachable host candidates. IPv4-UDP only
        // eliminates the IPv6 link-local bind failures (`could not listen udp
        // fe80::... os error 22`) on multi-homed hosts. Empty allowlist keeps
        // default gathering for non-robot callers.
        if !cfg.ice_ipv4_allowlist.is_empty() {
            se.set_network_types(vec![NetworkType::Udp4]);
            let allow = cfg.ice_ipv4_allowlist.clone();
            se.set_ip_filter(Box::new(move |ip| match ip {
                std::net::IpAddr::V4(v4) => allow.contains(&v4),
                std::net::IpAddr::V6(_) => false,
            }));
        }

        let api = APIBuilder::new()
            .with_media_engine(m)
            .with_interceptor_registry(registry)
            .with_setting_engine(se)
            .build();

        let pc = Arc::new(api.new_peer_connection(RTCConfiguration::default()).await?);

        let state = Arc::new(AtomicU8::new(ChannelState::New.as_u8()));
        let state_cb = state.clone();
        pc.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
            let mapped = match s {
                RTCPeerConnectionState::Connecting => ChannelState::Connecting,
                RTCPeerConnectionState::Connected => ChannelState::Connected,
                RTCPeerConnectionState::Disconnected => ChannelState::Disconnected,
                RTCPeerConnectionState::Failed => ChannelState::Failed,
                RTCPeerConnectionState::Closed => ChannelState::Closed,
                _ => ChannelState::New,
            };
            state_cb.store(mapped.as_u8(), Ordering::SeqCst);
            Box::pin(async {})
        }));

        // Latency: app-level keepalive RTT when configured (reliable), else the
        // transport ICE-pair RTT (best-effort; often 0 on LAN). Both write rtt_ms.
        let rtt_ms = Arc::new(AtomicU64::new(u64::MAX));
        let last_keepalive: Arc<StdMutex<Option<Instant>>> = Arc::new(StdMutex::new(None));
        if cfg.keepalive.is_none() {
            let pc_rtt = pc.clone();
            let rtt_store = rtt_ms.clone();
            let state_rtt = state.clone();
            tokio::spawn(async move {
                loop {
                    if ChannelState::from_u8(state_rtt.load(Ordering::SeqCst)) == ChannelState::Closed
                    {
                        break;
                    }
                    let report = pc_rtt.get_stats().await;
                    let rtt = report.reports.values().find_map(|t| match t {
                        StatsReportType::CandidatePair(p) if p.nominated => {
                            Some(p.current_round_trip_time)
                        }
                        _ => None,
                    });
                    if let Some(secs) = rtt {
                        if secs.is_finite() && secs >= 0.0 {
                            rtt_store.store((secs * 1000.0).round() as u64, Ordering::SeqCst);
                        }
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            });
        }

        let h264_sink: Arc<StdMutex<Option<mpsc::Sender<Bytes>>>> = Arc::new(StdMutex::new(None));
        let video_taken = Arc::new(AtomicBool::new(false));
        if cfg.want_video {
            pc.add_transceiver_from_kind(
                RTPCodecType::Video,
                Some(RTCRtpTransceiverInit {
                    direction: RTCRtpTransceiverDirection::Recvonly,
                    send_encodings: vec![],
                }),
            )
            .await?;

            let sink = h264_sink.clone();
            let taken = video_taken.clone();
            pc.on_track(Box::new(move |track, _receiver, _transceiver| {
                let sink = sink.clone();
                let taken = taken.clone();
                Box::pin(async move {
                    if track.kind() == RTPCodecType::Video {
                        tokio::spawn(h264_reader(track, sink, taken));
                    }
                })
            }));
        }

        let dc = pc.create_data_channel(&cfg.data_channel_label, None).await?;

        let dc_open = Arc::new(AtomicBool::new(false));
        let open_flag = dc_open.clone();
        dc.on_open(Box::new(move || {
            open_flag.store(true, Ordering::SeqCst);
            Box::pin(async {})
        }));
        let close_flag = dc_open.clone();
        dc.on_close(Box::new(move || {
            close_flag.store(false, Ordering::SeqCst);
            Box::pin(async {})
        }));

        let inbound = Arc::new(Mutex::new(VecDeque::<DcMessage>::new()));
        let dropped = Arc::new(AtomicU64::new(0));
        let inbound_cb = inbound.clone();
        let dropped_cb = dropped.clone();
        let cap = cfg.inbound_capacity.max(1);
        let ka_marker = cfg.keepalive.as_ref().map(|k| k.response_marker.clone());
        let last_ka_cb = last_keepalive.clone();
        let rtt_cb = rtt_ms.clone();
        dc.on_message(Box::new(move |msg: DataChannelMessage| {
            let inbound = inbound_cb.clone();
            let dropped = dropped_cb.clone();
            let ka_marker = ka_marker.clone();
            let last_ka = last_ka_cb.clone();
            let rtt = rtt_cb.clone();
            Box::pin(async move {
                let item = if msg.is_string {
                    match String::from_utf8(msg.data.to_vec()) {
                        Ok(s) => {
                            // App-level keepalive RTT: the reply is identified by
                            // the configured marker substring (vendor-supplied).
                            if let Some(marker) = &ka_marker {
                                if s.contains(marker.as_str()) {
                                    let sent = *last_ka.lock().unwrap_or_else(|p| p.into_inner());
                                    if let Some(t0) = sent {
                                        rtt.store(t0.elapsed().as_millis() as u64, Ordering::SeqCst);
                                    }
                                }
                            }
                            DcMessage::Text(s)
                        }
                        Err(e) => DcMessage::Binary(e.into_bytes()),
                    }
                } else {
                    DcMessage::Binary(msg.data.to_vec())
                };
                let mut q = inbound.lock().await;
                if q.len() >= cap {
                    q.pop_front();
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
                q.push_back(item);
            })
        }));

        // Keepalive sender: ping every interval, stamping the send time so the
        // on_message handler can compute precise RTT from the reply.
        if let Some(ka) = cfg.keepalive.clone() {
            let dc_ka = dc.clone();
            let open_ka = dc_open.clone();
            let last_ka = last_keepalive.clone();
            let state_ka = state.clone();
            tokio::spawn(async move {
                loop {
                    // Sleep first so the initial ping lands after the data-channel
                    // validation handshake completes (avoids interfering with it).
                    tokio::time::sleep(ka.interval).await;
                    if ChannelState::from_u8(state_ka.load(Ordering::SeqCst)) == ChannelState::Closed {
                        break;
                    }
                    if open_ka.load(Ordering::SeqCst) {
                        *last_ka.lock().unwrap_or_else(|p| p.into_inner()) = Some(Instant::now());
                        let _ = dc_ka.send_text(ka.text.clone()).await;
                    }
                }
            });
        }

        // Offer + bounded ICE gathering (non-trickle).
        let offer = pc.create_offer(None).await?;
        let mut gather = pc.gathering_complete_promise().await;
        pc.set_local_description(offer).await?;
        let _ = tokio::time::timeout(cfg.gather_timeout, gather.recv()).await;
        let offer_sdp = pc
            .local_description()
            .await
            .ok_or_else(|| anyhow!("no local description after ICE gathering"))?
            .sdp;

        Ok((
            WebRtcChannel {
                pc,
                dc,
                inbound,
                inbound_capacity: cap,
                dropped,
                state,
                dc_open,
                rtt_ms,
                h264_sink,
                video_taken,
                has_video: cfg.want_video,
            },
            offer_sdp,
        ))
    }

    /// Apply the remote answer SDP obtained via the caller's signaling.
    pub async fn set_answer(&self, answer_sdp: String) -> Result<()> {
        let answer = RTCSessionDescription::answer(answer_sdp)?;
        self.pc.set_remote_description(answer).await?;
        Ok(())
    }

    pub async fn dc_send_text(&self, s: String) -> Result<()> {
        if !self.dc_open.load(Ordering::SeqCst) {
            bail!("data channel not open");
        }
        self.dc.send_text(s).await.map_err(|e| anyhow!("dc send_text: {e}"))?;
        Ok(())
    }

    pub async fn dc_send_binary(&self, b: Vec<u8>) -> Result<()> {
        if !self.dc_open.load(Ordering::SeqCst) {
            bail!("data channel not open");
        }
        self.dc.send(&Bytes::from(b)).await.map_err(|e| anyhow!("dc send: {e}"))?;
        Ok(())
    }

    /// Drain all buffered inbound messages (pull model — the addon polls this).
    pub async fn dc_drain(&self) -> Vec<DcMessage> {
        let mut q = self.inbound.lock().await;
        q.drain(..).collect()
    }

    /// Drain up to `max_count` inbound messages (stopping early once accumulated
    /// raw payload would exceed `max_bytes`; always takes at least one so a single
    /// message still makes progress) and report the number still buffered
    /// afterwards, in a SINGLE inbound-lock acquisition (the host needs both per
    /// drain call and would otherwise lock the inbound queue twice).
    pub async fn dc_drain_budget_with_remaining(
        &self,
        max_count: usize,
        max_bytes: usize,
    ) -> (Vec<DcMessage>, usize) {
        let mut q = self.inbound.lock().await;
        let mut out = Vec::new();
        let mut bytes = 0usize;
        while out.len() < max_count {
            let sz = match q.front() {
                Some(DcMessage::Text(s)) => s.len(),
                Some(DcMessage::Binary(b)) => b.len(),
                None => break,
            };
            if !out.is_empty() && bytes + sz > max_bytes {
                break;
            }
            bytes += sz;
            out.push(q.pop_front().expect("front checked"));
        }
        let remaining = q.len();
        (out, remaining)
    }

    /// Current number of buffered inbound messages.
    pub async fn queue_len(&self) -> usize {
        self.inbound.lock().await.len()
    }

    /// Number of inbound messages dropped due to queue overflow (cumulative).
    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn inbound_capacity(&self) -> usize {
        self.inbound_capacity
    }

    pub fn state(&self) -> ChannelState {
        ChannelState::from_u8(self.state.load(Ordering::SeqCst))
    }

    /// Whether the data channel is currently open (peer connected != pipe usable).
    pub fn dc_open(&self) -> bool {
        self.dc_open.load(Ordering::SeqCst)
    }

    /// Latest transport round-trip time (ms) to the peer, or `None` if not yet
    /// measured. Refreshed continuously in the background.
    pub fn rtt_ms(&self) -> Option<f64> {
        let v = self.rtt_ms.load(Ordering::SeqCst);
        if v == u64::MAX {
            None
        } else {
            Some(v as f64)
        }
    }

    /// Take the inbound video as a stream of H.264 Annex-B access units
    /// (depacketized from RTP). Single consumer — returns `None` if already
    /// taken or the channel has no video. The first delivered data begins at an
    /// IDR with SPS/PPS prepended, so a decoder can start cleanly mid-stream.
    pub fn take_h264_rx(&self) -> Option<mpsc::Receiver<Bytes>> {
        if !self.has_video {
            return None;
        }
        if self.video_taken.swap(true, Ordering::SeqCst) {
            return None;
        }
        let (tx, rx) = mpsc::channel(256);
        *self.h264_sink.lock().unwrap_or_else(|p| p.into_inner()) = Some(tx);
        Some(rx)
    }

    pub async fn close(&self) -> Result<()> {
        self.pc.close().await.map_err(|e| anyhow!("peer close: {e}"))?;
        self.dc_open.store(false, Ordering::SeqCst);
        self.state.store(ChannelState::Closed.as_u8(), Ordering::SeqCst);
        Ok(())
    }
}

impl Drop for WebRtcChannel {
    fn drop(&mut self) {
        // Best-effort teardown if dropped without an explicit close() and a
        // tokio runtime is available. Deterministic cleanup is the host
        // registry's job (Chunk 1b), not best-effort addon discipline.
        if self.state() == ChannelState::Closed {
            return;
        }
        let pc = self.pc.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = pc.close().await;
            });
        }
    }
}

// =============================================================================
// H.264 video: RTP depacketize → Annex-B, with keyframe gating
// =============================================================================

/// NAL unit type from an Annex-B NAL (after the start code). 0 if too short.
fn nal_type(annexb: &[u8]) -> u8 {
    // start code is 00 00 00 01 (4B) or 00 00 01 (3B); the NAL header follows.
    let hdr = if annexb.starts_with(&[0, 0, 0, 1]) {
        annexb.get(4)
    } else if annexb.starts_with(&[0, 0, 1]) {
        annexb.get(3)
    } else {
        annexb.first()
    };
    hdr.map(|b| b & 0x1F).unwrap_or(0)
}

/// Split an Annex-B buffer (which may carry several NALs, e.g. from STAP-A) into
/// individual start-code-prefixed NAL units. Slices are zero-copy views into
/// `buf` (refcounted `Bytes`), not deep copies.
fn split_annexb(buf: &Bytes) -> Vec<Bytes> {
    // Anchor on the `0x01` byte of a `00 00 01` start code via SIMD memchr,
    // then verify the two preceding bytes. Emulation-prevention `00 00 03`
    // never matches `01`, so the verify step rejects it just like the scalar
    // scan did.
    let mut starts = Vec::new();
    for one in memchr::memchr_iter(0x01, buf) {
        if one >= 2 && buf[one - 1] == 0 && buf[one - 2] == 0 {
            let i = one - 2; // index of the leading `00 00 01`
            // 4-byte start code if preceded by an extra zero.
            let s = if i > 0 && buf[i - 1] == 0 { i - 1 } else { i };
            starts.push(s);
        }
    }
    if starts.is_empty() {
        return if buf.is_empty() { vec![] } else { vec![buf.clone()] };
    }
    let mut out = Vec::with_capacity(starts.len());
    for k in 0..starts.len() {
        let end = if k + 1 < starts.len() { starts[k + 1] } else { buf.len() };
        out.push(buf.slice(starts[k]..end));
    }
    out
}

/// Gates a NAL stream so a decoder can start (and recover) cleanly. Caches the
/// latest SPS (7) / PPS (8). Before the first IDR it drops non-parameter NALs;
/// it prepends SPS+PPS before EVERY IDR so the stream is self-healing — after
/// any drop/reset, the next IDR is independently decodable.
#[derive(Default)]
struct H264Gate {
    sps: Option<Bytes>,
    pps: Option<Bytes>,
    seen_idr: bool,
}

impl H264Gate {
    /// Re-arm: re-wait for the next IDR (after a send drop or RTP gap).
    fn reset(&mut self) {
        self.seen_idr = false;
    }

    /// Gate one NAL, emitting admitted units into the caller-provided buffer.
    /// `out` is cleared first and reused across calls so no fresh allocation
    /// happens per NAL; clones of cached SPS/PPS are refcount bumps on `Bytes`.
    fn admit(&mut self, nal: Bytes, out: &mut Vec<Bytes>) {
        out.clear();
        match nal_type(&nal) {
            7 => {
                self.sps = Some(nal.clone());
                if self.seen_idr {
                    out.push(nal);
                }
            }
            8 => {
                self.pps = Some(nal.clone());
                if self.seen_idr {
                    out.push(nal);
                }
            }
            5 => {
                // IDR — prepend the cached parameter sets, coalesced with the
                // IDR into a SINGLE buffer. The robot delimits NALs with AUDs and
                // delivers SPS/PPS in their own access units, so emitting SPS,
                // PPS and IDR as three separate buffers makes a downstream
                // h264parse frame them as separate AUs. When that parse must
                // output AVC (the fMP4 mux branch), it cannot build avcC
                // codec_data from an SPS-only AU (num_pps=0) and posts a FATAL
                // "No caps set" bus error. Concatenating SPS+PPS+IDR guarantees
                // one self-contained keyframe access unit so codec_data is always
                // constructible. Decoders accept the concatenation unchanged.
                let sps = self.sps.as_ref();
                let pps = self.pps.as_ref();
                if sps.is_some() || pps.is_some() {
                    let extra = sps.map_or(0, |s| s.len()) + pps.map_or(0, |p| p.len());
                    let mut au = Vec::with_capacity(extra + nal.len());
                    if let Some(s) = sps {
                        au.extend_from_slice(s);
                    }
                    if let Some(p) = pps {
                        au.extend_from_slice(p);
                    }
                    au.extend_from_slice(&nal);
                    out.push(Bytes::from(au));
                } else {
                    out.push(nal);
                }
                self.seen_idr = true;
            }
            _ => {
                if self.seen_idr {
                    out.push(nal);
                }
            }
        }
    }
}

/// Reads the inbound video track, depacketizes RTP → Annex-B, and forwards
/// keyframe-gated NAL units to the current sink. Drops (and re-arms the gate)
/// when the sink is full; on a closed sink it clears the binding so a fresh
/// `take_h264_rx` can reattach. RTP sequence gaps reset the depacketizer + gate
/// so a lost FU-A fragment cannot yield a corrupt NAL.
async fn h264_reader(
    track: Arc<TrackRemote>,
    sink: Arc<StdMutex<Option<mpsc::Sender<Bytes>>>>,
    video_taken: Arc<AtomicBool>,
) {
    let mut depacket = H264Packet::default();
    let mut gate = H264Gate::default();
    let mut had_sink = false;
    let mut last_seq: Option<u16> = None;
    // Reused across every admitted NAL so the gate allocates no per-NAL Vec.
    let mut admitted: Vec<Bytes> = Vec::new();
    loop {
        let (pkt, _) = match track.read_rtp().await {
            Ok(v) => v,
            Err(_) => break,
        };
        // Sequence-gap detection: drop partial FU-A state + re-wait for IDR.
        let seq = pkt.header.sequence_number;
        if let Some(prev) = last_seq {
            if seq != prev.wrapping_add(1) {
                depacket = H264Packet::default();
                gate.reset();
            }
        }
        last_seq = Some(seq);

        if pkt.payload.is_empty() {
            continue;
        }
        let annexb = match depacket.depacketize(&pkt.payload) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if annexb.is_empty() {
            continue;
        }
        let tx = {
            let guard = sink.lock().unwrap_or_else(|p| p.into_inner());
            guard.clone()
        };
        let tx = match tx {
            Some(t) => {
                if !had_sink {
                    gate.reset();
                    had_sink = true;
                }
                t
            }
            None => {
                had_sink = false;
                continue;
            }
        };
        'nals: for nal in split_annexb(&annexb) {
            gate.admit(nal, &mut admitted);
            for chunk in admitted.drain(..) {
                match tx.try_send(chunk) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        // Consumer behind — re-prime at the next IDR and DROP the
                        // rest of this admitted batch. Breaking the drain iterator
                        // discards the remaining chunks, so a half-sent IDR prefix
                        // [SPS, PPS, IDR] can never deliver the IDR without its
                        // parameter sets even if capacity frees between try_sends.
                        gate.reset();
                        break;
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        // Consumer gone — release the binding for reattach.
                        *sink.lock().unwrap_or_else(|p| p.into_inner()) = None;
                        video_taken.store(false, Ordering::SeqCst);
                        had_sink = false;
                        gate.reset();
                        break 'nals;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nal(ty: u8, body: &[u8]) -> Bytes {
        let mut v = vec![0, 0, 0, 1, ty & 0x1F];
        v.extend_from_slice(body);
        Bytes::from(v)
    }

    #[test]
    fn nal_type_reads_after_start_code() {
        assert_eq!(nal_type(&nal(7, b"x")), 7);
        assert_eq!(nal_type(&nal(5, b"x")), 5);
        assert_eq!(nal_type(&[0, 0, 1, 8]), 8); // 3-byte start code
    }

    #[test]
    fn split_annexb_separates_stap() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&nal(7, b"s"));
        buf.extend_from_slice(&nal(8, b"p"));
        buf.extend_from_slice(&nal(5, b"i"));
        let buf = Bytes::from(buf);
        let parts = split_annexb(&buf);
        assert_eq!(parts.len(), 3);
        assert_eq!(nal_type(&parts[0]), 7);
        assert_eq!(nal_type(&parts[2]), 5);
    }

    #[test]
    fn split_annexb_mixed_3b_4b_exact_boundaries() {
        // 3-byte start (00 00 01) then a back-to-back 4-byte start (00 00 00 01).
        let buf = Bytes::from(vec![0, 0, 1, 9, 0xAB, 0, 0, 0, 1, 0x41, 0xCD]);
        let parts = split_annexb(&buf);
        assert_eq!(parts.len(), 2);
        assert_eq!(&parts[0][..], &[0, 0, 1, 9, 0xAB]);
        assert_eq!(&parts[1][..], &[0, 0, 0, 1, 0x41, 0xCD]); // extra zero folds into NAL2
    }

    #[test]
    fn split_annexb_emulation_prevention_not_a_start() {
        // 00 00 03 inside the body must NOT split (it's not 00 00 01).
        let buf = Bytes::from(vec![0, 0, 1, 5, 0, 0, 3, 0, 0xFF]);
        let parts = split_annexb(&buf);
        assert_eq!(parts.len(), 1);
        assert_eq!(&parts[0][..], &buf[..]);
    }

    #[test]
    fn split_annexb_lone_0x01_ignored() {
        // A 0x01 not preceded by 00 00 is not a start code.
        let buf = Bytes::from(vec![0, 0, 1, 7, 0x12, 0x01, 0x34]);
        let parts = split_annexb(&buf);
        assert_eq!(parts.len(), 1);
        assert_eq!(&parts[0][..], &buf[..]);
    }

    #[test]
    fn split_annexb_no_underflow_at_index_0_and_1() {
        // 0x01 at index 0 / index 1 must not underflow the i-1/i-2 checks.
        for buf in [Bytes::from(vec![1, 2, 3]), Bytes::from(vec![0, 1, 2])] {
            let parts = split_annexb(&buf);
            assert_eq!(parts.len(), 1);
            assert_eq!(&parts[0][..], &buf[..]);
        }
    }

    #[test]
    fn split_annexb_no_start_and_empty() {
        let none = Bytes::from(vec![9, 8, 7]);
        let parts = split_annexb(&none);
        assert_eq!(parts.len(), 1);
        assert_eq!(&parts[0][..], &none[..]);
        assert!(split_annexb(&Bytes::new()).is_empty());
    }

    fn admit(g: &mut H264Gate, n: Bytes) -> Vec<Bytes> {
        let mut out = Vec::new();
        g.admit(n, &mut out);
        out
    }

    #[test]
    fn gate_waits_for_idr_and_prepends_params() {
        let mut g = H264Gate::default();
        // P-frame before any IDR → dropped.
        assert!(admit(&mut g, nal(1, b"p")).is_empty());
        // SPS/PPS cached, not emitted yet.
        assert!(admit(&mut g, nal(7, b"s")).is_empty());
        assert!(admit(&mut g, nal(8, b"p")).is_empty());
        // IDR → emits ONE coalesced buffer: SPS|PPS|IDR concatenated so a
        // downstream parser frames it as a single self-contained keyframe AU.
        let out = admit(&mut g, nal(5, b"i"));
        assert_eq!(out.len(), 1);
        let parts = split_annexb(&out[0]);
        assert_eq!(parts.len(), 3);
        assert_eq!(nal_type(&parts[0]), 7);
        assert_eq!(nal_type(&parts[1]), 8);
        assert_eq!(nal_type(&parts[2]), 5);
        // After start, everything passes through.
        let out2 = admit(&mut g, nal(1, b"p"));
        assert_eq!(out2.len(), 1);
        assert_eq!(nal_type(&out2[0]), 1);
    }

    #[test]
    fn gate_prepends_params_before_every_idr() {
        let mut g = H264Gate::default();
        admit(&mut g, nal(7, b"s"));
        admit(&mut g, nal(8, b"p"));
        let _ = admit(&mut g, nal(5, b"i")); // first IDR primes
        // A later IDR still carries SPS+PPS in its coalesced keyframe buffer
        // (self-healing stream).
        let out = admit(&mut g, nal(5, b"i2"));
        assert_eq!(out.len(), 1);
        let parts = split_annexb(&out[0]);
        assert_eq!(parts.len(), 3);
        assert_eq!(nal_type(&parts[0]), 7);
        assert_eq!(nal_type(&parts[2]), 5);
    }

    #[test]
    fn gate_reset_rewaits_for_idr() {
        let mut g = H264Gate::default();
        admit(&mut g, nal(7, b"s"));
        admit(&mut g, nal(8, b"p"));
        let _ = admit(&mut g, nal(5, b"i"));
        g.reset();
        // After reset, P-frames are dropped again until the next IDR.
        assert!(admit(&mut g, nal(1, b"p")).is_empty());
        let out = admit(&mut g, nal(5, b"i"));
        assert_eq!(out.len(), 1);
        assert_eq!(split_annexb(&out[0]).len(), 3);
    }
}

