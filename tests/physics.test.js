import { test } from 'node:test';
import assert from 'node:assert/strict';
import { World, PX_PER_METER, FIXED_DT, MAX_BALLS, MAX_SPEED } from '../site/js/physics.js';

/** Deterministic PRNG (mulberry32) so tests are repeatable. */
function seeded(seed = 1) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const run = (world, seconds) => {
  for (let i = 0; i < Math.round(seconds / FIXED_DT); i++) world.step(FIXED_DT);
};

const maxOverlap = (balls) => {
  let worst = 0;
  for (let i = 0; i < balls.length; i++)
    for (let j = i + 1; j < balls.length; j++) {
      const a = balls[i], b = balls[j];
      worst = Math.max(worst, a.r + b.r - Math.hypot(a.x - b.x, a.y - b.y));
    }
  return worst;
};

test('a dropped ball falls at the configured gravity', () => {
  const world = new World({ width: 800, height: 100000, settings: { gravity: 1.62, airDrag: 0 }, rng: seeded() });
  const ball = world.addBall({ x: 400, y: 100, r: 10 });
  run(world, 1);
  const expectedVy = 1.62 * PX_PER_METER; // v = g·t
  assert.ok(Math.abs(ball.vy - expectedVy) < 0.5, `vy=${ball.vy}, expected ≈${expectedVy}`);
  const expectedDrop = 0.5 * expectedVy; // d = ½·g·t²
  assert.ok(Math.abs(ball.y - 100 - expectedDrop) < 2, `drop=${ball.y - 100}`);
});

test('zero gravity leaves a still ball still', () => {
  const world = new World({ width: 800, height: 600, settings: { gravity: 0 }, rng: seeded() });
  const ball = world.addBall({ x: 400, y: 300, r: 10 });
  run(world, 2);
  assert.equal(ball.x, 400);
  assert.equal(ball.y, 300);
});

test('a bounce off the floor keeps the bounciness fraction of the speed', () => {
  const world = new World({ width: 800, height: 600, settings: { gravity: 0, bounciness: 0.8, airDrag: 0 }, rng: seeded() });
  const ball = world.addBall({ x: 400, y: 500, vy: 1000, r: 10 });
  run(world, 0.2);
  assert.ok(ball.vy < 0, 'ball should be moving up after the bounce');
  assert.ok(Math.abs(ball.vy + 800) < 1, `vy=${ball.vy}`);
});

test('balls never leave the world or overlap after settling', () => {
  const world = new World({ width: 900, height: 480, settings: { gravity: 9.81 }, rng: seeded(7) });
  world.spawn(80);
  run(world, 8);
  for (const b of world.balls) {
    assert.ok(Number.isFinite(b.x) && Number.isFinite(b.y), 'position is finite');
    assert.ok(b.x >= b.r - 1e-6 && b.x <= 900 - b.r + 1e-6, `x=${b.x}`);
    assert.ok(b.y >= b.r - 1e-6 && b.y <= 480 - b.r + 1e-6, `y=${b.y}`);
  }
  assert.ok(maxOverlap(world.balls) < 1.5, `overlap=${maxOverlap(world.balls)}`);
});

test('a pile of balls comes to rest under gravity', () => {
  const world = new World({ width: 600, height: 400, settings: { gravity: 9.81, bounciness: 0.7 }, rng: seeded(3) });
  world.spawn(30);
  run(world, 20);
  const fastest = Math.max(...world.balls.map((b) => Math.hypot(b.vx, b.vy)));
  assert.ok(fastest < 30, `fastest ball still moving at ${fastest}px/s`);
});

test('a head-on elastic collision between equal balls swaps their velocities', () => {
  const world = new World({ width: 2000, height: 600, settings: { gravity: 0, bounciness: 1, airDrag: 0 }, rng: seeded() });
  const a = world.addBall({ x: 900, y: 300, vx: 300, r: 20 });
  const b = world.addBall({ x: 1100, y: 300, vx: -300, r: 20 });
  run(world, 0.5);
  assert.ok(Math.abs(a.vx + 300) < 1, `a.vx=${a.vx}`);
  assert.ok(Math.abs(b.vx - 300) < 1, `b.vx=${b.vx}`);
});

test('perfectly elastic collisions with no gravity or drag conserve energy', () => {
  const world = new World({ width: 1000, height: 800, settings: { gravity: 0, bounciness: 1, airDrag: 0 }, rng: seeded(11) });
  world.spawn(25);
  const before = world.kineticEnergy();
  run(world, 3);
  const after = world.kineticEnergy();
  assert.ok(Math.abs(after - before) / before < 0.01, `energy changed from ${before} to ${after}`);
});

test('the ball count is capped, dropping the oldest first', () => {
  const world = new World({ width: 2000, height: 2000, rng: seeded() });
  const first = world.addBall({ x: 10, y: 10, r: 5 });
  for (let i = 0; i < MAX_BALLS + 10; i++) world.addBall({ x: 500, y: 500, r: 5 });
  assert.equal(world.balls.length, MAX_BALLS);
  assert.ok(!world.balls.includes(first));
});

test('a held ball follows the pointer and is thrown on release', () => {
  const world = new World({ width: 1000, height: 600, settings: { gravity: 9.81 }, rng: seeded() });
  const ball = world.addBall({ x: 200, y: 300, r: 20 });
  world.grab(ball, 200, 300, 0);
  // Drag right at 1000 px/s for 0.2 s.
  for (let i = 1; i <= 12; i++) {
    world.dragTo(200 + (i * 1000) / 60, 300, i / 60);
    run(world, 1 / 60);
  }
  assert.ok(Math.abs(ball.x - 400) < 1, `x=${ball.x}`);
  assert.ok(Math.abs(ball.y - 300) < 1, 'gravity does not pull a held ball');
  world.release(12 / 60);
  assert.ok(ball.vx > 700 && ball.vx <= MAX_SPEED, `throw vx=${ball.vx}`);
  assert.ok(ball.invMass > 0, 'released ball is movable again');
});

test('releasing after holding still drops the ball gently', () => {
  const world = new World({ width: 1000, height: 600, rng: seeded() });
  const ball = world.addBall({ x: 200, y: 300, r: 20 });
  world.grab(ball, 200, 300, 0);
  world.dragTo(400, 300, 0.05);
  world.release(1); // pointer idle for almost a second
  assert.equal(ball.vx, 0);
});

test('resizing pulls balls back inside', () => {
  const world = new World({ width: 1000, height: 600, rng: seeded() });
  const ball = world.addBall({ x: 950, y: 550, r: 20 });
  world.setSize(400, 300);
  assert.equal(ball.x, 380);
  assert.equal(ball.y, 280);
});

test('advance runs fixed steps and caps catch-up after a long pause', () => {
  const world = new World({ width: 800, height: 600, rng: seeded() });
  assert.equal(world.advance(FIXED_DT * 4 + 1e-9), 4);
  assert.ok(world.advance(10) <= Math.ceil(0.1 / FIXED_DT));
});

test('reset spawns the starting count of balls inside the world', () => {
  const world = new World({ width: 800, height: 600, settings: { startCount: 15 }, rng: seeded() });
  world.reset();
  assert.equal(world.balls.length, 15);
});

test('ballAt finds the topmost ball under a point', () => {
  const world = new World({ width: 800, height: 600, rng: seeded() });
  world.addBall({ x: 100, y: 100, r: 30 });
  const top = world.addBall({ x: 110, y: 100, r: 30 });
  assert.equal(world.ballAt(105, 100), top);
  assert.equal(world.ballAt(500, 500), null);
});
