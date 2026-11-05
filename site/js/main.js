// @ts-check
// Bounce Lab entry point: wires the DOM, input and animation loop to the physics and renderer.

import { World } from './physics.js';
import { Renderer } from './renderer.js';
import {
  DEFAULTS,
  GRAVITY_PRESETS,
  LIMITS,
  clampSetting,
  normalizeSettings,
  presetForGravity,
  settingsFromHash,
  settingsToHash,
} from './settings.js';

/** @typedef {import('./settings.js').Settings} Settings */
/** @typedef {keyof Settings} SettingKey */

const STORAGE_KEY = 'bounce-lab:settings:v1';

/** @template {HTMLElement} T @param {string} id @returns {T} */
const $ = (id) => {
  const el = document.getElementById(id);
  if (!el) throw new Error(`Missing #${id}`);
  return /** @type {T} */ (el);
};

const canvas = /** @type {HTMLCanvasElement} */ ($('canvas'));
const stage = /** @type {HTMLElement} */ (canvas.parentElement);
const hint = $('hint');
const pausedBadge = $('paused-badge');
const pauseBtn = $('pause-btn');
const presetsEl = $('presets');
const statBalls = $('stat-balls');
const statFps = $('stat-fps');
const toastEl = $('toast');
const shortcuts = /** @type {HTMLDialogElement} */ ($('shortcuts'));
/** @type {HTMLInputElement[]} */
const sliders = Array.from(document.querySelectorAll('input[data-setting]'));

// ---------------------------------------------------------------- settings

/** URL wins (it's what people share); otherwise the last settings this browser used. */
function initialSettings() {
  if (location.hash.length > 1) return settingsFromHash(location.hash);
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved) return normalizeSettings(JSON.parse(saved));
  } catch {
    /* storage unavailable or corrupt: use defaults */
  }
  return { ...DEFAULTS };
}

/** @type {Settings} */
let settings = initialSettings();
let saveTimer = 0;

function persist() {
  clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => {
    const hash = settingsToHash(settings);
    history.replaceState(null, '', hash || location.pathname + location.search);
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(settings));
    } catch {
      /* private mode etc. */
    }
  }, 150);
}

/** @param {SettingKey} key @param {number} v */
function formatSetting(key, v) {
  switch (key) {
    case 'gravity': {
      const preset = presetForGravity(v);
      return `${v.toFixed(2)} m/s²${preset && preset.gravity > 0 ? ` · ${preset.label}` : ''}`;
    }
    case 'bounciness':
      return `${Math.round(v * 100)}%`;
    case 'airDrag':
      return v === 0 ? 'None' : `${Math.round(v * 100)}%/s`;
    case 'ballSize':
      return `${v * 2}px`;
    case 'startCount':
      return String(v);
  }
}

/** @param {Partial<Settings>} next */
function applySettings(next) {
  settings = normalizeSettings({ ...settings, ...next });
  world.setSettings(settings);
  renderControls();
  persist();
}

function renderControls() {
  for (const input of sliders) {
    const key = /** @type {SettingKey} */ (input.dataset.setting);
    if (Number(input.value) !== settings[key]) input.value = String(settings[key]);
    const out = document.getElementById(`${key}-out`);
    if (out) out.textContent = formatSetting(key, settings[key]);
    input.setAttribute('aria-valuetext', formatSetting(key, settings[key]));
  }
  for (const btn of presetsEl.querySelectorAll('button')) {
    btn.setAttribute('aria-pressed', String(Number(btn.dataset.gravity) === settings.gravity));
  }
}

for (const input of sliders) {
  const key = /** @type {SettingKey} */ (input.dataset.setting);
  const { min, max, step } = LIMITS[key];
  Object.assign(input, { min: String(min), max: String(max), step: String(step) });
  input.addEventListener('input', () => applySettings({ [key]: clampSetting(key, input.value) }));
}

