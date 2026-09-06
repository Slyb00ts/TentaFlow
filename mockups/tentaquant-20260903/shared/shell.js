// ===== File: shell.js — off-canvas navigation for the mockups on narrow screens =====
(function () {
  document.querySelectorAll('.app').forEach(function (app) {
    var toggle = app.querySelector('.nav-toggle');
    var scrim = app.querySelector('.nav-scrim');
    var open = function (on) { app.classList.toggle('nav-open', on); };
    if (toggle) toggle.addEventListener('click', function () { open(!app.classList.contains('nav-open')); });
    if (scrim) scrim.addEventListener('click', function () { open(false); });
    app.querySelectorAll('.sidebar .nav-item').forEach(function (item) {
      item.addEventListener('click', function () { open(false); });
    });
    document.addEventListener('keydown', function (e) { if (e.key === 'Escape') open(false); });
  });
})();
