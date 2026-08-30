// Drives the real markup into a given state. Everything visual comes from
// oterm.css; this only flips the same classes the app flips.
const $ = (s) => document.querySelector(s);

async function loadMarks() {
  const names = { t1: 'claude', t2: 'gemini', t3: 'codex' };
  for (const [id, brand] of Object.entries(names)) {
    const svg = await (await fetch(`brand/${brand}.svg`)).text();
    document.querySelector(`#${id} .label`).innerHTML = svg;
  }
}

const STATES = ['state-idle', 'state-busy', 'state-done', 'state-needsInput'];

window.apply = function (s) {
  const b = document.body;
  b.classList.remove('mode-panel', 'mode-bar');
  b.classList.add(s.mode === 'bar' ? 'mode-bar' : 'mode-panel');

  STATES.forEach((c) => b.classList.remove(c));
  b.classList.add('state-' + s.status);
  b.classList.toggle('attention', !!s.attention);
  b.style.setProperty('--dot-op', s.dotOp === undefined ? 1 : s.dotOp);
  b.style.setProperty('--glow-op', s.glowOp === undefined ? 1 : s.glowOp);
  b.classList.toggle('attention-needs', s.status === 'needsInput');

  for (const el of document.querySelectorAll('.pill .label')) el.textContent = s.status === 'needsInput' ? 'needs you' : s.status;
  for (const el of document.querySelectorAll('.elapsed')) el.textContent = s.elapsed || '';
  $('#bar-cwd').textContent = s.cwd || '~/overterm';

  $('#term').innerHTML = s.term || '';
  $('#typed').textContent = s.typed || '';
  $('#caret').classList.toggle('off', !s.caret);
  $('#rail').style.visibility = s.mode === 'bar' ? 'hidden' : 'visible';
  $('#bar-input').value = s.barInput || '';

  // Per-tab dots: the active session takes the window state, the others
  // keep their own so the rail shows which one wants you.
  const tabStates = s.tabs || ['idle', 'idle', 'idle'];
  ['t1', 't2', 't3'].forEach((id, i) => {
    const el = $('#' + id);
    STATES.forEach((c) => el.classList.remove(c));
    el.classList.add('state-' + tabStates[i]);
  });
};

window.ready = (async () => {
  await loadMarks();
  await document.fonts.ready;
})();
