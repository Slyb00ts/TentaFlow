// =============================================================================
// Plik: modules/meeting-live.js
// Opis: Live view spotkania — 2-kolumnowy layout (transkrypt+dzialania+summary
//       po lewej, sidebar z uczestnikami/AI summary/backend po prawej) + sticky
//       footer. Subskrybuje unsolicited MeetingLiveEventBody filtrujac po
//       meeting_key i aktualizuje stan reaktywnie. Inicjalny stan pobrany przez
//       MeetingSessionDetail + MeetingSummariesList + MeetingActionItemsList.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';

// Module-scoped state — resetowany w mount().
const state = {
  meetingKey: null,
  sessionDetail: null,
  transcript: [],
  participants: new Map(),
  summaries: [],
  actionItems: [],
  backend: {
    sttModel: '',
    ttsModel: '',
    summarizationModel: '',
    diarizationModel: '',
    streamingLatencyMs: null,
    enrolledSpeakers: null,
    totalParticipants: null,
  },
  latencyHistory: [],
  lastTranscriptAt: 0,
  aiInsightsEnabled: true,
  activeTab: 'transcript',
  groupsCollapsed: { pending: false, done: true, cancelled: true },
  // Bot lifecycle — 'joined' means the LIVE chip is real; other values mean
  // the bot is still setting up and we show a pending chip instead.
  lifecycleStage: 'idle',
  lifecycleDetails: '',
  // Zbior flag oznaczajacych ktore sekcje wymagaja repaintu po zlapaniu
  // batcha eventow. Pelny re-render robimy tylko gdy ktos doda 'all'
  // (initial mount). Pozostale przypadki to surgical update DOM.
  dirty: new Set(),
  // Wtorny indeks atrybutow z ParticipantAttributes — kluczowany po
  // participantId (DOM id z Teams) ORAZ po lowercase nazwie. Pozwala
  // skleic emocje/wiek z roster entry nawet jezeli atrybuty przyszly
  // przed RosterSnapshot albo nazwy nie matchuja sie 1:1.
  participantAttrsByDomId: new Map(),
};

let unsubscribeLive = null;
let footerTimer = null;
// Coalesce wielu eventow w jednej klatce — bez tego batch rosteru (np. 5
// uczestnikow w jednym tick'u) wymusza N pelnych re-renderow.
let pendingRenderRaf = 0;

// Laczy wiele wywolan w jeden render na klatce. Synchroniczny renderAll()
// zostaje dla initial load i akcji uzytkownika, gdzie nie ma kontencji eventow.
function scheduleRender() {
  if (pendingRenderRaf) return;
  pendingRenderRaf = requestAnimationFrame(() => {
    pendingRenderRaf = 0;
    renderAll();
  });
}

// Zaznacza wszystkie sekcje jako dirty — uzywane przy initial mount, kiedy
// chcemy by renderAll wykonal pelny paint (renderBody przebuduje siatke).
function markAllDirty() {
  state.dirty.add('header');
  state.dirty.add('lifecycle');
  state.dirty.add('transcript');
  state.dirty.add('participants');
  state.dirty.add('actions');
  state.dirty.add('summary');
  state.dirty.add('backend');
  // Marker pelnego rebudowy body — rozni sie od pojedynczych sekcji bo
  // wymusza przejscie przez renderBody() zamiast surgical update.
  state.dirty.add('full');
}

// --- Lifecycle --------------------------------------------------------------

const MeetingLiveScreen = {
  get title() {
    return I18n.t('meeting.live.title');
  },
  // Meeting key trzymany w module-local, ustawiany przez openLive() zanim
  // router wola render. Router nie wspiera parametryzowanych sciezek, wiec
  // wywolanie z zewnatrz idzie przez eksportowany helper.
  render() {
    return `<div id="meeting-live-root"></div>`;
  },
  async mount() {
    if (!state.meetingKey) {
      const root = byId('meeting-live-root');
      if (root) {
        root.innerHTML = `<div class="tf-section-card"><p>${escapeHtml(I18n.t('meeting.live.missing_key'))}</p></div>`;
      }
      return;
    }
    resetState(state.meetingKey);
    renderShell();
    await loadInitialData();
    subscribeLive();
    startFooterTimer();
    // Pierwszy render musi zbudowac calosc — markeryjemy wszystkie sekcje
    // jako dirty, renderAll wykona pelny initial paint przez renderBody().
    markAllDirty();
    renderAll();
  },
  unmount() {
    stopFooterTimer();
    if (pendingRenderRaf) {
      cancelAnimationFrame(pendingRenderRaf);
      pendingRenderRaf = 0;
    }
    if (unsubscribeLive) {
      try { unsubscribeLive(); } catch (_) { /* no-op */ }
      unsubscribeLive = null;
    }
    state.meetingKey = null;
    state.sessionDetail = null;
    state.transcript = [];
    state.participants = new Map();
    state.summaries = [];
    state.actionItems = [];
    state.latencyHistory = [];
    state.dirty = new Set();
    state.participantAttrsByDomId = new Map();
  },
};

/** Wchodzimy do live view — ustawiamy klucz i nawigujemy do route. */
export function openMeetingLive(meetingKey) {
  state.meetingKey = String(meetingKey || '');
}

function resetState(meetingKey) {
  state.meetingKey = meetingKey;
  state.sessionDetail = null;
  state.transcript = [];
  state.participants = new Map();
  state.summaries = [];
  state.actionItems = [];
  state.backend = {
    sttModel: '',
    ttsModel: '',
    summarizationModel: '',
    diarizationModel: '',
    streamingLatencyMs: null,
    enrolledSpeakers: null,
    totalParticipants: null,
  };
  state.latencyHistory = [];
  state.lastTranscriptAt = 0;
  state.aiInsightsEnabled = true;
  state.activeTab = 'transcript';
  state.groupsCollapsed = { pending: false, done: true, cancelled: true };
  state.lifecycleStage = 'idle';
  state.lifecycleDetails = '';
  state.dirty = new Set();
  state.participantAttrsByDomId = new Map();
}

// Hydrates state.backend from a MeetingSessionDescriptor returned by the
// backend. Lets the live view render the BACKEND panel on mount even if the
// bot's BackendUpdate broadcast fired before this dashboard was open.
// Wasm bridge maps "not reported yet" to JS null/empty — only overwrite the
// in-memory value when the descriptor carries a concrete one so a later
// MeetingEvent broadcast can still refine it.
function seedBackendFromDescriptor(desc) {
  if (!desc) return;
  if (desc.backendSttModel) state.backend.sttModel = String(desc.backendSttModel);
  if (desc.backendTtsModel) state.backend.ttsModel = String(desc.backendTtsModel);
  if (desc.backendSummarizationModel) {
    state.backend.summarizationModel = String(desc.backendSummarizationModel);
  }
  if (desc.backendDiarizationModel) {
    state.backend.diarizationModel = String(desc.backendDiarizationModel);
  }
  if (desc.backendStreamingLatencyMs != null) {
    state.backend.streamingLatencyMs = Number(desc.backendStreamingLatencyMs);
  }
  if (desc.backendEnrolledSpeakers != null) {
    state.backend.enrolledSpeakers = Number(desc.backendEnrolledSpeakers);
  }
  if (desc.backendTotalParticipants != null) {
    state.backend.totalParticipants = Number(desc.backendTotalParticipants);
  }
}

// --- Data loading -----------------------------------------------------------

async function loadInitialData() {
  const meetingKey = state.meetingKey;
  // Szukamy sesji przez meetingSessionList — detail wymaga session_id.
  let sessionId = null;
  try {
    const resp = await ApiBinary.one('meetingSessionListRequest', { onlyMine: false });
    const sessions = Array.isArray(resp?.sessions) ? resp.sessions : [];
    const match = sessions.find((s) => s.meetingKey === meetingKey);
    if (match) {
      sessionId = match.sessionId;
      state.sessionDetail = match;
      if (match.lifecycleStage) state.lifecycleStage = match.lifecycleStage;
      if (match.lifecycleDetails) state.lifecycleDetails = match.lifecycleDetails;
      seedBackendFromDescriptor(match);
    }
  } catch (e) {
    toast(e?.message || I18n.t('meeting.live.load_failed'), 'error');
  }

  // Detail + pelna historia transkryptu (snapshot) — pozniej live eventy dokladaja.
  if (sessionId != null) {
    try {
      const det = await ApiBinary.one('meetingSessionDetailRequest', {
        sessionId,
        includeTranscripts: true,
      });
      if (det?.session) {
        state.sessionDetail = det.session;
        if (det.session.lifecycleStage) state.lifecycleStage = det.session.lifecycleStage;
        if (det.session.lifecycleDetails) state.lifecycleDetails = det.session.lifecycleDetails;
        seedBackendFromDescriptor(det.session);
      }
      const entries = Array.isArray(det?.transcripts) ? det.transcripts : [];
      // Mapujemy stary format transcript entry na live event shape.
      state.transcript = entries.map((t) => ({
        timestampMs: Number(t.timestampMs || 0),
        speakerId: String(t.speaker || ''),
        speakerName: t.speaker || '',
        isEnrolled: !!t.isEnrolled,
        speakerConfidence: typeof t.confidence === 'number' ? t.confidence : null,
        text: String(t.text || ''),
        language: null,
        resolvedSttModel: t.model || '',
        latencyMs: 0,
      }));
      // Ostatni timestamp dla footer "Xs temu".
      if (state.transcript.length) {
        state.lastTranscriptAt = state.transcript[state.transcript.length - 1].timestampMs;
      }
    } catch (e) {
      toast(e?.message || I18n.t('meeting.live.load_failed'), 'error');
    }
  }

  // Summaries + action items (z DB).
  try {
    const [sumResp, aiResp] = await Promise.all([
      ApiBinary.one('meetingSummariesListRequest', { meetingKey, limit: 20 }).catch(() => null),
      ApiBinary.one('meetingActionItemsListRequest', { meetingKey }).catch(() => null),
    ]);
    const sumItems = Array.isArray(sumResp?.items) ? sumResp.items : [];
    state.summaries = sumItems.map((s) => ({
      id: Number(s.id || 0),
      createdAt: String(s.createdAt || ''),
      decisionsText: String(s.decisionsText || ''),
      summaryText: String(s.summaryText || ''),
      model: String(s.model || ''),
      timestampMs: parseIsoMs(s.createdAt),
    }));
    const aiItems = Array.isArray(aiResp?.items) ? aiResp.items : [];
    state.actionItems = aiItems.map((a) => ({
      id: Number(a.id || 0),
      owner: String(a.owner || ''),
      task: String(a.task || ''),
      deadline: a.deadline || null,
      status: String(a.status || 'pending'),
      createdAt: String(a.createdAt || ''),
      updatedAt: String(a.updatedAt || ''),
    }));
  } catch (_) {
    // Graceful — puste listy juz zainicjalizowane.
  }
}

