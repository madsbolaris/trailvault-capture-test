// @ts-check
// Simulation settings: defaults, limits, gravity presets, and URL (hash) round-tripping
// so a configuration can be shared as a link.

/**
 * @typedef {object} Settings
 * @property {number} gravity     m/s²
 * @property {number} bounciness  coefficient of restitution, 0.3–1
 * @property {number} airDrag     fraction of speed lost per second, 0–0.5
 * @property {number} ballSize    average ball radius in CSS pixels
 * @property {number} startCount  balls spawned on reset
 */

/** Surface gravity of a few bodies, in m/s². */
export const GRAVITY_PRESETS = Object.freeze([
  { id: 'zero-g', label: 'Zero-G', gravity: 0 },
  { id: 'moon', label: 'Moon', gravity: 1.62 },
  { id: 'mars', label: 'Mars', gravity: 3.71 },
  { id: 'earth', label: 'Earth', gravity: 9.81 },
  { id: 'jupiter', label: 'Jupiter', gravity: 24.79 },
]);

/** @type {Readonly<Record<keyof Settings, { min: number, max: number, step: number }>>} */
export const LIMITS = Object.freeze({
  gravity: { min: 0, max: 25, step: 0.01 },
  bounciness: { min: 0.3, max: 1, step: 0.01 },
  airDrag: { min: 0, max: 0.5, step: 0.01 },
  ballSize: { min: 8, max: 40, step: 1 },
  startCount: { min: 1, max: 100, step: 1 },
});

/** @type {Readonly<Settings>} */
export const DEFAULTS = Object.freeze({
  gravity: 1.62, // the Moon
  bounciness: 0.85,
  airDrag: 0.02,
  ballSize: 20,
  startCount: 12,
});

// Short keys keep shared URLs tidy.
/** @type {Readonly<Record<keyof Settings, string>>} */
const HASH_KEYS = Object.freeze({
  gravity: 'g',
  bounciness: 'e',
  airDrag: 'd',
  ballSize: 's',
  startCount: 'n',
});

/**
 * Clamp a value into the setting's range and snap it to the step.
 * Returns the default for anything that is not a finite number.
 * @param {keyof Settings} key
 * @param {unknown} value
 */
export function clampSetting(key, value) {
  const { min, max, step } = LIMITS[key];
  const n = typeof value === 'string' && value.trim() === '' ? NaN : Number(value);
  if (!Number.isFinite(n)) return DEFAULTS[key];
  const snapped = Math.round(n / step) * step;
  const decimals = (String(step).split('.')[1] || '').length;
  return Number(Math.min(max, Math.max(min, snapped)).toFixed(decimals));
}

/**
 * @param {Partial<Record<keyof Settings, unknown>>} [partial]
 * @returns {Settings}
 */
export function normalizeSettings(partial = {}) {
  /** @type {any} */
  const out = {};
  for (const key of /** @type {(keyof Settings)[]} */ (Object.keys(DEFAULTS))) {
    out[key] = key in partial ? clampSetting(key, partial[key]) : DEFAULTS[key];
  }
  return out;
}

/**
 * Read settings from a location hash such as "#g=9.81&e=0.9".
 * Unknown keys are ignored; invalid values fall back to defaults.
 * @param {string} hash
 * @returns {Settings}
 */
export function settingsFromHash(hash) {
  const params = new URLSearchParams(hash.replace(/^#/, ''));
  /** @type {Partial<Record<keyof Settings, unknown>>} */
  const partial = {};
  for (const [key, short] of Object.entries(HASH_KEYS)) {
    if (params.has(short)) partial[/** @type {keyof Settings} */ (key)] = params.get(short);
  }
  return normalizeSettings(partial);
}

/**
 * Encode only the settings that differ from the defaults, so the default view has a clean URL.
 * @param {Settings} settings
 * @returns {string} hash including "#", or "" when everything is default
 */
export function settingsToHash(settings) {
  const params = new URLSearchParams();
  for (const [key, short] of Object.entries(HASH_KEYS)) {
    const k = /** @type {keyof Settings} */ (key);
    const v = clampSetting(k, settings[k]);
    if (v !== DEFAULTS[k]) params.set(short, String(v));
  }
  const s = params.toString();
  return s ? `#${s}` : '';
}

/**
 * The preset whose gravity matches exactly, if any.
 * @param {number} gravity
 */
export function presetForGravity(gravity) {
  return GRAVITY_PRESETS.find((p) => Math.abs(p.gravity - gravity) < 1e-9) || null;
}