for (const p of GRAVITY_PRESETS) {
  const btn = document.createElement('button');
  btn.type = 'button';
  btn.className = 'preset';
  btn.textContent = p.label;
  btn.dataset.gravity = String(p.gravity);
  btn.title = `${p.label}: ${p.gravity} m/s²`;
  btn.addEventListener('click', () => applySettings({ gravity: p.gravity }));
  presetsEl.append(btn);
}

window.addEventListener('hashchange', () => applySettings(settingsFromHash(location.hash)));

// ---------------------------------------------------------------- world + rendering

const world = new World({ width: 0, height: 0, settings });
const renderer = new Renderer(canvas);
renderer.readTheme();
window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => renderer.readTheme());

let seeded = false;
function resize() {
  const { width, height } = stage.getBoundingClientRect();
  if (width < 1 || height < 1) return; // not laid out yet (e.g. hidden tab)
  renderer.resize(width, height, Math.min(window.devicePixelRatio || 1, 2.5));
  world.setSize(width, height);
  if (!seeded) {
    seeded = true;
    world.reset();
  }
}
new ResizeObserver(resize).observe(stage);

// ---------------------------------------------------------------- playback

const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)');
let paused = reducedMotion.matches;

/** @param {boolean} value */
function setPaused(value) {
  paused = value;
  pauseBtn.textContent = paused ? 'Play' : 'Pause';
  pauseBtn.setAttribute('aria-pressed', String(paused));
  pausedBadge.hidden = !paused;
}
setPaused(paused);
if (paused) toast('Paused because your system prefers reduced motion. Press Play to start.', 5000);

let last = performance.now();
let frames = 0;
let fpsWindowStart = last;
/** @type {number | null} */
let hoverId = null;

/** @param {number} now */
function frame(now) {
  const elapsed = (now - last) / 1000;
  last = now;
  if (!paused) world.advance(elapsed);
  else world.syncHeld();
  renderer.draw(world, { hoverId });

  frames++;
  if (now - fpsWindowStart >= 500) {
    statFps.textContent = paused ? '–' : String(Math.round((frames * 1000) / (now - fpsWindowStart)));
    statBalls.textContent = String(world.balls.length);
    frames = 0;
    fpsWindowStart = now;
  }
  requestAnimationFrame(frame);
}
requestAnimationFrame((t) => {
  last = t;
  fpsWindowStart = t;
  frame(t);
});

// Don't simulate the time a background tab spent asleep.
document.addEventListener('visibilitychange', () => {
  last = performance.now();
});

// ---------------------------------------------------------------- pointer input

/** @param {PointerEvent} ev */
function localPoint(ev) {
  const rect = canvas.getBoundingClientRect();
  return { x: ev.clientX - rect.left, y: ev.clientY - rect.top };
}

if (matchMedia('(pointer: coarse)').matches) hint.textContent = 'Tap to drop a ball · Drag one to throw it';
let hintDismissed = false;
function dismissHint() {
  if (hintDismissed) return;
  hintDismissed = true;
  hint.classList.add('is-hidden');
}

canvas.addEventListener('pointerdown', (ev) => {
  if (ev.button !== 0) return;
  dismissHint();
  const { x, y } = localPoint(ev);
  const t = ev.timeStamp / 1000;
  const ball = world.ballAt(x, y);
  if (ball) {
    world.grab(ball, x, y, t);
    try {
      canvas.setPointerCapture(ev.pointerId);
    } catch {
      /* pointer already gone */
    }
    canvas.classList.add('grabbing');
  } else {
    world.addBall({ x, y, vx: (Math.random() - 0.5) * 120 });
  }
  ev.preventDefault();
});

canvas.addEventListener('pointermove', (ev) => {
  const { x, y } = localPoint(ev);
  if (world.held) {
    world.dragTo(x, y, ev.timeStamp / 1000);
    return;
  }
  if (ev.pointerType === 'mouse') {
    hoverId = world.ballAt(x, y)?.id ?? null;
    canvas.classList.toggle('can-grab', hoverId !== null);
  }
});

