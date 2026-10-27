// ===== File: audio.js — Audio mode demo: drives the real tf-face, the live transcript and the status line =====
(function () {
  'use strict';

  var stage = document.querySelector('.face-stage');
  if (!stage) return;
  var face = stage.querySelector('tf-face');
  var label = document.getElementById('audioLabel');
  var list = document.getElementById('trList');
  var reduce = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  var LABELS = { listen: 'Słucham…', think: 'Myślę…', speak: 'AI mówi…' };

  // Two scripted turns, alternated; sentences are separated by "|".
  var TURNS = [
    {
      user: 'Ustaw mi na jutro przypomnienie o spotkaniu z zespołem infrastruktury o czternastej.',
      bot: 'Gotowe, przypomnienie jest ustawione na jutro na czternastą.|Spotkanie z zespołem infrastruktury trwa godzinę, więc zakończy się o piętnastej.|Chcesz, żebym dodał do niego link do kanału z notatkami?'
    },
    {
      user: 'Tak, dodaj ten link i przypomnij mi jeszcze o przygotowaniu agendy.',
      bot: 'Dodałem link do kanału z notatkami.|Agendę przypomnę ci o trzynastej, godzinę przed spotkaniem.'
    }
  ];

  var token = 0;       // incremented on every restart so stale timers stop themselves
  var turnIndex = 0;
  var state = stage.getAttribute('data-state') || 'listen';
  var speaking = null; // { start: ms timestamp of the current word, len: its duration }
var added = [];      // transcript items created by the current loop pass (removed again when a single state is looped)

  // ----- Wave bars -----
  var wave = document.getElementById('wave');
  if (wave) {
    for (var i = 0; i < 34; i++) {
      var b = document.createElement('i');
      b.className = 'bar';
      b.style.animationDelay = (-Math.random() * 0.9).toFixed(2) + 's';
      b.style.animationDuration = (0.7 + Math.random() * 0.5).toFixed(2) + 's';
      wave.appendChild(b);
    }
  }

  function setState(s) {
    state = s;
    stage.setAttribute('data-state', s);
    if (face) face.setAttribute('mode', s);
    if (label) label.textContent = LABELS[s];
    document.querySelectorAll('[data-audio-state]').forEach(function (btn) {
      btn.classList.toggle('active', btn.getAttribute('data-audio-state') === s);
    });
  }

  // ----- Transcript helpers -----
  function now() { var d = new Date(); return ('0' + d.getHours()).slice(-2) + ':' + ('0' + d.getMinutes()).slice(-2); }
  function esc(t) { return t.replace(/&/g, '&amp;').replace(/</g, '&lt;'); }
  function addItem(who, html, cls) {
    var el = document.createElement('div');
    el.className = 'tr-item ' + who + ' ' + (cls || '');
    el.innerHTML = '<div class="who">' + (who === 'user' ? 'Ty' : 'Asystent') + '<span class="t">' + now() + '</span></div>' + html;
    list.appendChild(el);
    added.push(el);
    list.scrollTop = list.scrollHeight;
    return el;
  }
  function clearNow() { list.querySelectorAll('.tr-item.now').forEach(function (e) { e.classList.remove('now'); }); }

  function wait(ms, my) {
    return new Promise(function (res, rej) {
      setTimeout(function () { my === token ? res() : rej(new Error('cancelled')); }, reduce ? Math.min(ms, 30) : ms);
    });
  }

  function listen(turn, my) {
    setState('listen');
    clearNow();
    var item = addItem('user', '<div class="text partial"></div>', 'now');
    var text = item.querySelector('.text');
    var words = turn.user.split(' ');
    var p = Promise.resolve();
    words.forEach(function (w, i) {
      p = p.then(function () {
        text.textContent = words.slice(0, i + 1).join(' ');
        return wait(210 + Math.random() * 120, my);
      });
    });
    return p.then(function () {
      text.classList.remove('partial');
      return wait(350, my);
    });
  }

  function think(my) {
    setState('think');
    clearNow();
    var item = addItem('bot', '<div class="think-note"><span class="typing-dots"><span></span><span></span><span></span></span>Przygotowuję odpowiedź</div>', 'now');
    if (window.ChatSession) {
      var k = 0;
      var tick = setInterval(function () {
        if (my !== token || k++ > 2) return clearInterval(tick);
        window.ChatSession.push(40 + Math.random() * 14);
      }, 500);
    }
    return wait(1500, my).then(function () { return item; });
  }

  function speak(turn, item, my) {
    setState('speak');
    var sentences = turn.bot.split('|');
    var html = sentences.map(function (s, si) {
      return '<span class="sent" data-s="' + si + '">' + s.split(' ').map(function (w) { return '<span class="w">' + esc(w) + '</span>'; }).join(' ') + '</span>';
    }).join(' ');
    item.innerHTML = '<div class="who">Asystent<span class="t">' + now() + '</span></div><div class="text">' + html + '</div>';
    var ws = item.querySelectorAll('.w');
    var p = Promise.resolve();
    ws.forEach(function (w, i) {
      p = p.then(function () {
        ws.forEach(function (x, j) { x.classList.toggle('said', j < i); x.classList.toggle('cur', j === i); });
        item.querySelectorAll('.sent').forEach(function (s) { s.classList.toggle('cur', s.contains(w)); });
        var len = 150 + w.textContent.length * 38;
        speaking = { start: performance.now(), len: len };
        return wait(len, my);
      });
    });
    return p.then(function () {
      speaking = null;
      ws.forEach(function (x) { x.classList.add('said'); x.classList.remove('cur'); });
      item.querySelectorAll('.sent').forEach(function (s) { s.classList.remove('cur'); });
      return wait(700, my);
    });
  }

  // mode: 'auto' loops listen -> think -> speak; otherwise it loops a single state.
  function start(mode) {
    token++;
    var my = token;
    speaking = null;
    var loop = function () {
      added = [];
      var turn = TURNS[turnIndex % TURNS.length];
      var chain;
      if (mode === 'auto') chain = listen(turn, my).then(function () { return think(my); }).then(function (it) { return speak(turn, it, my); });
      else if (mode === 'listen') chain = listen(turn, my);
      else if (mode === 'think') chain = think(my).then(function () { return wait(1200, my); });
      else chain = think(my).then(function (it) { return speak(turn, it, my); });
      return chain.then(function () {
        if (mode === 'auto') turnIndex++;
        else added.splice(0).forEach(function (el) { el.remove(); });
        return loop();
      });
    };
    loop().catch(function () { /* cancelled */ });
  }

  document.querySelectorAll('[data-audio-state]').forEach(function (btn) {
    btn.addEventListener('click', function () { start(btn.getAttribute('data-audio-state')); });
  });

  // ----- Face speech driver: amplitude follows word boundaries so the lips pause between words -----
  var noise = { round: 0.4, wide: 0.3, fric: 0.2, tr: 0.4, tw: 0.3, tf: 0.2, next: 0 };
  function frame(t) {
    if (face && typeof face.setSpeechAmplitude === 'function') {
      if (state === 'speak' && speaking) {
        var ph = Math.min(1, (performance.now() - speaking.start) / speaking.len);
        var env = Math.sin(Math.PI * ph);
        var syl = 0.55 + 0.45 * Math.abs(Math.sin(t / 1000 * 11));
        var amp = Math.min(1, env * syl * (0.75 + Math.random() * 0.25));
        if (t > noise.next) { noise.tr = Math.random(); noise.tw = Math.random(); noise.tf = Math.random() * 0.6; noise.next = t + 140; }
        noise.round += (noise.tr - noise.round) * 0.2;
        noise.wide += (noise.tw - noise.wide) * 0.2;
        noise.fric += (noise.tf - noise.fric) * 0.2;
        face.setSpeechAmplitude(amp, { round: noise.round, wide: noise.wide, fricative: noise.fric });
      } else {
        face.setSpeechAmplitude(0);
      }
    }
    requestAnimationFrame(frame);
  }
  requestAnimationFrame(frame);

  // ----- Init -----
  if (list) {
    addItem('user', '<div class="text">Cześć, jaka jest dziś pogoda we Wrocławiu?</div>');
    addItem('bot', '<div class="text">We Wrocławiu jest teraz czternaście stopni i pochmurno. Po południu możliwy przelotny deszcz.</div>');
  }
  setState('listen');
  start(stage.getAttribute('data-auto') === 'false' ? 'listen' : 'auto');
})();
