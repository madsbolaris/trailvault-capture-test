// @ts-check
// Bounce Lab physics. Pure and DOM-free so it can be unit tested in Node.
//
// Units: positions in CSS pixels, time in seconds. Gravity comes in as m/s² and is
// converted with PX_PER_METER. The world runs at a fixed timestep for stable,
// frame-rate-independent results.

import { randomColor } from './palette.js';
import { DEFAULTS } from './settings.js';

/** @typedef {import('./settings.js').Settings} Settings */

/** On-screen pixels per simulated meter. */
export const PX_PER_METER = 140;
/** Fixed simulation step (240 Hz). */
export const FIXED_DT = 1 / 240;
/** Hard cap on the number of balls, to keep the page responsive. */
export const MAX_BALLS = 300;
/** Upper bound on any ball's speed, in px/s. */
export const MAX_SPEED = 5000;
/** Most simulation time we'll catch up on in one frame (e.g. after a hitch). */
const MAX_FRAME_TIME = 0.1;
/** Fraction of horizontal speed kept per second while rolling on the floor. */
const FLOOR_GRIP = 0.5;
/** Overlap tolerated before positional correction kicks in (px). */
const SLOP = 0.05;
/** Collision solver iterations per step. */
const SOLVER_PASSES = 2;

/**
 * @typedef {object} Ball
 * @property {number} id
 * @property {number} x
 * @property {number} y
 * @property {number} vx
 * @property {number} vy
 * @property {number} r
 * @property {number} invMass 0 means immovable (used while a ball is held)
 * @property {string} color
 * @property {number} squash      0..0.35, visual deformation after an impact
 * @property {number} squashAngle direction of the last impact normal (radians)
 */

export class World {
  /**
   * @param {{ width: number, height: number, settings?: Partial<Settings>, rng?: () => number }} options
   */
  constructor({ width, height, settings = {}, rng = Math.random }) {
    this.width = Math.max(0, width);
    this.height = Math.max(0, height);
    /** @type {Settings} */
    this.settings = { ...DEFAULTS, ...settings };
    this.rng = rng;
    /** @type {Ball[]} */
    this.balls = [];
    this.nextId = 1;
    /** @type {null | { ball: Ball, dx: number, dy: number, tx: number, ty: number, vx: number, vy: number, t: number }} */
    this.held = null;
    this.accumulator = 0;
  }

  /** @param {Partial<Settings>} settings */
  setSettings(settings) {
    this.settings = { ...this.settings, ...settings };
  }

  /**
   * Resize the world, keeping every ball inside it.
   * @param {number} width
   * @param {number} height
   */
  setSize(width, height) {
    this.width = Math.max(0, width);
    this.height = Math.max(0, height);
    for (const b of this.balls) this.#keepInside(b);
  }

  /**
   * @param {{ x: number, y: number, vx?: number, vy?: number, r?: number, color?: string }} init
   * @returns {Ball}
   */
  addBall({ x, y, vx = 0, vy = 0, r, color }) {
    const radius = r ?? this.#randomRadius();
    /** @type {Ball} */
    const ball = {
      id: this.nextId++,
      x,
      y,
      vx,
      vy,
      r: radius,
      invMass: 1 / (radius * radius),
      color: color ?? randomColor(this.rng),
      squash: 0,
      squashAngle: 0,
    };
    this.#keepInside(ball);
    this.balls.push(ball);
    // Drop the oldest ball that isn't being held.
    while (this.balls.length > MAX_BALLS) {
      const i = this.balls.findIndex((b) => b !== this.held?.ball);
      this.balls.splice(i, 1);
    }
    return ball;
  }

  /**
   * Scatter `count` balls across the upper part of the world with a little sideways motion.
   * @param {number} count
   */
  spawn(count) {
    for (let i = 0; i < count; i++) {
      const r = this.#randomRadius();
      this.addBall({
        x: r + this.rng() * Math.max(0, this.width - 2 * r),
        y: r + this.rng() * Math.max(0, this.height * 0.4 - r),
        vx: (this.rng() - 0.5) * 400,
        vy: (this.rng() - 0.5) * 200,
        r,
      });
    }
  }

  /** Remove every ball. */
  clear() {
    this.balls = [];
    this.held = null;
  }

  /** Clear and re-spawn the configured starting set. */
  reset() {
    this.clear();
    this.spawn(this.settings.startCount);
  }

  /**
   * Kick every ball upward and sideways.
   * @param {number} [strength] px/s
   */
  shake(strength = 900) {
    for (const b of this.balls) {
      if (b === this.held?.ball) continue;
      b.vx += (this.rng() - 0.5) * strength;
      b.vy -= strength * (0.5 + this.rng() * 0.5);
    }
  }

