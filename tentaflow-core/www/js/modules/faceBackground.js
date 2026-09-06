// =============================================================================
// File: js/modules/faceBackground.js
// Description: Thin overlay manager over the <tf-face> web component.
//              show()/hide()/transitionOut()/shakeHead() drive the fullscreen
//              login overlay; embed() mounts a <tf-face> inside a given host.
// Example: FaceBackground.show(); ... FaceBackground.hide();
//          const handle = FaceBackground.embed(node); handle.setMode('idle');
// =============================================================================

// Ensure tf-face is registered before use
import '/js/components/tf-face.js';

const CONTAINER_ID = 'face-bg-root';

// Singleton fullscreen <tf-face> instance (only one login overlay at a time)
let fullscreenFace = null;

// Active embed element reference for cleanup on show()
let activeEmbedEl = null;

// -- public API ---------------------------------------------------------------

export const FaceBackground = {
  show() {
    if (document.getElementById(CONTAINER_ID)) return;

    // Destroy active embed if any
    if (activeEmbedEl) {
      activeEmbedEl.remove();
      activeEmbedEl = null;
    }

    const container = document.createElement('div');
    container.id = CONTAINER_ID;
    container.className = 'face-bg';

    const face = document.createElement('tf-face');
    face.setAttribute('fullscreen', '');
    face.setAttribute('mode', 'idle');
    face.setAttribute('track', 'pointer gyro');
    face.style.width = '100%';
    face.style.height = '100%';
    face.style.display = 'block';
    container.appendChild(face);

    document.body.appendChild(container);
    document.body.classList.add('has-face-bg');
    fullscreenFace = face;

    requestAnimationFrame(() => { container.classList.add('is-visible'); });
  },

  transitionOut(opts) {
    const onMidpoint = (opts && opts.onMidpoint) || (() => {});
    const onComplete = (opts && opts.onComplete) || (() => {});

    if (!fullscreenFace) { onMidpoint(); onComplete(); return; }

    // UI emerges from the eye: per-frame CSS variables on #app-root scale the
    // freshly mounted UI in sync with the face zoom.
    const UI_OFFSET_X = 15;
    const UI_OFFSET_Y = 25;
    let uiRoot = null;

    fullscreenFace.transitionOut({
      onMidpoint,
      onProgress(uiScale) {
        if (!uiRoot) uiRoot = document.getElementById('app-root');
        if (!uiRoot) return;
        if (!uiRoot.classList.contains('is-emerging')) uiRoot.classList.add('is-emerging');
        uiRoot.style.setProperty('--tf-ui-scale', uiScale.toFixed(4));
        const uiOpacity = Math.min(1, uiScale * 5);
        uiRoot.style.setProperty('--tf-ui-opacity', uiOpacity.toFixed(3));
        const offFactor = Math.max(0, 1 - uiScale);
        uiRoot.style.setProperty('--tf-ui-offset-x', `${(UI_OFFSET_X * offFactor).toFixed(1)}px`);
        uiRoot.style.setProperty('--tf-ui-offset-y', `${(UI_OFFSET_Y * offFactor).toFixed(1)}px`);
      },
      onComplete() {
        if (uiRoot) {
          uiRoot.classList.remove('is-emerging');
          uiRoot.style.removeProperty('--tf-ui-scale');
          uiRoot.style.removeProperty('--tf-ui-opacity');
          uiRoot.style.removeProperty('--tf-ui-offset-x');
          uiRoot.style.removeProperty('--tf-ui-offset-y');
        }
        FaceBackground.hide();
        try { onComplete(); } catch (e) { console.error('[faceBg] onComplete error:', e); }
      },
    });
  },

  shakeHead() {
    if (!fullscreenFace) return;
    fullscreenFace.shakeHead();
  },

  hide() {
    const container = document.getElementById(CONTAINER_ID);
    if (!container) return;

    container.classList.remove('is-visible');
    document.body.classList.remove('has-face-bg');
    setTimeout(() => {
      container.remove();
      fullscreenFace = null;
    }, 650);
  },

  /**
   * Embed mode — creates a <tf-face> element inside the given container.
   * Returns a handle compatible with the old embed() API.
   */
  embed(container) {
    if (!container || !(container instanceof HTMLElement)) {
      throw new Error('FaceBackground.embed: container must be HTMLElement');
    }
    // Destroy previous embed if different container
    if (activeEmbedEl && activeEmbedEl.parentNode) {
      if (activeEmbedEl.parentNode === container) {
        return activeEmbedEl._handle;
      }
      activeEmbedEl.remove();
      activeEmbedEl = null;
    }
    if (document.getElementById(CONTAINER_ID)) {
      FaceBackground.hide();
    }

    const face = document.createElement('tf-face');
    face.setAttribute('mode', 'idle');
    const w = container.clientWidth || 360;
    const h = container.clientHeight || 360;
    face.setAttribute('size', String(Math.min(w, h)));
    face.style.width = '100%';
    face.style.height = '100%';
    face.style.display = 'block';

    container.classList.add('face-embed-host');
    container.appendChild(face);
    activeEmbedEl = face;

    // ResizeObserver keeps the size attribute in sync with host
    let resizeObs = null;
    if (typeof ResizeObserver !== 'undefined') {
      resizeObs = new ResizeObserver(() => {
        const cw = container.clientWidth || 360;
        const ch = container.clientHeight || 360;
        face.setAttribute('size', String(Math.min(cw, ch)));
      });
      resizeObs.observe(container);
    }

    const handle = {
      setMode(mode) {
        if (mode !== 'idle' && mode !== 'listen' && mode !== 'think' && mode !== 'speak') {
          console.warn('[faceBg] setMode: unknown mode', mode);
          return;
        }
        face.setAttribute('mode', mode);
        container.style.setProperty('--ui-mode', mode);
        container.dataset.uiMode = mode;
      },
      setSpeechAmplitude(rms, articulation) {
        face.setSpeechAmplitude(rms, articulation);
      },
      setListenAmplitude(rms) {
        const v = Number(rms);
        if (!Number.isFinite(v)) return;
        const clamped = Math.max(0, Math.min(1, v));
        container.style.setProperty('--listen-amp', clamped.toFixed(3));
      },
      destroy() {
        if (resizeObs) { resizeObs.disconnect(); resizeObs = null; }
        if (face.parentNode === container) container.removeChild(face);
        container.classList.remove('face-embed-host');
        container.style.removeProperty('--ui-mode');
        container.style.removeProperty('--listen-amp');
        delete container.dataset.uiMode;
        if (activeEmbedEl === face) activeEmbedEl = null;
      },
    };

    face._handle = handle;
    return handle;
  },
};

export default FaceBackground;
