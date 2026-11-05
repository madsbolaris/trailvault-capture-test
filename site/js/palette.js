// @ts-check
// Ball colors: four base hues, each with 10 shades whose RGB channels are
// scaled by -5%..+5% (capped at 255), so neighbouring balls never look identical.

/** @type {ReadonlyArray<{ name: string, base: string, shades: readonly string[] }>} */
export const COLOR_FAMILIES = Object.freeze([
  {
    name: 'red',
    base: '#F25022',
    shades: ['#E64C20', '#E94D21', '#EB4E21', '#EE4F21', '#F15022', '#F35022', '#F65123', '#F95223', '#FB5323', '#FE5424'],
  },
  {
    name: 'green',
    base: '#7FBA00',
    shades: ['#79B100', '#7AB300', '#7BB500', '#7DB700', '#7EB900', '#80BB00', '#81BD00', '#83BF00', '#84C100', '#85C300'],
  },
  {
    name: 'blue',
    base: '#00A4EF',
    shades: ['#009CE3', '#009EE6', '#009FE8', '#00A1EB', '#00A3EE', '#00A5F0', '#00A7F3', '#00A9F6', '#00AAF8', '#00ACFB'],
  },
  {
    name: 'yellow',
    base: '#FFB900',
    shades: ['#F2B000', '#F5B200', '#F8B400', '#FBB600', '#FEB800', '#FFBA00', '#FFBC00', '#FFBE00', '#FFC000', '#FFC200'],
  },
]);

/** All 40 shades, flattened. */
export const PALETTE = Object.freeze(COLOR_FAMILIES.flatMap((f) => f.shades));

/**
 * Pick a random shade.
 * @param {() => number} [rng]
 */
export function randomColor(rng = Math.random) {
  return PALETTE[Math.floor(rng() * PALETTE.length) % PALETTE.length];
}

/**
 * @param {string} hex like "#AABBCC"
 * @returns {[number, number, number]}
 */
export function hexToRgb(hex) {
  const m = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex);
  if (!m) throw new Error(`Invalid hex color: ${hex}`);
  return [parseInt(m[1], 16), parseInt(m[2], 16), parseInt(m[3], 16)];
}