  /**
   * Topmost ball under a point, if any.
   * @param {number} x
   * @param {number} y
   */
  ballAt(x, y) {
    for (let i = this.balls.length - 1; i >= 0; i--) {
      const b = this.balls[i];
      if ((b.x - x) ** 2 + (b.y - y) ** 2 <= b.r * b.r) return b;
    }
    return null;
  }

  /**
   * Start dragging a ball. While held it follows the pointer and pushes others aside.
   * @param {Ball} ball
   * @param {number} x
   * @param {number} y
   * @param {number} t time in seconds (e.g. performance.now() / 1000)
   */
  grab(ball, x, y, t) {
    // Bring it to the front so it draws above the rest.
    this.balls.splice(this.balls.indexOf(ball), 1);
    this.balls.push(ball);
    ball.invMass = 0;
    this.held = { ball, dx: ball.x - x, dy: ball.y - y, tx: ball.x, ty: ball.y, vx: 0, vy: 0, t };
  }

  /**
   * Move the held ball's target, tracking a smoothed pointer velocity for the throw.
   * @param {number} x
   * @param {number} y
   * @param {number} t seconds
   */
  dragTo(x, y, t) {
    const h = this.held;
    if (!h) return;
    const tx = x + h.dx;
    const ty = y + h.dy;
    const dt = t - h.t;
    if (dt > 1e-4) {
      const k = Math.min(1, dt / 0.05); // ~50ms smoothing window
      h.vx += ((tx - h.tx) / dt - h.vx) * k;
      h.vy += ((ty - h.ty) / dt - h.vy) * k;
      h.t = t;
    }
    h.tx = tx;
    h.ty = ty;
  }

  /** Snap the held ball to its target immediately (used while the simulation is paused). */
  syncHeld() {
    const h = this.held;
    if (!h) return;
    h.ball.x = h.tx;
    h.ball.y = h.ty;
    this.#keepInside(h.ball);
  }

  /**
   * Let go of the held ball, throwing it with the pointer's recent velocity.
   * @param {number} [t] seconds; if the pointer has been still for a while, the throw is soft
   */
  release(t) {
    const h = this.held;
    if (!h) return;
    const idle = t === undefined ? 0 : t - h.t;
    const keep = idle > 0.1 ? 0 : 1;
    h.ball.vx = clampSpeed(h.vx * keep);
    h.ball.vy = clampSpeed(h.vy * keep);
    h.ball.invMass = 1 / (h.ball.r * h.ball.r);
    this.held = null;
  }

  /**
   * Advance by real elapsed time using fixed steps. Returns the number of steps run.
   * @param {number} elapsed seconds since the previous call
   */
  advance(elapsed) {
    this.accumulator = Math.min(this.accumulator + Math.max(0, elapsed), MAX_FRAME_TIME);
    let steps = 0;
    while (this.accumulator >= FIXED_DT) {
      this.step(FIXED_DT);
      this.accumulator -= FIXED_DT;
      steps++;
    }
    return steps;
  }

  /**
   * One simulation step.
   * @param {number} dt seconds
   */
  step(dt) {
    const { bounciness: e, airDrag } = this.settings;
    const g = this.settings.gravity * PX_PER_METER;
    const drag = Math.pow(1 - airDrag, dt);
    const grip = Math.pow(FLOOR_GRIP, dt);
    const settleSpeed = g * dt * 4 + 4;
    const squashDecay = Math.pow(0.0005, dt);

    for (const b of this.balls) {
      b.squash *= squashDecay;

      if (b === this.held?.ball) {
        b.vx = clampSpeed((this.held.tx - b.x) / dt);
        b.vy = clampSpeed((this.held.ty - b.y) / dt);
        b.x += b.vx * dt;
        b.y += b.vy * dt;
        this.#keepInside(b);
        continue;
      }

      b.vy += g * dt;
      b.vx *= drag;
      b.vy *= drag;
      b.x += b.vx * dt;
      b.y += b.vy * dt;

      if (b.x - b.r < 0) {
        b.x = b.r;
        if (b.vx < 0) this.#bounce(b, -b.vx, Math.PI, e, 'x');
      } else if (b.x + b.r > this.width) {
        b.x = this.width - b.r;
        if (b.vx > 0) this.#bounce(b, b.vx, 0, e, 'x');
      }
      if (b.y - b.r < 0) {
        b.y = b.r;
        if (b.vy < 0) this.#bounce(b, -b.vy, -Math.PI / 2, e, 'y');
      } else if (b.y + b.r >= this.height) {
        b.y = this.height - b.r;
        if (b.vy > 0) this.#bounce(b, b.vy, Math.PI / 2, e, 'y');
        if (Math.abs(b.vy) < settleSpeed) b.vy = 0; // come to rest instead of buzzing
        b.vx *= grip;
      }
    }

    // Two solver passes keep tall piles from sinking into each other.
    for (let pass = 0; pass < SOLVER_PASSES; pass++) this.#collide(e);
  }