function parseIsoMs(iso) {
  if (!iso) return 0;
  try {
    const t = new Date(String(iso).replace(' ', 'T') + (String(iso).endsWith('Z') ? '' : 'Z'));
    return t.getTime() || 0;
  } catch (_) {
    return 0;
  }
}

// --- Live subscription ------------------------------------------------------

async function subscribeLive() {
  try {
    const client = await ApiBinary.client();
    unsubscribeLive = client.addUnsolicitedListener(({ body }) => {
      if (!body || body.variant !== 'MeetingLiveEventBody') return;
      if (body.meetingKey !== state.meetingKey) return;
      const payload = body.payload;
      if (!payload || !payload.type) return;
      applyLiveEvent(Number(body.timestampMs || Date.now()), payload.type, payload.data || {});
      scheduleRender();
    });
  } catch (e) {
    console.warn('[meeting-live] subscribeLive failed:', e?.message);
  }
}

function applyLiveEvent(timestampMs, type, data) {
  switch (type) {
    case 'TranscriptEntry': {
      const entry = {
        timestampMs,
        speakerId: String(data.speakerId || ''),
        speakerName: data.speakerName || data.speakerId || '',
        isEnrolled: !!data.isEnrolled,
        speakerConfidence: typeof data.speakerConfidence === 'number' ? data.speakerConfidence : null,
        text: String(data.text || ''),
        language: data.language || null,
        resolvedSttModel: String(data.resolvedSttModel || ''),
        latencyMs: Number(data.latencyMs || 0),
      };
      state.transcript.push(entry);
      state.lastTranscriptAt = timestampMs;
      if (entry.latencyMs > 0) {
        state.latencyHistory.push(entry.latencyMs);
        if (state.latencyHistory.length > 10) state.latencyHistory.shift();
      }
      // Aktywny mowca — update participants map zachowujac wzbogacone
      // pola (emocje, wiek, plec) z poprzedniego stanu.
      if (entry.speakerId) {
        const prev = state.participants.get(entry.speakerId) || {};
        state.participants.set(entry.speakerId, {
          ...prev,
          speakerId: entry.speakerId,
          speakerName: entry.speakerName || prev.speakerName || entry.speakerId,
          isEnrolled: entry.isEnrolled || prev.isEnrolled || false,
          status: 'active_now',
          lastSpokenAt: timestampMs,
        });
      }
      state.dirty.add('transcript');
      state.dirty.add('participants');
      state.dirty.add('header');
      break;
    }
    case 'RosterSnapshot': {
      // Snapshot zastepuje caly roster — bot wysyla go raz na DOM scan.
      // Brak entry = brak uczestnika (a nie "wyszedl" jako osobny event).
      // KRYTYCZNE: nie nadpisujemy emocji/wieku/plci, bo bot ich nie
      // wysyla w roster — przyszly wczesniejszym ParticipantAttributes
      // i muszą przezyc snapshot. Fallback na inne pola id zostawia bramke
      // jezeli bot zmieni schemat w przyszlosci.
      const entries = Array.isArray(data.entries) ? data.entries : [];
      const next = new Map();
      for (const entry of entries) {
        const id = String(entry.speakerId || entry.participantId || entry.id || '');
        if (!id) continue;
        const prev = state.participants.get(id) || {};
        next.set(id, {
          speakerId: id,
          speakerName: entry.speakerName || prev.speakerName || id,
          isEnrolled: prev.isEnrolled || false,
          status: String(entry.status || 'joined'),
          lastSpokenAt: entry.lastSpokenAgoSec != null
            ? timestampMs - Number(entry.lastSpokenAgoSec) * 1000
            : prev.lastSpokenAt || 0,
          // Preserve atrybutow vision pipeline.
          emotion: prev.emotion || null,
          emotionConfidence: prev.emotionConfidence != null ? prev.emotionConfidence : null,
          age: prev.age != null ? prev.age : null,
          genderMaleProb: prev.genderMaleProb != null ? prev.genderMaleProb : null,
          attributesUpdatedAt: prev.attributesUpdatedAt || 0,
          hasVideo: entry.hasVideo != null ? Boolean(entry.hasVideo) : (prev.hasVideo || false),
          hasAudio: entry.hasAudio != null ? Boolean(entry.hasAudio) : (prev.hasAudio || false),
        });
      }
      // Sklejenie atrybutow z secondary index — moze byc tak ze
      // ParticipantAttributes przyszlo zanim diarization wyprodukowalo
      // speakerId; po snapshot nazwy juz pasuja, wiec dociagamy emocje.
      for (const p of next.values()) {
        const nameKey = String(p.speakerName || '').toLowerCase();
        const cached = nameKey ? state.participantAttrsByDomId.get(nameKey) : null;
        if (cached && (!p.attributesUpdatedAt || cached.timestamp > p.attributesUpdatedAt)) {
          p.emotion = cached.emotion;
          p.emotionConfidence = cached.emotionConfidence;
          p.age = cached.age;
          p.genderMaleProb = cached.genderMaleProb;
          p.attributesUpdatedAt = cached.timestamp;
        }
      }
      state.participants = next;
      state.dirty.add('participants');
      state.dirty.add('header');
      break;
    }
    case 'SummaryUpdate': {
      state.summaries.unshift({
        id: 0,
        createdAt: new Date(timestampMs).toISOString(),
        timestampMs,
        decisionsText: String(data.decisionsText || ''),
        summaryText: String(data.summaryText || ''),
        model: String(data.model || ''),
      });
      state.dirty.add('summary');
      state.dirty.add('header');
      break;
    }
    case 'ActionItemsUpdate': {
      const items = Array.isArray(data.items) ? data.items : [];
      for (const item of items) {
        const owner = String(item.owner || '');
        const task = String(item.task || '');
        const deadline = item.deadline || null;
        const idx = state.actionItems.findIndex(
          (a) => a.owner === owner && a.task === task,
        );
        if (idx >= 0) {
          // Live update bez id (DB upsert nadal sie dzieje po stronie routera;
          // kolejny reload poda id). Deadline moze przyjsc nowszy.
          state.actionItems[idx].deadline = deadline;
        } else {
          state.actionItems.push({
            id: 0,
            owner,
            task,
            deadline,
            status: 'pending',
            createdAt: '',
            updatedAt: '',
          });
        }
      }
      state.dirty.add('actions');
      state.dirty.add('header');
      break;
    }
    case 'BackendUpdate': {
      state.backend = {
        sttModel: String(data.sttModel || state.backend.sttModel || ''),
        ttsModel: String(data.ttsModel || state.backend.ttsModel || ''),
        summarizationModel: String(data.summarizationModel || state.backend.summarizationModel || ''),
        diarizationModel: String(data.diarizationModel || state.backend.diarizationModel || ''),
        streamingLatencyMs: data.streamingLatencyMs != null
          ? Number(data.streamingLatencyMs)
          : state.backend.streamingLatencyMs,
        enrolledSpeakers: data.enrolledSpeakers != null
          ? Number(data.enrolledSpeakers)
          : state.backend.enrolledSpeakers,
        totalParticipants: data.totalParticipants != null
          ? Number(data.totalParticipants)
          : state.backend.totalParticipants,
      };
      state.dirty.add('backend');
      break;
    }
    case 'LifecycleUpdate': {
      state.lifecycleStage = String(data.stage || state.lifecycleStage);
      state.lifecycleDetails = data.details ? String(data.details) : '';
      state.dirty.add('lifecycle');
      state.dirty.add('header');
      break;
    }
    case 'ParticipantAttributes': {
      // Pipeline rozpoznawania emocji + wieku + plci. `participantId` z DOM
      // Teams (typowo = display name) — `state.participants` kluczowane
      // po `speakerId` z diarization. Dopasowanie:
      //  1. case-insensitive equality (speakerName ↔ data.name),
      //  2. case-insensitive substring (np. "Jan Kowalski" vs "Jan Kowalski (Guest)"),
      //  3. brak targetu => tylko zapis do secondary index, snapshot
      //     potem dociagnie atrybuty.
      const nameLower = String(data.name || '').trim().toLowerCase();
      const domId = String(data.participantId || '').trim();
      const emotion = data.emotion || null;
      const emotionConfidence = typeof data.emotionConfidence === 'number'
        ? data.emotionConfidence
        : null;
      const age = typeof data.age === 'number' ? data.age : null;
      const genderMaleProb = typeof data.genderMaleProb === 'number'
        ? data.genderMaleProb
        : null;

      let target = null;
      if (nameLower) {
        for (const p of state.participants.values()) {
          const candidates = [
            String(p.speakerName || '').trim().toLowerCase(),
            String(p.speakerId || '').trim().toLowerCase(),
          ];
          const match = candidates.some((c) => {
            if (!c) return false;
            if (c === nameLower) return true;
            // Substring matching tylko dla rozsadnie dlugich nazw —
            // chronimy przed false-positive na 1-2 znakowych prefixach.
            if (c.length >= 3 && (c.includes(nameLower) || nameLower.includes(c))) return true;
            return false;
          });
          if (match) { target = p; break; }
        }
      }

      if (target) {
        target.emotion = emotion;
        target.emotionConfidence = emotionConfidence;
        target.age = age;
        target.genderMaleProb = genderMaleProb;
        target.attributesUpdatedAt = timestampMs;
      }

      // Zapis do secondary index po wszystkich znanych kluczach. Roster
      // snapshot pozniej skleci atrybuty z kazdym entry vbo nazwie.
      const indexKeys = [domId, nameLower].filter(Boolean);
      for (const key of indexKeys) {
        state.participantAttrsByDomId.set(key.toLowerCase(), {
          emotion,
          emotionConfidence,
          age,
          genderMaleProb,
          timestamp: timestampMs,
        });
      }
      state.dirty.add('participants');
      break;
    }
    default:
      // Nieznane warianty ignorujemy (forward-compat).
      break;
  }
}