/** @param {PointerEvent} ev */
function endDrag(ev) {
  if (!world.held) return;
  world.release(ev.timeStamp / 1000);
  canvas.classList.remove('grabbing');
}
canvas.addEventListener('pointerup', endDrag);
canvas.addEventListener('pointercancel', endDrag);
canvas.addEventListener('lostpointercapture', endDrag);
canvas.addEventListener('pointerleave', () => {
  hoverId = null;
  canvas.classList.remove('can-grab');
});

// ---------------------------------------------------------------- buttons

function dropBallAtTop() {
  dismissHint();
  world.addBall({ x: world.width * (0.3 + Math.random() * 0.4), y: 40, vx: (Math.random() - 0.5) * 200 });
}

const actions = {
  togglePause: () => setPaused(!paused),
  reset: () => {
    world.reset();
    toast('Reset');
  },
  add: () => {
    dismissHint();
    world.spawn(10);
  },
  shake: () => {
    if (paused) setPaused(false);
    world.shake();
  },
  clear: () => world.clear(),
};

pauseBtn.addEventListener('click', actions.togglePause);
$('reset-btn').addEventListener('click', actions.reset);
$('add-btn').addEventListener('click', actions.add);
$('shake-btn').addEventListener('click', actions.shake);
$('clear-btn').addEventListener('click', actions.clear);
$('defaults-btn').addEventListener('click', () => {
  applySettings({ ...DEFAULTS });
  toast('Settings restored to defaults');
});
$('shortcuts-btn').addEventListener('click', () => shortcuts.showModal());
$('share-btn').addEventListener('click', share);

async function share() {
  clearTimeout(saveTimer);
  const url = new URL(location.href);
  url.hash = settingsToHash(settings);
  const link = url.toString();
  history.replaceState(null, '', url.hash || location.pathname + location.search);
  try {
    if (navigator.share && matchMedia('(pointer: coarse)').matches) {
      await navigator.share({ title: 'Bounce Lab', url: link });
      return;
    }
    await navigator.clipboard.writeText(link);
    toast('Link copied to clipboard');
  } catch (err) {
    if (err instanceof DOMException && err.name === 'AbortError') return; // share sheet dismissed
    window.prompt('Copy this link:', link);
  }
}

// ---------------------------------------------------------------- keyboard

document.addEventListener('keydown', (ev) => {
  if (ev.metaKey || ev.ctrlKey || ev.altKey) return;
  const target = /** @type {HTMLElement} */ (ev.target);
  if (shortcuts.open) return;
  // Leave keys alone while the user is operating a form control.
  const inControl = target.closest('input, button, select, textarea, a, [contenteditable]');

  if (ev.key === 'Enter' && target === canvas) {
    dropBallAtTop();
    ev.preventDefault();
    return;
  }
  if (ev.key === ' ' && !inControl) {
    actions.togglePause();
    ev.preventDefault();
    return;
  }
  if (inControl && target instanceof HTMLInputElement) return; // arrows etc. on sliders

  switch (ev.key.toLowerCase()) {
    case 'r': actions.reset(); break;
    case 'a': actions.add(); break;
    case 's': actions.shake(); break;
    case '?': shortcuts.showModal(); break;
    default: {
      const preset = GRAVITY_PRESETS[Number(ev.key) - 1];
      if (!preset) return;
      applySettings({ gravity: preset.gravity });
      toast(`Gravity: ${preset.label}`);
    }
  }
  ev.preventDefault();
});

// ---------------------------------------------------------------- toast

let toastTimer = 0;
/** @param {string} message @param {number} [ms] */
function toast(message, ms = 1800) {
  toastEl.textContent = message;
  toastEl.classList.add('is-visible');
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => toastEl.classList.remove('is-visible'), ms);
}

renderControls();
persist(); // make the URL reflect settings restored from storage