  /** Total kinetic energy (arbitrary units), useful for tests and debugging. */
  kineticEnergy() {
    let sum = 0;
    for (const b of this.balls) {
      if (b.invMass === 0) continue;
      sum += 0.5 * (1 / b.invMass) * (b.vx * b.vx + b.vy * b.vy);
    }
    return sum;
  }

  // Sweep-and-prune: sort by left edge, then only test balls whose x-extents overlap.
  /** @param {number} e */
  #collide(e) {
    const order = this.balls.slice().sort((a, b) => a.x - a.r - (b.x - b.r));
    for (let i = 0; i < order.length; i++) {
      const a = order[i];
      const right = a.x + a.r;
      for (let j = i + 1; j < order.length; j++) {
        const b = order[j];
        if (b.x - b.r > right) break;
        this.#resolve(a, b, e);
      }
    }
  }

  /**
   * @param {Ball} a
   * @param {Ball} b
   * @param {number} e
   */
  #resolve(a, b, e) {
    const dx = b.x - a.x;
    const dy = b.y - a.y;
    const minDist = a.r + b.r;
    const distSq = dx * dx + dy * dy;
    if (distSq >= minDist * minDist) return;
    const invSum = a.invMass + b.invMass;
    if (invSum === 0) return;

    let dist = Math.sqrt(distSq);
    let nx, ny;
    if (dist < 1e-6) {
      // Perfectly stacked: push apart in a random direction.
      const angle = this.rng() * Math.PI * 2;
      nx = Math.cos(angle);
      ny = Math.sin(angle);
      dist = 0;
    } else {
      nx = dx / dist;
      ny = dy / dist;
    }

    // Positional correction, weighted by inverse mass.
    const correction = Math.max(0, minDist - dist - SLOP) / invSum;
    a.x -= nx * correction * a.invMass;
    a.y -= ny * correction * a.invMass;
    b.x += nx * correction * b.invMass;
    b.y += ny * correction * b.invMass;
    this.#keepInside(a);
    this.#keepInside(b);

    // Impulse along the contact normal.
    const rel = (b.vx - a.vx) * nx + (b.vy - a.vy) * ny;
    if (rel >= 0) return; // already separating
    const j = (-(1 + e) * rel) / invSum;
    a.vx = clampSpeed(a.vx - j * nx * a.invMass);
    a.vy = clampSpeed(a.vy - j * ny * a.invMass);
    b.vx = clampSpeed(b.vx + j * nx * b.invMass);
    b.vy = clampSpeed(b.vy + j * ny * b.invMass);

    const impact = Math.min(0.35, -rel / 4000);
    const angle = Math.atan2(ny, nx);
    if (impact > a.squash) { a.squash = impact; a.squashAngle = angle; }
    if (impact > b.squash) { b.squash = impact; b.squashAngle = angle + Math.PI; }
  }

  /**
   * Reflect one velocity component off a wall.
   * @param {Ball} b
   * @param {number} speed speed into the wall (positive)
   * @param {number} angle wall normal direction, for the squash effect
   * @param {number} e restitution
   * @param {'x' | 'y'} axis
   */
  #bounce(b, speed, angle, e, axis) {
    if (axis === 'x') b.vx = -b.vx * e;
    else b.vy = -b.vy * e;
    const impact = Math.min(0.35, speed / 4000);
    if (impact > b.squash) {
      b.squash = impact;
      b.squashAngle = angle;
    }
  }

  /** @param {Ball} b */
  #keepInside(b) {
    b.x = this.width >= 2 * b.r ? Math.min(Math.max(b.x, b.r), this.width - b.r) : this.width / 2;
    b.y = this.height >= 2 * b.r ? Math.min(Math.max(b.y, b.r), this.height - b.r) : this.height / 2;
  }

  #randomRadius() {
    const base = this.settings.ballSize;
    return Math.max(4, base * (0.6 + this.rng() * 0.8));
  }
}

/** @param {number} v */
function clampSpeed(v) {
  return Math.max(-MAX_SPEED, Math.min(MAX_SPEED, v));
}