// --- Footer refresh ---------------------------------------------------------

// Footer "ostatni wpis Xs temu" wymaga tickera — reszta UI reaguje na eventy.
function startFooterTimer() {
  stopFooterTimer();
  footerTimer = setInterval(() => {
    const footer = byId('meet-live-footer');
    if (footer) footer.innerHTML = footerHtml();
  }, 1000);
}

function stopFooterTimer() {
  if (footerTimer) {
    clearInterval(footerTimer);
    footerTimer = null;
  }
}

// --- Render helpers ---------------------------------------------------------

function renderShell() {
  const root = byId('meeting-live-root');
  if (!root) return;
  root.innerHTML = `
    <tf-screen>
      <div slot="breadcrumb" class="tf-breadcrumb" id="meet-live-crumbs"></div>
      <div slot="header" id="meet-live-header"></div>
      <div id="meet-live-body"></div>
      <footer class="meet-footer" id="meet-live-footer"></footer>
    </tf-screen>
  `;
}

function renderAll() {
  // Partial-update pipeline: surgical updates per zmienionych sekcji.
  // Pelny rebuild (renderBody) odpalamy tylko gdy initial mount oznaczyl
  // 'full' albo gdy DOM body w ogole nie istnieje jeszcze.
  const dirty = state.dirty;
  if (dirty.size === 0) return;

  // Breadcrumb zawsze trzymamy aktualny — to jeden DOM node, koszt zerowy.
  if (!byId('meet-live-crumbs')?.firstChild) {
    renderBreadcrumb();
  }

  const wantsFull = dirty.has('full') || !byId('meet-live-body')?.firstChild;
  if (wantsFull) {
    renderBreadcrumb();
    renderHeader();
    renderBody();
    const footer = byId('meet-live-footer');
    if (footer) footer.innerHTML = footerHtml();
    state.dirty.clear();
    return;
  }

  if (dirty.has('header') || dirty.has('lifecycle')) {
    applyHeaderUpdate();
  }
  if (dirty.has('participants')) {
    applyParticipantsUpdate();
  }
  if (dirty.has('transcript') && state.activeTab === 'transcript') {
    applyTranscriptAppend();
  }
  if (dirty.has('actions')) {
    applyActionsUpdate();
  }
  if (dirty.has('summary')) {
    applySummaryUpdate();
  }
  if (dirty.has('backend')) {
    applyBackendUpdate();
  }
  // Footer ma osobny ticker (1 Hz), ale zaraz po zmianie chcemy odswiezyc
  // od razu zeby nie czekac do nastepnego tick'u.
  const footer = byId('meet-live-footer');
  if (footer) footer.innerHTML = footerHtml();

  state.dirty.clear();
}

// --- Surgical updates -------------------------------------------------------

// Aktualizuje header (subtitle z licznikami, lifecycle chip) bez burzenia
// caleo node — szuka konkretnych spans i podmienia textContent. Fallback do
// pelnego renderHeader jezeli root nie istnieje.
function applyHeaderUpdate() {
  const host = byId('meet-live-header');
  if (!host || !host.firstChild) {
    renderHeader();
    return;
  }
  const sub = host.querySelector('.d-sub');
  if (sub) {
    const s = state.sessionDetail || {};
    const participantsCount = state.participants.size;
    const durationLabel = formatDuration(Date.now() - parseIsoMs(s.startedAt));
    const platform = s.platform || '—';
    sub.innerHTML = `${I18n.t('meeting.live.subtitle_participants', { n: participantsCount })} · ${durationLabel} · ${escapeHtml(platform)}`;
  }
  // Chip jest osobnym elementem w .d-name — najprosciej wymienic tylko go.
  const dName = host.querySelector('.d-name');
  if (dName) {
    const chipNode = dName.querySelector('tf-chip');
    const newChipHtml = renderLifecycleChip();
    if (chipNode) {
      const tmp = document.createElement('div');
      tmp.innerHTML = newChipHtml;
      const fresh = tmp.firstElementChild;
      if (fresh) chipNode.replaceWith(fresh);
    } else {
      dName.insertAdjacentHTML('beforeend', newChipHtml);
    }
  }
}

// Surgical diff listy uczestnikow: dla kazdego speaker_id zachowuje istniejacy
// row jezeli jest, aktualizuje tylko zmienione atrybuty, dodaje brakujace
// i usuwa te ktorych juz nie ma w stanie.
function applyParticipantsUpdate() {
  const container = document.getElementById('meet-participants-list');
  if (!container) {
    // Fallback — sidebar jeszcze nie zmontowany, robimy pelny rebuild body.
    renderBody();
    return;
  }
  const list = Array.from(state.participants.values())
    .sort((a, b) => (b.lastSpokenAt || 0) - (a.lastSpokenAt || 0));

  // Aktualizacja licznika w naglowku karty (h3 .counter).
  const card = container.closest('.tf-section-card');
  const counter = card?.querySelector('h3 .counter');
  if (counter) {
    const count = list.length || state.backend.totalParticipants || 0;
    counter.textContent = `(${count})`;
  }

  // Empty state ↔ rzeczywista lista. Jezeli prev byl empty, wymieniamy
  // calosc. Jezeli prev byl listing — diff per row.
  const hadEmpty = container.querySelector('.users-empty');
  if (list.length === 0) {
    if (!hadEmpty) {
      container.innerHTML = `<div class="users-empty">${escapeHtml(I18n.t('meeting.live.no_participants'))}</div>`;
    }
    return;
  }
  if (hadEmpty) {
    container.innerHTML = list.map(renderParticipantRow).join('');
    return;
  }

  const existing = new Map();
  container.querySelectorAll('[data-speaker-id]').forEach((el) => {
    existing.set(el.dataset.speakerId, el);
  });

  const seen = new Set();
  list.forEach((p, idx) => {
    seen.add(p.speakerId);
    let row = existing.get(p.speakerId);
    if (!row) {
      // Nowy uczestnik — buduje przez istniejacy renderParticipantRow.
      const tmp = document.createElement('div');
      tmp.innerHTML = renderParticipantRow(p);
      row = tmp.firstElementChild;
      if (row) container.appendChild(row);
    } else {
      updateParticipantRow(row, p);
    }
    if (row) row.style.order = String(idx);
  });

  // Usun rows nie obecne w aktualnym snapshot.
  for (const [id, el] of existing) {
    if (!seen.has(id)) el.remove();
  }
}

