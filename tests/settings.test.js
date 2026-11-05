import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  DEFAULTS,
  LIMITS,
  GRAVITY_PRESETS,
  clampSetting,
  settingsFromHash,
  settingsToHash,
  presetForGravity,
} from '../site/js/settings.js';

test('defaults to Moon gravity', () => {
  assert.equal(DEFAULTS.gravity, 1.62);
  assert.equal(presetForGravity(DEFAULTS.gravity)?.id, 'moon');
});

test('every default and preset is within its limits', () => {
  for (const [key, value] of Object.entries(DEFAULTS)) {
    assert.ok(value >= LIMITS[key].min && value <= LIMITS[key].max, key);
  }
  for (const p of GRAVITY_PRESETS) {
    assert.ok(p.gravity >= LIMITS.gravity.min && p.gravity <= LIMITS.gravity.max, p.id);
  }
});

test('clampSetting clamps, snaps to step, and rejects junk', () => {
  assert.equal(clampSetting('gravity', 99), 25);
  assert.equal(clampSetting('gravity', -3), 0);
  assert.equal(clampSetting('gravity', '3.714'), 3.71);
  assert.equal(clampSetting('startCount', 7.6), 8);
  assert.equal(clampSetting('bounciness', 'abc'), DEFAULTS.bounciness);
  assert.equal(clampSetting('bounciness', ''), DEFAULTS.bounciness);
  assert.equal(clampSetting('ballSize', Infinity), DEFAULTS.ballSize);
});

test('the default configuration has an empty hash', () => {
  assert.equal(settingsToHash(DEFAULTS), '');
  assert.deepEqual(settingsFromHash(''), DEFAULTS);
});

test('settings round-trip through the URL hash', () => {
  const custom = { gravity: 9.81, bounciness: 0.9, airDrag: 0.1, ballSize: 30, startCount: 40 };
  const hash = settingsToHash(custom);
  assert.equal(hash, '#g=9.81&e=0.9&d=0.1&s=30&n=40');
  assert.deepEqual(settingsFromHash(hash), custom);
});

test('a hash only encodes what differs from the defaults', () => {
  assert.equal(settingsToHash({ ...DEFAULTS, gravity: 24.79 }), '#g=24.79');
});

test('hostile or malformed hashes fall back to safe values', () => {
  const s = settingsFromHash('#g=1e9&e=-5&n=NaN&s=<script>&x=1');
  assert.equal(s.gravity, LIMITS.gravity.max);
  assert.equal(s.bounciness, LIMITS.bounciness.min);
  assert.equal(s.startCount, DEFAULTS.startCount);
  assert.equal(s.ballSize, DEFAULTS.ballSize);
  assert.ok(!('x' in s));
});
