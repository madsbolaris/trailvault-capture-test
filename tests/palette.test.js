import { test } from 'node:test';
import assert from 'node:assert/strict';
import { COLOR_FAMILIES, PALETTE, hexToRgb, randomColor } from '../site/js/palette.js';

test('four base colors with 10 shades each, 40 in total', () => {
  assert.deepEqual(
    COLOR_FAMILIES.map((f) => f.base),
    ['#F25022', '#7FBA00', '#00A4EF', '#FFB900'],
  );
  for (const f of COLOR_FAMILIES) assert.equal(f.shades.length, 10, f.name);
  assert.equal(PALETTE.length, 40);
});

test('every shade is a valid, unique hex color', () => {
  for (const c of PALETTE) assert.match(c, /^#[0-9A-F]{6}$/);
  assert.equal(new Set(PALETTE).size, 40);
});

test('every shade is within 5% of its base color on each channel', () => {
  for (const f of COLOR_FAMILIES) {
    const base = hexToRgb(f.base);
    for (const shade of f.shades) {
      hexToRgb(shade).forEach((v, i) => {
        const allowed = Math.max(1, base[i] * 0.05) + 0.5; // +0.5 for rounding to integers
        assert.ok(Math.abs(v - base[i]) <= allowed, `${f.name} ${shade} channel ${i}: ${v} vs ${base[i]}`);
      });
    }
  }
});

test('randomColor always returns a palette entry', () => {
  assert.equal(randomColor(() => 0), PALETTE[0]);
  assert.equal(randomColor(() => 0.999999), PALETTE[39]);
  for (let i = 0; i < 200; i++) assert.ok(PALETTE.includes(randomColor()));
});