// Aktualizuje pojedynczy row uczestnika na miejscu — tylko te atrybuty
// ktore moga sie zmienic miedzy renderami (speaking flag, emocje, wiek,
// czas ostatniej wypowiedzi).
function updateParticipantRow(row, p) {
  const speakingNow = p.status === 'active_now' && (Date.now() - (p.lastSpokenAt || 0) < 5000);
  row.classList.toggle('speaking', speakingNow);

  const nameEl = row.querySelector('.p-name');
  if (nameEl) {
    const nm = p.speakerName || p.speakerId || '?';
    if (nameEl.textContent !== nm) nameEl.textContent = nm;
  }

  const subEl = row.querySelector('.p-sub');
  if (subEl) {
    const sub = speakingNow
      ? `${p.isEnrolled ? 'Enrolled' : I18n.t('meeting.live.not_enrolled')} · ${I18n.t('meeting.live.participant_speaking_now')}`
      : p.lastSpokenAt
        ? `${p.isEnrolled ? 'Enrolled' : I18n.t('meeting.live.not_enrolled')} · ${I18n.t('meeting.live.participant_spoke_ago', { n: Math.max(1, Math.floor((Date.now() - p.lastSpokenAt) / 1000)) })}`
        : (p.isEnrolled ? 'Enrolled' : I18n.t('meeting.live.not_enrolled'));
    if (subEl.textContent !== sub) subEl.textContent = sub;
  }

  // Emotion badge — dodaj/podmien/usun w zaleznosci od stanu atrybutow.
  const wrap = row.querySelector('.p-avatar-wrap');
  let badge = wrap?.querySelector('.p-emotion-badge');
  const stale = p.attributesUpdatedAt
    ? (Date.now() - p.attributesUpdatedAt) > ATTRIBUTES_STALE_AFTER_MS
    : false;
  if (p.emotion && EMOTION_VISUAL[p.emotion] && wrap) {
    const v = EMOTION_VISUAL[p.emotion];
    const conf = typeof p.emotionConfidence === 'number'
      ? `${(p.emotionConfidence * 100).toFixed(0)}%`
      : '';
    const title = `${p.emotion}${conf ? ' ' + conf : ''}`;
    if (!badge) {
      badge = document.createElement('span');
      badge.className = 'p-emotion-badge';
      wrap.appendChild(badge);
    }
    if (badge.textContent !== v.emoji) badge.textContent = v.emoji;
    badge.style.background = v.bg;
    badge.title = title;
    badge.classList.toggle('stale', stale);
  } else if (badge) {
    badge.remove();
  }

  // Wiek + plec — div .p-attrs pod nazwa.
  const meta = row.querySelector('.p-meta');
  let attrs = meta?.querySelector('.p-attrs');
  if ((p.age != null || p.genderMaleProb != null) && meta) {
    const parts = [];
    if (typeof p.age === 'number') {
      parts.push(`<span class="p-attr-age">~${Math.round(p.age)} ${escapeHtml(I18n.t('meeting.live.attr_years_short'))}</span>`);
    }
    if (typeof p.genderMaleProb === 'number') {
      const symbol = p.genderMaleProb > 0.5 ? '♂' : '♀';
      parts.push(`<span class="p-attr-gender">${symbol}</span>`);
    }
    if (!attrs) {
      attrs = document.createElement('div');
      attrs.className = 'p-attrs';
      meta.appendChild(attrs);
    }
    attrs.innerHTML = parts.join('');
    attrs.classList.toggle('stale', stale);
  } else if (attrs) {
    attrs.remove();
  }
}

// Append najnowszego transcript entry zamiast rebudowy listy. Sprawdzamy
// czy scroll jest "u dolu" — tylko wtedy auto-scrollujemy, zeby nie zbic
// uzytkownikowi pozycji gdy przewinal sam do gory.
function applyTranscriptAppend() {
  const host = byId('meet-live-transcript-scroll');
  if (!host) {
    // Tab transcript zmontowany, ale scroller nie istnieje — np. byl
    // wczesniej empty state. Pelny rebuild zakladki.
    const body = byId('meet-live-tab-body');
    if (body && state.activeTab === 'transcript') {
      body.innerHTML = renderActiveTab();
      scrollTranscriptToBottom();
    }
    return;
  }
  const last = state.transcript[state.transcript.length - 1];
  if (!last) return;
  const wasAtBottom = (host.scrollTop + host.clientHeight) >= (host.scrollHeight - 50);
  // Append jednego node — taniej niz innerHTML +=.
  const tmp = document.createElement('div');
  tmp.innerHTML = renderTranscriptEntry(last);
  const node = tmp.firstElementChild;
  if (node) host.appendChild(node);
  if (wasAtBottom) scrollTranscriptToBottom();

  // Aktualizacja countera w tabsie.
  const tabsEl = byId('meet-live-tabs');
  const tabTranscript = tabsEl?.querySelector('tf-tab[id="transcript"]');
  if (tabTranscript) tabTranscript.setAttribute('count', String(state.transcript.length));
}

function applyActionsUpdate() {
  // Aktualizacja countera w tabsie zawsze.
  const tabsEl = byId('meet-live-tabs');
  const tabActions = tabsEl?.querySelector('tf-tab[id="actions"]');
  if (tabActions) tabActions.setAttribute('count', String(state.actionItems.length));

  // Sidebar AI summary card — odswiez liste pending action items.
  const aside = document.querySelector('.meet-side');
  if (aside) {
    const cards = aside.querySelectorAll('.tf-section-card');
    // Drugi card to AI summary (po participants). Zachowawczo zastepujemy
    // tylko jego markup — dzieki zorzedowanej kolejnosci w renderBody.
    if (cards[1]) cards[1].outerHTML = renderAiSummaryCard();
  }

  if (state.activeTab === 'actions') {
    const body = byId('meet-live-tab-body');
    if (body) {
      body.innerHTML = renderActionsTab();
      wireActiveTab();
    }
  }
}

function applySummaryUpdate() {
  // Sidebar — ten sam card jak w applyActionsUpdate (AI summary zawiera
  // decisions + action items). Pojedynczy outerHTML swap to najprostsza
  // forma, koszt mikroskopijny bo card to ~30 linii markupu.
  const aside = document.querySelector('.meet-side');
  if (aside) {
    const cards = aside.querySelectorAll('.tf-section-card');
    if (cards[1]) cards[1].outerHTML = renderAiSummaryCard();
  }
  if (state.activeTab === 'summary') {
    const body = byId('meet-live-tab-body');
    if (body) body.innerHTML = renderActiveTab();
  }
}

function applyBackendUpdate() {
  const aside = document.querySelector('.meet-side');
  if (!aside) return;
  const cards = aside.querySelectorAll('.tf-section-card');
  // Trzecia karta to backend (participants, ai-summary, backend).
  if (cards[2]) cards[2].outerHTML = renderBackendCard();
}

function renderBreadcrumb() {
  const host = byId('meet-live-crumbs');
  if (!host) return;
  const chev = '<svg viewBox="0 0 24 24" stroke="currentColor" stroke-width="2" fill="none" stroke-linecap="round" stroke-linejoin="round"><polyline points="9 18 15 12 9 6"/></svg>';
  const title = displayTitle();
  host.innerHTML = `
    <span class="crumb" data-action="back">${escapeHtml(I18n.t('meeting.live.breadcrumb_root'))}</span>
    <span class="sep">${chev}</span>
    <span class="crumb current">${escapeHtml(title)}</span>
  `;
  host.querySelector('[data-action="back"]')?.addEventListener('click', onBack);
}

function renderHeader() {
  const host = byId('meet-live-header');
  if (!host) return;
  const title = displayTitle();
  const s = state.sessionDetail || {};
  const participantsCount = Math.max(state.participants.size, Number(s.entryCount ? 0 : 0));
  const durationLabel = formatDuration(Date.now() - parseIsoMs(s.startedAt));
  const platform = s.platform || '—';
  const sub = `${I18n.t('meeting.live.subtitle_participants', { n: participantsCount })} · ${durationLabel} · ${escapeHtml(platform)}`;
  // Ikona video (lucide).
  const ico = '<svg viewBox="0 0 24 24" stroke-linecap="round" stroke-linejoin="round"><polygon points="23 7 16 12 23 17 23 7"/><rect x="1" y="5" width="15" height="14" rx="2" ry="2"/></svg>';
  const chip = renderLifecycleChip();
  host.className = 'tf-detail-header';
  host.innerHTML = `
    <div class="big-ico">${ico}</div>
    <div class="d-meta">
      <div class="d-name">
        ${escapeHtml(title)}
        ${chip}
      </div>
      <div class="d-sub">${sub}</div>
    </div>
    <div class="d-actions">
      <tf-button variant="ghost" icon="desktop" id="meet-live-vnc-btn">${escapeHtml(I18n.t('meeting.live.action_btn_screen'))}</tf-button>
      <tf-button variant="ghost" icon="code" id="meet-live-diag-btn">${escapeHtml(I18n.t('meeting.live.action_btn_diag'))}</tf-button>
      <tf-button variant="ghost" icon="share" id="meet-live-dl-btn">${escapeHtml(I18n.t('meeting.live.action_btn_download'))}</tf-button>
      <tf-button variant="danger" icon="logout" id="meet-live-leave-btn">${escapeHtml(I18n.t('meeting.live.action_btn_leave'))}</tf-button>
    </div>
  `;
  byId('meet-live-vnc-btn')?.addEventListener('click', onOpenVnc);
  byId('meet-live-diag-btn')?.addEventListener('click', onOpenDiag);
  byId('meet-live-dl-btn')?.addEventListener('click', onDownloadTranscript);
  byId('meet-live-leave-btn')?.addEventListener('click', onLeave);
}

