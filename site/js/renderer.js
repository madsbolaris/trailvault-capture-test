// @ts-check
// Draws a World onto a <canvas>. Knows nothing about input or settings.

/** @typedef {import('./physics.js').World} World */
/** @typedef {import('./physics.js').Ball} Ball */

export class Renderer {
  /** @param {HTMLCanvasElement} canvas */
  constructor(canvas) {
    this.canvas = canvas;
    const ctx = canvas.getContext('2d', { alpha: true });
    if (!ctx) throw new Error('Canvas 2D is not supported');
    this.ctx = ctx;
    this.width = 0;
    this.height = 0;
    this.theme = { shadow: 'rgba(0,0,0,0.18)', ring: '#1d2330', floor: 'rgba(0,0,0,0.08)' };
  }

  /**
   * Match the backing store to the element's size and the device pixel ratio.
   * @param {number} width CSS px
   * @param {number} height CSS px
   * @param {number} dpr
   */
  resize(width, height, dpr) {
    this.width = width;
    this.height = height;
    this.canvas.width = Math.max(1, Math.round(width * dpr));
    this.canvas.height = Math.max(1, Math.round(height * dpr));
    this.ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  }

  /** Re-read theme colors from CSS custom properties on the canvas. */
  readTheme() {
    const css = getComputedStyle(this.canvas);
    const get = (/** @type {string} */ name, /** @type {string} */ fallback) =>
      css.getPropertyValue(name).trim() || fallback;
    this.theme = {
      shadow: get('--ball-shadow', this.theme.shadow),
      ring: get('--ball-ring', this.theme.ring),
      floor: get('--stage-floor', this.theme.floor),
    };
  }

  /**
   * @param {World} world
   * @param {{ hoverId?: number | null }} [state]
   */
  draw(world, state = {}) {
    const { ctx, width, height, theme } = this;
    ctx.clearRect(0, 0, width, height);

    // Floor line
    ctx.fillStyle = theme.floor;
    ctx.fillRect(0, height - 2, width, 2);

    // Shadows first so every ball draws above every shadow.
    ctx.fillStyle = theme.shadow;
    for (const b of world.balls) {
      const lift = Math.min(1, Math.max(0, (height - b.y - b.r) / (height * 0.8)));
      ctx.globalAlpha = 1 - lift * 0.85;
      ctx.beginPath();
      ctx.ellipse(b.x, height - 2, b.r * (1 - 0.5 * lift), Math.max(1.5, b.r * 0.18 * (1 - 0.5 * lift)), 0, 0, Math.PI * 2);
      ctx.fill();
    }
    ctx.globalAlpha = 1;

    const heldId = world.held?.ball.id ?? null;
    for (const b of world.balls) this.#drawBall(b, b.id === heldId || b.id === state.hoverId);
  }

  /**
   * @param {Ball} b
   * @param {boolean} highlighted
   */
  #drawBall(b, highlighted) {
    const { ctx } = this;
    ctx.save();
    ctx.translate(b.x, b.y);
    if (b.squash > 0.005) {
      // Flatten along the last impact normal, bulge across it.
      ctx.rotate(b.squashAngle);
      ctx.scale(1 - b.squash, 1 + b.squash * 0.6);
      ctx.rotate(-b.squashAngle);
    }
    const hx = -b.r * 0.35;
    const hy = -b.r * 0.4;
    const grad = ctx.createRadialGradient(hx, hy, b.r * 0.05, 0, 0, b.r);
    grad.addColorStop(0, 'rgba(255,255,255,0.85)');
    grad.addColorStop(0.22, b.color);
    grad.addColorStop(1, b.color);
    ctx.fillStyle = grad;
    ctx.beginPath();
    ctx.arc(0, 0, b.r, 0, Math.PI * 2);
    ctx.fill();

    // Soft rim shading for depth
    const rim = ctx.createRadialGradient(0, 0, b.r * 0.7, 0, 0, b.r);
    rim.addColorStop(0, 'rgba(0,0,0,0)');
    rim.addColorStop(1, 'rgba(0,0,0,0.14)');
    ctx.fillStyle = rim;
    ctx.fill();

    if (highlighted) {
      ctx.lineWidth = 2.5;
      ctx.strokeStyle = this.theme.ring;
      ctx.beginPath();
      ctx.arc(0, 0, b.r + 3, 0, Math.PI * 2);
      ctx.stroke();
    }
    ctx.restore();
  }
}