function renderLifecycleChip() {
  const stage = state.lifecycleStage || 'idle';
  if (stage === 'joined') {
    return `<tf-chip status="ok" dot>${escapeHtml(I18n.t('meeting.live.chip_live'))}</tf-chip>`;
  }
  if (stage === 'failed') {
    return `<tf-chip status="err" dot>${escapeHtml(I18n.t('meeting.status_error'))}</tf-chip>`;
  }
  // Lobby gets its own short chip label and a different colour so the user
  // sees at a glance that the bot is blocked on the host's admit action,
  // not on something the dashboard can fix.
  if (stage === 'lobby_waiting') {
    return `<tf-chip status="warn" dot>${escapeHtml(I18n.t('meeting.live.chip_lobby'))}</tf-chip>`;
  }
  // Any pre-'joined' stage — show the stage label so the user knows why LIVE
  // has not turned on yet (the backend may take ~20s to reach joined).
  const labelKey = `meeting.lifecycle_${stage}`;
  const label = I18n.t(labelKey);
  const resolved = label === labelKey ? I18n.t('meeting.status_joining') : label;
  return `<tf-chip status="warn" dot>${escapeHtml(resolved)}</tf-chip>`;
}

function renderBody() {
  const host = byId('meet-live-body');
  if (!host) return;
  host.innerHTML = `
    <div class="meet-live-grid">
      <section class="meet-main">
        <div class="tf-section-card">
          <tf-tabs variant="underline" value="${state.activeTab}" id="meet-live-tabs">
            <tf-tab id="transcript" icon="chat" count="${state.transcript.length}">${escapeHtml(I18n.t('meeting.live.tab_transcript'))}</tf-tab>
            <tf-tab id="actions" icon="check" count="${state.actionItems.length}">${escapeHtml(I18n.t('meeting.live.tab_actions'))}</tf-tab>
            <tf-tab id="summary" icon="star">${escapeHtml(I18n.t('meeting.live.tab_summary'))}</tf-tab>
          </tf-tabs>
          <div class="meet-tab-body" id="meet-live-tab-body">${renderActiveTab()}</div>
        </div>
      </section>
      <aside class="meet-side">
        ${renderParticipantsCard()}
        ${renderAiSummaryCard()}
        ${renderBackendCard()}
      </aside>
    </div>
  `;
  byId('meet-live-tabs')?.addEventListener('change', (e) => {
    state.activeTab = e.detail?.value || 'transcript';
    const body = byId('meet-live-tab-body');
    if (body) body.innerHTML = renderActiveTab();
    wireActiveTab();
    if (state.activeTab === 'transcript') scrollTranscriptToBottom();
  });
  wireActiveTab();
  if (state.activeTab === 'transcript') scrollTranscriptToBottom();
}

function renderActiveTab() {
  if (state.activeTab === 'actions') return renderActionsTab();
  if (state.activeTab === 'summary') return renderSummaryTab();
  return renderTranscriptTab();
}

function wireActiveTab() {
  const body = byId('meet-live-tab-body');
  if (!body) return;
  if (state.activeTab === 'actions') {
    body.querySelectorAll('[data-group-toggle]').forEach((el) => {
      el.addEventListener('click', () => {
        const key = el.dataset.groupToggle;
        state.groupsCollapsed[key] = !state.groupsCollapsed[key];
        body.innerHTML = renderActionsTab();
        wireActiveTab();
      });
    });
    body.querySelectorAll('[data-item-id]').forEach((toggle) => {
      toggle.addEventListener('change', async (e) => {
        const id = Number(toggle.dataset.itemId);
        const nextStatus = toggle.dataset.nextStatus;
        await setActionItemStatus(id, nextStatus, e);
      });
    });
  }
}

// --- Transcript tab ---------------------------------------------------------

function renderTranscriptTab() {
  if (state.transcript.length === 0) {
    return `<div class="users-empty">${escapeHtml(I18n.t('meeting.live.waiting_for_transcript'))}</div>`;
  }
  const rows = state.transcript.map(renderTranscriptEntry).join('');
  return `<div class="meet-transcript-scroll" id="meet-live-transcript-scroll">${rows}</div>`;
}

function renderTranscriptEntry(entry) {
  const displayName = entry.speakerName || entry.speakerId || 'Unknown';
  const isTemp = !entry.isEnrolled;
  const initials = entryInitials(displayName, isTemp);
  const avatarClass = isTemp ? 'meet-entry-avatar unknown' : 'meet-entry-avatar';
  const time = formatClock(entry.timestampMs);
  const conf = typeof entry.speakerConfidence === 'number'
    ? `<span class="meet-entry-conf">conf ${Math.round(entry.speakerConfidence * 100)}%</span>`
    : '';
  const chip = entry.isEnrolled
    ? `<tf-chip status="ok">${escapeHtml(I18n.t('meeting.live.chip_known'))}</tf-chip>`
    : `<tf-chip status="warn">${escapeHtml(I18n.t('meeting.live.chip_temp'))}</tf-chip>`;
  const avatarContent = isTemp
    ? '<svg viewBox="0 0 24 24" stroke-linecap="round" stroke-linejoin="round" stroke="currentColor" stroke-width="2" fill="none"><circle cx="12" cy="12" r="10"/><path d="M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3"/><line x1="12" y1="17" x2="12.01" y2="17"/></svg>'
    : escapeHtml(initials);
  return `
    <div class="meet-entry">
      <div class="${avatarClass}">${avatarContent}</div>
      <div class="meet-entry-body">
        <div class="meet-entry-head">
          <span class="meet-entry-name">${escapeHtml(displayName)}</span>
          <span class="meet-entry-time">${escapeHtml(time)}</span>
          ${chip}
          ${conf}
        </div>
        <div class="meet-entry-text">${escapeHtml(entry.text)}</div>
      </div>
    </div>
  `;
}

function scrollTranscriptToBottom() {
  // requestAnimationFrame zapewnia ze nowy content jest juz wyrenderowany.
  requestAnimationFrame(() => {
    const host = byId('meet-live-transcript-scroll');
    if (host) host.scrollTop = host.scrollHeight;
  });
}

// --- Actions tab ------------------------------------------------------------

function renderActionsTab() {
  const groups = {
    pending: state.actionItems.filter((a) => a.status === 'pending'),
    done: state.actionItems.filter((a) => a.status === 'done'),
    cancelled: state.actionItems.filter((a) => a.status === 'cancelled'),
  };
  const labels = {
    pending: I18n.t('meeting.live.status_pending'),
    done: I18n.t('meeting.live.status_done'),
    cancelled: I18n.t('meeting.live.status_cancelled'),
  };
  const order = ['pending', 'done', 'cancelled'];
  return order.map((key) => renderActionGroup(key, labels[key], groups[key])).join('');
}

function renderActionGroup(key, label, items) {
  const collapsed = state.groupsCollapsed[key];
  const chev = '<svg class="chevron" viewBox="0 0 24 24" stroke-linecap="round" stroke-linejoin="round" stroke="currentColor" stroke-width="2" fill="none"><polyline points="6 9 12 15 18 9"/></svg>';
  const rows = items.map(renderActionItem).join('');
  const bodyHtml = items.length === 0
    ? `<div class="users-empty" style="padding: 10px 0;">${escapeHtml(I18n.t('meeting.live.no_action_items'))}</div>`
    : rows;
  return `
    <div class="meet-action-group${collapsed ? ' collapsed' : ''}">
      <div class="meet-action-group-head" data-group-toggle="${escapeAttr(key)}">
        ${chev}
        ${escapeHtml(label)} (${items.length})
      </div>
      <div class="meet-action-items">${bodyHtml}</div>
    </div>
  `;
}

function renderActionItem(item) {
  const initials = ownerInitials(item.owner);
  const deadlineChip = deadlineChipHtml(item.deadline);
  // Nastepny status w cyklu pending → done → cancelled → pending.
  const nextMap = { pending: 'done', done: 'cancelled', cancelled: 'pending' };
  const nextStatus = nextMap[item.status] || 'done';
  const checked = item.status === 'done' ? ' checked' : '';
  const statusLabel = I18n.t(`meeting.live.status_${item.status}`) || item.status;
  // Akcje: tf-toggle z zaznaczeniem dla 'done', klikniecie cyklicznie zmienia status.
  return `
    <div class="meet-action-item">
      <div class="ai-avatar">${escapeHtml(initials)}</div>
      <div class="ai-meta">
        <div class="ai-owner">${escapeHtml(item.owner || '—')}</div>
        <div class="ai-task">${escapeHtml(item.task || '')}</div>
      </div>
      ${deadlineChip}
      <span class="meet-action-status-label">${escapeHtml(statusLabel)}</span>
      <tf-toggle data-item-id="${item.id}" data-next-status="${escapeAttr(nextStatus)}"${checked}></tf-toggle>
    </div>
  `;
}

function deadlineChipHtml(deadline) {
  if (!deadline) {
    return `<tf-chip status="err">${escapeHtml(I18n.t('meeting.live.no_deadline'))}</tf-chip>`;
  }
  return `<tf-chip status="ok">${escapeHtml(String(deadline))}</tf-chip>`;
}

async function setActionItemStatus(itemId, newStatus, _event) {
  if (!itemId) {
    // Item bez id (utworzony tylko przez live event) — wymaga reloada z DB
    // zeby dostac id. Robimy full refresh listy.
    try {
      const aiResp = await ApiBinary.one('meetingActionItemsListRequest', { meetingKey: state.meetingKey });
      const items = Array.isArray(aiResp?.items) ? aiResp.items : [];
      state.actionItems = items.map((a) => ({
        id: Number(a.id || 0),
        owner: String(a.owner || ''),
        task: String(a.task || ''),
        deadline: a.deadline || null,
        status: String(a.status || 'pending'),
        createdAt: String(a.createdAt || ''),
        updatedAt: String(a.updatedAt || ''),
      }));
      state.dirty.add('actions');
      state.dirty.add('header');
      renderAll();
    } catch (e) {
      toast(e?.message || I18n.t('meeting.live.update_failed'), 'error');
    }
    return;
  }
  try {
    const resp = await ApiBinary.one('meetingActionItemStatusUpdateRequest', {
      itemId,
      status: newStatus,
    });
    if (resp && resp.success) {
      const item = state.actionItems.find((a) => a.id === itemId);
      if (item) item.status = newStatus;
      toast(I18n.t('meeting.live.update_ok'), 'success');
      state.dirty.add('actions');
      renderAll();
    } else {
      throw new Error(I18n.t('meeting.live.update_failed'));
    }
  } catch (e) {
    toast(e?.message || I18n.t('meeting.live.update_failed'), 'error');
  }
}

// --- Summary tab ------------------------------------------------------------

function renderSummaryTab() {
  const latest = state.summaries[0];
  if (!latest) {
    return `<div class="users-empty">${escapeHtml(I18n.t('meeting.live.summary_pending_backend'))}</div>`;
  }
  const decisions = parseBulletList(latest.decisionsText);
  const decisionsHtml = decisions.length
    ? `<ul class="meet-summary-list">${decisions.map((d) => `<li>${escapeHtml(d)}</li>`).join('')}</ul>`
    : `<div class="meet-summary-text">${escapeHtml(latest.decisionsText || '—')}</div>`;
  const updated = latest.createdAt
    ? `${I18n.t('meeting.live.summary_updated_at')} ${escapeHtml(String(latest.createdAt))}${latest.model ? ' · ' + escapeHtml(latest.model) : ''}`
    : '';
  return `
    <div class="meet-summary-text">${escapeHtml(latest.summaryText || '')}</div>
    <hr class="meet-summary-sep">
    <div class="meet-ai-sub-label">${escapeHtml(I18n.t('meeting.live.section_decisions'))}</div>
    ${decisionsHtml}
    <div class="meet-summary-update">${updated}</div>
  `;
}

function parseBulletList(text) {
  if (!text) return [];
  return String(text)
    .split(/\r?\n/)
    .map((l) => l.trim().replace(/^[-*•]\s*/, ''))
    .filter(Boolean);
}

// --- Sidebar cards ----------------------------------------------------------

function renderParticipantsCard() {
  const list = Array.from(state.participants.values())
    .sort((a, b) => (b.lastSpokenAt || 0) - (a.lastSpokenAt || 0));
  const count = list.length || state.backend.totalParticipants || 0;
  const items = list.length
    ? list.map(renderParticipantRow).join('')
    : `<div class="users-empty">${escapeHtml(I18n.t('meeting.live.no_participants'))}</div>`;
  // id="meet-participants-list" jest potrzebne dla applyParticipantsUpdate —
  // to root surgical-diff. Poszczegolne row maja data-speaker-id by mozna
  // bylo ich zlokalizowac bez query po DOM.
  return `
    <div class="tf-section-card">
      <h3>${escapeHtml(I18n.t('meeting.live.sidebar_participants'))} <span class="counter">(${count})</span></h3>
      <div id="meet-participants-list" class="meet-participants-list">${items}</div>
    </div>
  `;
}

// Mapowanie 8-klasowego AffectNet HSEmotion → emoji + tło badge'a. Kolejność
// i nazwy zgodne z `EMOTION_LABELS` w `tentaflow-core/src/vision/hsemotion.rs`.
const EMOTION_VISUAL = {
  Happiness: { emoji: '😊', bg: '#10b981' },
  Neutral:   { emoji: '😐', bg: '#6b7280' },
  Sadness:   { emoji: '😢', bg: '#3b82f6' },
  Surprise:  { emoji: '😲', bg: '#f59e0b' },
  Fear:      { emoji: '😨', bg: '#8b5cf6' },
  Anger:     { emoji: '😠', bg: '#ef4444' },
  Disgust:   { emoji: '🤢', bg: '#84cc16' },
  Contempt:  { emoji: '😒', bg: '#a16207' },
};

// Atrybuty starsze niż 5 s traktujemy jako "stale" — stale visual hint
// (przyciemnione) bez całkowitego ukrywania, bo throttle vision pipeline
// to 2 s i naturalnie zostawia okno bez świeżego sygnału.
const ATTRIBUTES_STALE_AFTER_MS = 5_000;

function renderParticipantRow(p) {
  const speakingNow = p.status === 'active_now' && (Date.now() - (p.lastSpokenAt || 0) < 5000);
  const sub = speakingNow
    ? `${p.isEnrolled ? 'Enrolled' : I18n.t('meeting.live.not_enrolled')} · ${escapeHtml(I18n.t('meeting.live.participant_speaking_now'))}`
    : p.lastSpokenAt
      ? `${p.isEnrolled ? 'Enrolled' : I18n.t('meeting.live.not_enrolled')} · ${escapeHtml(I18n.t('meeting.live.participant_spoke_ago', { n: Math.max(1, Math.floor((Date.now() - p.lastSpokenAt) / 1000)) }))}`
      : (p.isEnrolled ? 'Enrolled' : I18n.t('meeting.live.not_enrolled'));
  const initials = ownerInitials(p.speakerName || p.speakerId);
  const avatarClass = p.isEnrolled ? 'p-avatar' : 'p-avatar unknown';
  const rowClass = speakingNow ? 'meet-participant speaking' : 'meet-participant';
  const micSvg = '<svg class="p-mic" viewBox="0 0 24 24" stroke-linecap="round" stroke-linejoin="round"><path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><line x1="12" y1="19" x2="12" y2="23"/></svg>';

  // Vision pipeline overlay: badge emocji nad awatarem + chip wiek/płeć
  // pod nazwą. Renderujemy tylko gdy mamy aktualne dane — brak pól =
  // tradycyjny widok bez badge'a.
  const stale = p.attributesUpdatedAt
    ? (Date.now() - p.attributesUpdatedAt) > ATTRIBUTES_STALE_AFTER_MS
    : false;
  const staleClass = stale ? ' stale' : '';
  let emotionBadge = '';
  if (p.emotion && EMOTION_VISUAL[p.emotion]) {
    const v = EMOTION_VISUAL[p.emotion];
    const conf = typeof p.emotionConfidence === 'number'
      ? `${(p.emotionConfidence * 100).toFixed(0)}%`
      : '';
    const title = `${p.emotion}${conf ? ' ' + conf : ''}`;
    emotionBadge = `<span class="p-emotion-badge${staleClass}" style="background:${v.bg}" title="${escapeAttr(title)}">${v.emoji}</span>`;
  }
  let attrsLine = '';
  if (p.age != null || p.genderMaleProb != null) {
    const parts = [];
    if (typeof p.age === 'number') {
      parts.push(`<span class="p-attr-age">~${Math.round(p.age)} ${I18n.t('meeting.live.attr_years_short')}</span>`);
    }
    if (typeof p.genderMaleProb === 'number') {
      const isMale = p.genderMaleProb > 0.5;
      const symbol = isMale ? '♂' : '♀';
      parts.push(`<span class="p-attr-gender">${symbol}</span>`);
    }
    attrsLine = `<div class="p-attrs${staleClass}">${parts.join('')}</div>`;
  }

  return `
    <div class="${rowClass}" data-speaker-id="${escapeAttr(p.speakerId || '')}">
      <div class="p-avatar-wrap">
        <div class="${avatarClass}">${escapeHtml(initials)}</div>
        ${emotionBadge}
      </div>
      <div class="p-meta">
        <div class="p-name">${escapeHtml(p.speakerName || p.speakerId || '?')}</div>
        <div class="p-sub">${sub}</div>
        ${attrsLine}
      </div>
      ${micSvg}
    </div>
  `;
}

function renderAiSummaryCard() {
  const latest = state.summaries[0];
  const decisions = latest ? latest.decisionsText : '';
  const decisionsHtml = decisions
    ? `<div class="meet-ai-sub-text">${escapeHtml(decisions)}</div>`
    : `<div class="meet-ai-sub-text" style="color: var(--text-3);">${escapeHtml(I18n.t('meeting.live.summary_pending_backend'))}</div>`;

  const pending = state.actionItems.filter((a) => a.status === 'pending');
  const actionsHtml = pending.length
    ? pending.slice(0, 5).map((a) => {
        const chip = deadlineChipHtml(a.deadline);
        return `
          <div class="meet-ai-action">
            <span class="owner">${escapeHtml(a.owner || '—')}</span>
            <span class="arrow">→</span>
            <span class="task">${escapeHtml(a.task || '')}</span>
            ${chip}
          </div>
        `;
      }).join('')
    : `<div class="users-empty" style="padding: 8px 0;">${escapeHtml(I18n.t('meeting.live.no_action_items'))}</div>`;

  return `
    <div class="tf-section-card">
      <h3>${escapeHtml(I18n.t('meeting.live.sidebar_ai_summary'))}</h3>
      <div class="meet-ai-sub">
        <div class="meet-ai-sub-label">${escapeHtml(I18n.t('meeting.live.section_decisions'))}</div>
        ${decisionsHtml}
      </div>
      <div class="meet-ai-sub">
        <div class="meet-ai-sub-label">${escapeHtml(I18n.t('meeting.live.section_action_items'))} (${pending.length})</div>
        ${actionsHtml}
      </div>
    </div>
  `;
}

function renderBackendCard() {
  const b = state.backend;
  const latencyAvg = state.latencyHistory.length
    ? Math.round(state.latencyHistory.reduce((a, v) => a + v, 0) / state.latencyHistory.length)
    : (b.streamingLatencyMs != null ? Number(b.streamingLatencyMs) : null);
  const latencyClass = latencyAvg == null
    ? ''
    : latencyAvg < 300 ? 'ok'
      : latencyAvg < 1000 ? 'warn'
      : 'err';
  const latencyText = latencyAvg == null ? '—' : `${latencyAvg} ms`;
  const enrolled = b.enrolledSpeakers != null ? b.enrolledSpeakers : '—';
  const total = b.totalParticipants != null ? b.totalParticipants : '—';
  return `
    <div class="tf-section-card">
      <h3>${escapeHtml(I18n.t('meeting.live.sidebar_backend'))}</h3>
      <div class="meet-kv">
        <div class="meet-kv-row"><span class="k">${escapeHtml(I18n.t('meeting.live.backend_stt_label'))}</span><span class="v">${escapeHtml(b.sttModel || '—')}</span></div>
        <div class="meet-kv-row"><span class="k">${escapeHtml(I18n.t('meeting.live.backend_diarization_label'))}</span><span class="v">${escapeHtml(b.diarizationModel || '—')}</span></div>
        <div class="meet-kv-row"><span class="k">${escapeHtml(I18n.t('meeting.live.backend_summary_label'))}</span><span class="v">${escapeHtml(b.summarizationModel || '—')}</span></div>
        <div class="meet-kv-row"><span class="k">${escapeHtml(I18n.t('meeting.live.backend_latency_label'))}</span><span class="v ${latencyClass}">${escapeHtml(latencyText)}</span></div>
        <div class="meet-kv-row"><span class="k">${escapeHtml(I18n.t('meeting.live.backend_enrolled_label'))}</span><span class="v">${escapeHtml(String(enrolled))}/${escapeHtml(String(total))}</span></div>
      </div>
    </div>
  `;
}

function footerHtml() {
  const ageSec = state.lastTranscriptAt
    ? Math.max(0, Math.floor((Date.now() - state.lastTranscriptAt) / 1000))
    : 0;
  const latencyAvg = state.latencyHistory.length
    ? Math.round(state.latencyHistory.reduce((a, v) => a + v, 0) / state.latencyHistory.length)
    : (state.backend.streamingLatencyMs != null ? Number(state.backend.streamingLatencyMs) : 0);
  const left = I18n.t('meeting.live.footer_listening', { age: ageSec, latency: latencyAvg });
  const b = state.backend;
  const right = [b.sttModel, b.diarizationModel].filter(Boolean).join(' · ');
  return `
    <span class="live-dot">${escapeHtml(left)}</span>
    <span class="mono">${escapeHtml(right)}</span>
  `;
}

// --- Action handlers --------------------------------------------------------

async function onOpenVnc() {
  const s = state.sessionDetail;
  if (!s || !Number.isFinite(s.sessionId)) {
    toast(I18n.t('meeting.live.vnc_unavailable'), 'warn');
    return;
  }

  const win = document.createElement('tf-window');
  win.setAttribute('title', I18n.t('meeting.live.action_btn_screen'));
  // Use the full default button set so users can minimize / maximize; mark the
  // window draggable and resizable so noVNC scales to whatever size the user
  // settles on (scaleViewport=true preserves the captured aspect ratio by
  // letterboxing inside the bounds).
  win.setAttribute('draggable', '');
  win.setAttribute('resizable', '');
  win.setAttribute('width', '1024');
  win.setAttribute('height', '640');
  win.setAttribute('min-width', '480');
  win.setAttribute('min-height', '320');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  const body = document.createElement('div');
  body.slot = 'body';
  body.className = 'meet-vnc-body';
  const screen = document.createElement('div');
  screen.className = 'meet-vnc-screen';
  body.appendChild(screen);
  win.appendChild(body);
  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  document.body.append(backdrop, win);

  let rfb = null;
  let transport = null;
  let disposed = false;

  const cleanup = () => {
    if (disposed) return;
    disposed = true;
    try { rfb?.disconnect(); } catch (_) {}
    try { transport?.close(); } catch (_) {}
    if (win.parentNode) win.remove();
    if (backdrop.parentNode) backdrop.remove();
  };
  win.addEventListener('action', (e) => {
    if (e.detail?.action === 'close') cleanup();
  });

  const showStatusToast = ({ status, error }) => {
    if (disposed) return;
    if (status === 'ok') return;
    const keyByStatus = {
      not_found: 'meeting.live.vnc_error_not_found',
      forbidden: 'meeting.live.vnc_error_forbidden',
      no_port: 'meeting.live.vnc_error_no_port',
      remote_node: 'meeting.live.vnc_error_remote_node',
    };
    const key = keyByStatus[status] || 'meeting.live.vnc_error_failed';
    const msg = key === 'meeting.live.vnc_error_failed'
      ? I18n.t(key, { message: error || '' })
      : I18n.t(key);
    toast(msg, 'error');
    cleanup();
  };

  try {
    const [{ default: RFB }, { VncApiBinaryTransport }] = await Promise.all([
      import('/js/vendor/novnc/core/rfb.js'),
      import('/js/modules/meeting/vnc-transport.js'),
    ]);
    transport = new VncApiBinaryTransport(Number(s.sessionId));
    rfb = new RFB(screen, transport, { shared: true });
    rfb.scaleViewport = true;
    rfb.resizeSession = false;
    rfb.addEventListener('disconnect', (ev) => {
      if (disposed) return;
      if (!ev?.detail?.clean) {
        toast(I18n.t('meeting.live.vnc_disconnected'), 'warn');
      }
      cleanup();
    });
    await transport.start(showStatusToast);
  } catch (err) {
    console.error('[meeting-live] vnc open failed:', err);
    toast(I18n.t('meeting.live.vnc_error_failed', { message: err?.message || '' }), 'error');
    cleanup();
  }
}

async function onOpenDiag() {
  const s = state.sessionDetail;
  if (!s || !Number.isFinite(s.sessionId)) {
    toast(I18n.t('meeting.live.vnc_unavailable'), 'warn');
    return;
  }
  const sessionId = Number(s.sessionId);

  const win = document.createElement('tf-window');
  win.setAttribute('title', I18n.t('meeting.live.diag_title'));
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '860');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');

  const body = document.createElement('div');
  body.slot = 'body';
  body.className = 'diag-window-body';

  const actions = document.createElement('div');
  actions.className = 'diag-actions';
  const btnShot = document.createElement('tf-button');
  btnShot.setAttribute('variant', 'primary');
  btnShot.setAttribute('icon', 'camera');
  btnShot.textContent = I18n.t('meeting.live.diag_screenshot_viewport');
  const btnShotFull = document.createElement('tf-button');
  btnShotFull.setAttribute('variant', 'ghost');
  btnShotFull.setAttribute('icon', 'camera');
  btnShotFull.textContent = I18n.t('meeting.live.diag_screenshot_full');
  const btnDom = document.createElement('tf-button');
  btnDom.setAttribute('variant', 'ghost');
  btnDom.setAttribute('icon', 'code');
  btnDom.textContent = I18n.t('meeting.live.diag_dump_dom');
  actions.append(btnShot, btnShotFull, btnDom);

  const result = document.createElement('div');
  result.className = 'diag-result';
  const empty = document.createElement('div');
  empty.className = 'diag-empty';
  empty.textContent = I18n.t('meeting.live.diag_empty');
  result.appendChild(empty);

  body.append(actions, result);
  win.appendChild(body);
  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  document.body.append(backdrop, win);

  // Track blob URLs created inside this window so we can revoke them on close.
  const objectUrls = [];
  const trackUrl = (url) => { objectUrls.push(url); return url; };

  let disposed = false;
  const cleanup = () => {
    if (disposed) return;
    disposed = true;
    for (const u of objectUrls) { try { URL.revokeObjectURL(u); } catch (_) {} }
    objectUrls.length = 0;
    if (win.parentNode) win.remove();
    if (backdrop.parentNode) backdrop.remove();
  };
  win.addEventListener('action', (e) => {
    if (e.detail?.action === 'close') cleanup();
  });

  const setBusy = (busy) => {
    for (const b of [btnShot, btnShotFull, btnDom]) {
      if (busy) b.setAttribute('disabled', '');
      else b.removeAttribute('disabled');
    }
  };

  const showLoading = () => {
    result.innerHTML = '';
    const load = document.createElement('div');
    load.className = 'diag-empty';
    load.textContent = I18n.t('meeting.live.diag_loading');
    result.appendChild(load);
  };

  const handleError = ({ status, error }) => {
    const keyByStatus = {
      not_found: 'meeting.live.diag_error_not_found',
      forbidden: 'meeting.live.diag_error_forbidden',
      remote_node: 'meeting.live.diag_error_remote_node',
    };
    const key = keyByStatus[status] || 'meeting.live.diag_error_failed';
    const msg = key === 'meeting.live.diag_error_failed'
      ? I18n.t(key, { message: error || status || '' })
      : I18n.t(key);
    toast(msg, 'error');
    result.innerHTML = '';
    const fail = document.createElement('div');
    fail.className = 'diag-empty';
    fail.textContent = msg;
    result.appendChild(fail);
  };

  const renderScreenshot = (pngBytes) => {
    // Reset panel (shared for screenshot + DOM) and revoke previous URLs.
    for (const u of objectUrls) { try { URL.revokeObjectURL(u); } catch (_) {} }
    objectUrls.length = 0;
    result.innerHTML = '';
    const blob = new Blob([pngBytes], { type: 'image/png' });
    const url = trackUrl(URL.createObjectURL(blob));
    const toolbar = document.createElement('div');
    toolbar.className = 'diag-actions';
    const dl = document.createElement('tf-button');
    dl.setAttribute('variant', 'ghost');
    dl.setAttribute('icon', 'share');
    dl.textContent = I18n.t('meeting.live.diag_download');
    dl.addEventListener('click', () => {
      const a = document.createElement('a');
      a.href = url;
      a.download = `meeting-${sessionId}-screenshot-${Date.now()}.png`;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
    });
    toolbar.appendChild(dl);
    const img = document.createElement('img');
    img.src = url;
    img.alt = 'screenshot';
    result.append(toolbar, img);
  };

  const renderDom = (html) => {
    for (const u of objectUrls) { try { URL.revokeObjectURL(u); } catch (_) {} }
    objectUrls.length = 0;
    result.innerHTML = '';
    const toolbar = document.createElement('div');
    toolbar.className = 'diag-actions';
    const copyBtn = document.createElement('tf-button');
    copyBtn.setAttribute('variant', 'ghost');
    copyBtn.setAttribute('icon', 'copy');
    copyBtn.textContent = I18n.t('meeting.live.diag_copy');
    copyBtn.addEventListener('click', async () => {
      try {
        await navigator.clipboard.writeText(html);
        toast(I18n.t('meeting.live.diag_copy_ok'), 'success');
      } catch (err) {
        toast(err?.message || 'Clipboard error', 'error');
      }
    });
    const dlBtn = document.createElement('tf-button');
    dlBtn.setAttribute('variant', 'ghost');
    dlBtn.setAttribute('icon', 'share');
    dlBtn.textContent = I18n.t('meeting.live.diag_download');
    dlBtn.addEventListener('click', () => {
      const blob = new Blob([html], { type: 'text/html;charset=utf-8' });
      const url = trackUrl(URL.createObjectURL(blob));
      const a = document.createElement('a');
      a.href = url;
      a.download = `meeting-${sessionId}-dom-${Date.now()}.html`;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
    });
    toolbar.append(copyBtn, dlBtn);
    const pre = document.createElement('pre');
    // Large dumps: show as text only; parsing HTML into DOM would be slow and unsafe.
    pre.textContent = html;
    result.append(toolbar, pre);
  };

  const runCapture = async (kind, fullPage) => {
    if (disposed) return;
    setBusy(true);
    showLoading();
    try {
      const resp = await ApiBinary.one('browserCaptureRequest', {
        sessionId,
        kind,
        fullPage: !!fullPage,
      });
      if (disposed) return;
      if (resp?.status !== 'ok') {
        handleError({ status: resp?.status, error: resp?.error });
        return;
      }
      if (kind === 'screenshot') {
        const png = resp.png instanceof Uint8Array ? resp.png : new Uint8Array(resp.png || []);
        renderScreenshot(png);
      } else {
        renderDom(String(resp.html ?? ''));
      }
    } catch (err) {
      if (disposed) return;
      handleError({ status: 'failed', error: err?.message || '' });
    } finally {
      if (!disposed) setBusy(false);
    }
  };

  btnShot.addEventListener('click', () => runCapture('screenshot', false));
  btnShotFull.addEventListener('click', () => runCapture('screenshot', true));
  btnDom.addEventListener('click', () => runCapture('dom', false));
}

async function onDownloadTranscript() {
  try {
    const resp = await ApiBinary.one('meetingTranscriptExportRequest', {
      meetingKey: state.meetingKey,
    });
    const content = String(resp?.content ?? '');
    if (!content) {
      toast(I18n.t('meeting.live.export_empty'), 'warn');
      return;
    }
    const blob = new Blob([content], { type: 'text/plain;charset=utf-8' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    const safeName = String(displayTitle()).replace(/[^a-z0-9_-]+/gi, '_').slice(0, 80) || 'meeting';
    const date = new Date().toISOString().slice(0, 10);
    a.download = `${safeName}-${date}.txt`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  } catch (e) {
    toast(e?.message || I18n.t('meeting.live.export_failed'), 'error');
  }
}

async function onLeave() {
  if (!state.sessionDetail?.sessionId) return;
  const confirmed = await confirmDialog(
    I18n.t('meeting.live.leave_confirm_title'),
    I18n.t('meeting.live.leave_confirm_body'),
    I18n.t('meeting.live.leave_confirm_yes'),
    I18n.t('meeting.live.leave_confirm_no'),
  );
  if (!confirmed) return;
  try {
    await ApiBinary.one('meetingSessionLeaveRequest', { sessionId: state.sessionDetail.sessionId });
    toast(I18n.t('meeting.live.leave_ok'), 'success');
    // Powrot do listy spotkan.
    const { Router } = await import('/js/router.js');
    Router.navigate('meeting');
  } catch (e) {
    toast(e?.message || I18n.t('meeting.live.leave_failed'), 'error');
  }
}

async function onBack() {
  const { Router } = await import('/js/router.js');
  Router.navigate('meeting');
}

// --- Modal helper -----------------------------------------------------------

function confirmDialog(title, body, yesLabel, noLabel) {
  return new Promise((resolve) => {
    const win = document.createElement('tf-window');
    win.setAttribute('title', title);
    win.setAttribute('buttons', 'close');
    win.setAttribute('width', '460');
    win.setAttribute('initial-x', 'center');
    win.setAttribute('initial-y', 'center');
    const bodyEl = document.createElement('div');
    bodyEl.slot = 'body';
    bodyEl.innerHTML = `<p style="margin:0; color: var(--text-2); font-size: 13.5px; line-height: 1.55;">${escapeHtml(body)}</p>`;
    win.appendChild(bodyEl);
    const foot = document.createElement('div');
    foot.slot = 'footer';
    foot.innerHTML = `
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(noLabel)}</tf-button>
      <tf-button variant="danger" data-action="confirm">${escapeHtml(yesLabel)}</tf-button>
    `;
    win.appendChild(foot);
    const backdrop = document.createElement('div');
    backdrop.className = 'tf-window-backdrop';
    document.body.append(backdrop, win);
    const cleanup = (result) => {
      win.remove();
      backdrop.remove();
      resolve(result);
    };
    win.addEventListener('action', (e) => {
      const a = e.detail?.action;
      if (a === 'cancel' || a === 'close') cleanup(false);
      else if (a === 'confirm') cleanup(true);
    });
  });
}

// --- Utils ------------------------------------------------------------------

function displayTitle() {
  const s = state.sessionDetail;
  return (s && (s.title || s.meetingKey)) || state.meetingKey || '—';
}

function formatDuration(ms) {
  if (!Number.isFinite(ms) || ms <= 0) return '—';
  const sec = Math.floor(ms / 1000);
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  return `${m} min ${s} s`;
}

function formatClock(timestampMs) {
  if (!timestampMs) return '';
  try {
    return new Date(timestampMs).toLocaleTimeString(I18n.getLanguage ? I18n.getLanguage() : 'pl-PL', { hour12: false });
  } catch (_) {
    return '';
  }
}

function entryInitials(name, isTemp) {
  if (isTemp) return '?';
  return ownerInitials(name);
}

function ownerInitials(name) {
  if (!name) return '?';
  const str = String(name).trim();
  if (!str) return '?';
  if (str.startsWith('SPEAKER_')) return '?';
  const parts = str.split(/\s+/);
  if (parts.length >= 2) return (parts[0][0] + parts[1][0]).toUpperCase();
  return str.slice(0, 2).toUpperCase();
}

export default MeetingLiveScreen;
