//! CPU particle simulation for Wallpaper Engine particle systems.
//!
//! The definition (`hyprwpe_core::particle::System`) says *what* a system is; this
//! turns it into *where the sprites are* at a given time. The model is the
//! standard one the editor authors:
//!
//! * an **emitter** decides where and how often particles appear,
//! * **initializers** set each new particle's starting state,
//! * **operators** change every particle over its life each frame,
//! * a **renderer** turns the state into drawable sprites.
//!
//! It is a simulation, so it is stateful and time-dependent, and it is seeded so
//! the same input yields the same output — a particle system that cannot be
//! tested is a particle system that cannot be trusted.
//!
//! Unknown behaviour names are skipped rather than fatal: a definition from a
//! newer editor version still renders, just without the behaviour we do not yet
//! model.

use hyprwpe_core::particle::{Item, System};

/// One live particle.
#[derive(Debug, Clone, Default)]
struct Particle {
    pos: [f32; 3],
    vel: [f32; 3],
    /// Base size in model units, before `sizechange`/`oscillatesize`.
    size: f32,
    /// The size the particle was born with, so `sizechange` can be applied from
    /// a fixed base each frame instead of compounding.
    size_base: f32,
    has_size_base: bool,
    color: [f32; 3],
    /// Base alpha, before fades and oscillation.
    alpha: f32,
    /// The alpha the particle was born with. Fades and oscillation are computed
    /// from this each frame rather than compounding a per-frame multiplication,
    /// which would decay exponentially instead of following the fade curve.
    alpha_base: f32,
    rotation: f32,
    angular_velocity: f32,
    lifetime: f32,
    age: f32,
    /// Random per-particle phase in `0..1`, for oscillation and turbulence.
    phase: f32,
    /// Random per-particle seed in `0..1`, for per-particle turbulence sampling.
    seed: f32,
    /// Which control point the particle follows (`-1` for the object origin).
    control: i32,
    /// Past positions for trail renderers, newest last.
    trail: Vec<[f32; 3]>,
}

/// A sprite ready to draw, in model space.
#[derive(Debug, Clone, Copy)]
pub struct Sprite {
    pub pos: [f32; 3],
    pub size: f32,
    pub rotation: f32,
    pub color: [f32; 3],
    pub alpha: f32,
}

/// A small deterministic PRNG (xorshift32). Enough for particle jitter and,
/// unlike a system RNG, reproducible in tests.
#[derive(Debug, Clone)]
struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Self {
        Rng(seed | 1)
    }

    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// Uniform in `0..1`.
    fn unit(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / (1 << 24) as f32
    }

    /// Uniform in `a..b`. Equal or inverted bounds yield `a`, which keeps a
    /// mis-authored range from producing a NaN or a negative span.
    fn range(&mut self, a: f32, b: f32) -> f32 {
        if b <= a || !b.is_finite() || !a.is_finite() {
            return a;
        }
        a + (b - a) * self.unit()
    }

    fn range3(&mut self, a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [
            self.range(a[0], b[0]),
            self.range(a[1], b[1]),
            self.range(a[2], b[2]),
        ]
    }
}

/// A running particle system.
pub struct Sim {
    system: System,
    particles: Vec<Particle>,
    rng: Rng,
    time: f32,
    /// Fractional particles owed to each emitter between frames.
    emit_credit: Vec<f32>,
    pub control_points: Vec<[f32; 3]>,
    maxcount: usize,
}

impl Sim {
    pub fn new(system: System, control_points: Vec<[f32; 3]>, seed: u32) -> Self {
        let n = system.emitters.len();
        let maxcount = system.maxcount.max(1);
        Sim {
            system,
            particles: Vec::new(),
            rng: Rng::new(seed),
            time: 0.0,
            emit_credit: vec![0.0; n],
            control_points,
            maxcount,
        }
    }

    pub fn live(&self) -> usize {
        self.particles.len()
    }

    /// Advance to absolute time `t` in fixed steps and return the sprites.
    ///
    /// For offline rendering there is no wall clock, so a fixed step keeps a
    /// rendered frame reproducible regardless of how fast the host is.
    pub fn advance_to(&mut self, t: f32) -> Vec<Sprite> {
        const DT: f32 = 1.0 / 60.0;
        let mut last = Vec::new();
        while self.time < t - 1e-6 {
            let step = DT.min(t - self.time);
            last = self.step(step);
        }
        last
    }

    /// Advance by `dt` seconds and return the drawable sprites.
    pub fn step(&mut self, dt: f32) -> Vec<Sprite> {
        if !dt.is_finite() || dt < 0.0 {
            return Vec::new();
        }
        // A long stall (a paused daemon, a slow frame) must not spawn a burst of
        // particles, so a frame is clamped to something a frame could plausibly be.
        let dt = dt.min(0.1);
        self.time += dt;

        if self.time >= self.system.starttime {
            self.emit(dt);
        }
        self.apply_operators(dt);
        self.age(dt);
        self.sprites()
    }

    fn emit(&mut self, dt: f32) {
        // Collect spawn orders first so the borrow of `system` ends before
        // touching `self`.
        let mut spawns: Vec<(usize, f32)> = Vec::new();
        for (i, em) in self.system.emitters.iter().enumerate() {
            let rate = em.f32_or("rate", 0.0).max(0.0);
            if rate <= 0.0 && !em.has("instantaneous") {
                // A zero rate with no instantaneous flag still emits one particle
                // in the editor's model for some presets, but the corpus uses
                // rate>0 everywhere; treating zero as "none" is the safe reading.
                self.emit_credit[i] = 0.0;
                continue;
            }
            self.emit_credit[i] += rate * dt;
            let whole = self.emit_credit[i].floor();
            if whole > 0.0 {
                self.emit_credit[i] -= whole;
                spawns.push((i, whole));
            }
        }

        for (i, count) in spawns {
            let em = self.system.emitters[i].clone();
            for _ in 0..(count as usize).min(self.maxcount) {
                if self.particles.len() >= self.maxcount {
                    break;
                }
                if let Some(p) = self.spawn(&em) {
                    self.particles.push(p);
                }
            }
        }
    }

    /// Build one particle from an emitter plus the initializer list.
    fn spawn(&mut self, em: &Item) -> Option<Particle> {
        let origin = em.vec3("origin").unwrap_or([0.0, 0.0, 0.0]);
        let dmin = em.f32_or("distancemin", 0.0);
        let dmax = em.f32_or("distancemax", 0.0);

        // Position: uniformly inside the emitter's shape.
        let pos = if em.name == "boxrandom" {
            // A box of half-extent = distance, direction-skewed by `directions`.
            let dir = em.vec3("directions").unwrap_or([1.0, 1.0, 1.0]);
            let half = [
                dmax * dir[0].abs(),
                dmax * dir[1].abs(),
                dmax * dir[2].abs(),
            ];
            let mut jitter = self.rng.range3([-half[0], -half[1], -half[2]], half);
            jitter[0] += origin[0];
            jitter[1] += origin[1];
            jitter[2] += origin[2];
            jitter
        } else {
            // sphererandom: a random direction, radius in dmin..dmax.
            let dir = random_unit(&mut self.rng);
            let r = self.rng.range(dmin, dmax.max(dmin));
            [
                origin[0] + dir[0] * r,
                origin[1] + dir[1] * r,
                origin[2] + dir[2] * r,
            ]
        };

        let mut p = Particle {
            pos,
            size: 16.0,
            color: [1.0, 1.0, 1.0],
            alpha: 1.0,
            lifetime: 1.0,
            phase: self.rng.unit(),
            seed: self.rng.unit(),
            control: -1,
            ..Default::default()
        };

        // Emitter velocity: a speed along `directions`, when present.
        if let (Some(dir), true) = (em.vec3("directions"), em.has("speedmax")) {
            let speed = self.rng.range(
                em.f32_or("speedmin", 0.0).abs(),
                em.f32_or("speedmax", 0.0).abs(),
            );
            let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2])
                .sqrt()
                .max(1e-6);
            p.vel = [
                dir[0] / len * speed,
                dir[1] / len * speed,
                dir[2] / len * speed,
            ];
        }

        for init in &self.system.initializers {
            apply_initializer(init, &mut p, &mut self.rng);
        }
        p.alpha_base = p.alpha;
        p.size_base = p.size;
        if let Some(cp) = em.f32("controlpoint") {
            p.control = cp as i32;
        }
        Some(p)
    }

    fn apply_operators(&mut self, dt: f32) {
        let ops = self.system.operators.clone();
        let cps = self.control_points.clone();
        for p in &mut self.particles {
            for op in &ops {
                apply_operator(op, p, dt, self.time, &cps);
            }
        }
    }

    fn age(&mut self, dt: f32) {
        let mut kept = Vec::with_capacity(self.particles.len());
        let steps = self
            .system
            .renderers
            .iter()
            .any(|r| r.name == "spritetrail" || r.name == "ropetrail");
        for mut p in self.particles.drain(..) {
            p.age += dt;
            if steps {
                p.trail.push(p.pos);
                // ~1.5 s of history is plenty for the trail lengths in use.
                let cap = 48;
                if p.trail.len() > cap {
                    p.trail.remove(0);
                }
            }
            if p.age < p.lifetime && p.lifetime > 0.0 {
                kept.push(p);
            }
        }
        self.particles = kept;
    }

    fn sprites(&self) -> Vec<Sprite> {
        let trail = self
            .system
            .renderers
            .iter()
            .find(|r| r.name == "spritetrail" || r.name == "ropetrail");
        let mut out =
            Vec::with_capacity(self.particles.len() * if trail.is_some() { 8 } else { 1 });
        for p in &self.particles {
            let alpha = p.alpha.clamp(0.0, 1.0);
            if alpha <= 0.0 || p.size <= 0.0 {
                continue;
            }
            if let Some(r) = trail {
                // A trail is the sprite repeated along its recent path, fading
                // toward the tail. `length` scales the history used.
                let len = (r.f32_or("length", 0.03) * 400.0).clamp(2.0, p.trail.len() as f32);
                let n = len as usize;
                let start = p.trail.len().saturating_sub(n);
                for (k, tpos) in p.trail[start..].iter().enumerate() {
                    let f = (k + 1) as f32 / n.max(1) as f32;
                    out.push(Sprite {
                        pos: *tpos,
                        size: p.size * (0.35 + 0.65 * f),
                        rotation: p.rotation,
                        color: p.color,
                        alpha: alpha * f,
                    });
                }
            } else {
                out.push(Sprite {
                    pos: p.pos,
                    size: p.size,
                    rotation: p.rotation,
                    color: p.color,
                    alpha,
                });
            }
        }
        out
    }
}

fn random_unit(rng: &mut Rng) -> [f32; 3] {
    // Rejection-free: a random direction from two angles plus a sqrt radius
    // keeps it uniform on the sphere.
    let z = rng.range(-1.0, 1.0);
    let t = rng.range(0.0, std::f32::consts::TAU);
    let r = (1.0 - z * z).max(0.0).sqrt();
    [r * t.cos(), r * t.sin(), z]
}

/// `exponent` bends a random value toward one end of its range.
fn with_exponent(rng: &mut Rng, min: f32, max: f32, exponent: f32) -> f32 {
    let u = rng.unit();
    let u = if (exponent - 1.0).abs() > 1e-6 && exponent > 0.0 {
        u.powf(exponent)
    } else {
        u
    };
    min + (max - min) * u
}

fn apply_initializer(init: &Item, p: &mut Particle, rng: &mut Rng) {
    match init.name.as_str() {
        "lifetimerandom" => {
            let [a, b] = init.min_max([1.0, 1.0]);
            p.lifetime = with_exponent(rng, a, b, init.f32_or("exponent", 1.0)).max(0.01);
        }
        "sizerandom" => {
            let [a, b] = init.min_max([16.0, 16.0]);
            p.size = with_exponent(rng, a, b, init.f32_or("exponent", 1.0));
        }
        "colorrandom" => {
            let a = init.vec3("min").unwrap_or([1.0, 1.0, 1.0]);
            let b = init.vec3("max").unwrap_or(a);
            // Colors are 0..255 in the file.
            p.color = [
                rng.range(a[0], b[0]) / 255.0,
                rng.range(a[1], b[1]) / 255.0,
                rng.range(a[2], b[2]) / 255.0,
            ];
        }
        "velocityrandom" => {
            let a = init.vec3("min").unwrap_or([0.0, 0.0, 0.0]);
            let b = init.vec3("max").unwrap_or(a);
            let v = rng.range3(a, b);
            p.vel = [p.vel[0] + v[0], p.vel[1] + v[1], p.vel[2] + v[2]];
        }
        "rotationrandom" => {
            let [a, b] = init.min_max([0.0, 0.0]);
            p.rotation = rng.range(a, b);
        }
        "angularvelocityrandom" => {
            let [a, b] = init.min_max([0.0, 0.0]);
            p.angular_velocity = rng.range(a, b);
        }
        "alpharandom" => {
            let [a, b] = init.min_max([1.0, 1.0]);
            p.alpha = with_exponent(rng, a, b, init.f32_or("exponent", 1.0));
        }
        // Turbulent initial velocity is applied continuously by the matching
        // operator; a nonzero `offset` just seeds the direction.
        "turbulentvelocityrandom" => {
            let off = init.f32_or("offset", 0.0);
            let s = init.f32_or("scale", 0.1);
            p.vel[0] += off * s;
        }
        // These need the control-point sequence machinery; leaving them out
        // changes only which sprite a particle shows.
        _ => {}
    }
}

fn apply_operator(op: &Item, p: &mut Particle, dt: f32, time: f32, cps: &[[f32; 3]]) {
    match op.name.as_str() {
        "movement" => {
            let g = op.vec3("gravity").unwrap_or([0.0, 0.0, 0.0]);
            p.vel[0] += g[0] * dt;
            p.vel[1] += g[1] * dt;
            p.vel[2] += g[2] * dt;
            let drag = op.f32_or("drag", 0.0).max(0.0);
            if drag > 0.0 {
                // Exponential drag is what a per-second coefficient means and
                // stays stable at any frame rate.
                let k = (-drag * dt).exp();
                p.vel[0] *= k;
                p.vel[1] *= k;
                p.vel[2] *= k;
            }
            p.pos[0] += p.vel[0] * dt;
            p.pos[1] += p.vel[1] * dt;
            p.pos[2] += p.vel[2] * dt;
        }
        "alphafade" => {
            let fin = op.f32_or("fadeintime", 0.0).max(0.0);
            let fout = op.f32_or("fadeouttime", 0.0).max(0.0);
            let mut m = 1.0f32;
            if fin > 0.0 {
                m *= (p.age / fin).clamp(0.0, 1.0);
            }
            if fout > 0.0 {
                let tail = (p.lifetime - p.age) / fout;
                m *= tail.clamp(0.0, 1.0);
            }
            // From the base each frame, not from the previous frame's value.
            p.alpha = (p.alpha_base * m.max(0.0)).clamp(0.0, 1.0);
        }
        "alphachange" => {
            let t = op.f32_or("starttime", 0.0);
            if p.age >= t {
                p.alpha = 0.0;
            }
        }
        "sizechange" => {
            // `startvalue`/`endvalue` are multipliers of the initial size; a
            // missing pair leaves the size alone.
            let t0 = op.f32_or("starttime", 0.0);
            let t1 = op.f32_or("endtime", p.lifetime).max(t0 + 1e-4);
            let s0 = op.f32_or("startvalue", 1.0);
            let s1 = op.f32_or("endvalue", 1.0);
            let f = ((p.age - t0) / (t1 - t0)).clamp(0.0, 1.0);
            // Apply once, from the base size, so repeated frames do not compound.
            if !p.has_size_base {
                p.size_base = p.size;
                p.has_size_base = true;
            }
            p.size = p.size_base * (s0 + (s1 - s0) * f);
        }
        "colorchange" => {
            let t0 = op.f32_or("starttime", 0.0);
            let t1 = op.f32_or("endtime", p.lifetime).max(t0 + 1e-4);
            let c0 = op.vec3("startvalue").unwrap_or([1.0, 1.0, 1.0]);
            let c1 = op.vec3("endvalue").unwrap_or(c0);
            let f = ((p.age - t0) / (t1 - t0)).clamp(0.0, 1.0);
            for i in 0..3 {
                p.color[i] = (c0[i] + (c1[i] - c0[i]) * f) / 255.0;
            }
        }
        "oscillatealpha" => {
            let scale = oscillate_scale(op, p, time);
            p.alpha = (p.alpha_base * scale).clamp(0.0, 1.0);
        }
        "oscillatesize" => {
            let scale = oscillate_scale(op, p, time);
            if !p.has_size_base {
                p.size_base = p.size;
                p.has_size_base = true;
            }
            p.size = p.size_base * scale;
        }
        "oscillateposition" => {
            let scale = oscillate_scale(op, p, time);
            let mask = op.vec3("mask").unwrap_or([1.0, 1.0, 1.0]);
            p.pos[0] += mask[0] * scale * 10.0;
            p.pos[1] += mask[1] * scale * 10.0;
        }
        "angularmovement" => {
            let force = op.f32_or("force", 0.0);
            p.angular_velocity += force * dt;
            let drag = op.f32_or("drag", 0.0).max(0.0);
            if drag > 0.0 {
                p.angular_velocity *= (-drag * dt).exp();
            }
            p.rotation += p.angular_velocity * dt;
        }
        "turbulence" => {
            // A cheap curl-like wobble: two out-of-phase sinusoids per axis,
            // seeded per particle so neighbours do not move in lockstep.
            let scale = op.f32_or("scale", 0.002);
            let speed = op.f32_or("speedmax", 100.0);
            let mask = op.vec3("mask").unwrap_or([1.0, 1.0, 1.0]);
            let t = time * op.f32_or("timescale", 1.0);
            let a = t * 0.9 + p.seed * std::f32::consts::TAU;
            let b = t * 1.3 + p.phase * std::f32::consts::TAU;
            p.vel[0] += mask[0] * a.sin() * speed * scale * dt;
            p.vel[1] += mask[1] * b.sin() * speed * scale * dt;
            p.vel[2] += mask[2] * (a + b).sin() * speed * scale * 0.5 * dt;
        }
        "vortex" => {
            let axis = op.vec3("axis").unwrap_or([0.0, 0.0, 1.0]);
            let inner = op.f32_or("distanceinner", 0.0);
            let outer = op.f32_or("distanceouter", 1.0).max(inner + 1e-3);
            let speed = op.f32_or("speedouter", 0.0);
            let r = (p.pos[0] * p.pos[0] + p.pos[1] * p.pos[1]).sqrt();
            if r > inner && r < outer && axis[2].abs() > 0.5 {
                // 2D swirl about z.
                let f = (speed * dt) / r.max(1e-3);
                let (x, y) = (p.pos[0], p.pos[1]);
                p.pos[0] += -y * f;
                p.pos[1] += x * f;
            }
        }
        "controlpointattract" => {
            let cp = op.f32("controlpoint").unwrap_or(-1.0) as i32;
            if cp >= 0 {
                if let Some(target) = cps.get(cp as usize) {
                    let origin = op.vec3("origin").unwrap_or([0.0, 0.0, 0.0]);
                    let scale = op.f32_or("scale", 1.0);
                    for i in 0..3 {
                        p.pos[i] += (target[i] + origin[i] - p.pos[i]) * scale * dt;
                    }
                }
            }
        }
        _ => {}
    }
}

/// The shared oscillator used by `oscillate*`: a sine between `scalemin` and
/// `scalemax`, at a random frequency in `frequencymin..frequencymax`.
fn oscillate_scale(op: &Item, p: &Particle, time: f32) -> f32 {
    let fmin = op.f32_or("frequencymin", 1.0);
    let fmax = op.f32_or("frequencymax", 1.0);
    let smin = op.f32_or("scalemin", 1.0);
    let smax = op.f32_or("scalemax", 1.0);
    let fmul = op.f32_or("phasemax", 1.0);
    let phase = op.f32_or("phasemin", 0.0) + p.phase * fmul;
    let freq = fmin + (fmax - fmin) * p.phase;
    let s = (time * freq * std::f32::consts::TAU + phase).sin() * 0.5 + 0.5;
    smin + (smax - smin) * s
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprwpe_core::particle::parse;

    fn fog() -> System {
        parse(
            r#"{
            "emitter": [{"name":"sphererandom","rate":10,"distancemin":0,"distancemax":100,
                         "origin":"0 0 0","directions":"0 0 0"}],
            "initializer": [
                {"name":"lifetimerandom","min":1,"max":1},
                {"name":"sizerandom","min":20,"max":20},
                {"name":"colorrandom","min":"255 255 255","max":"255 255 255"},
                {"name":"alpharandom","min":0.5,"max":0.5}
            ],
            "operator": [{"name":"movement","gravity":"0 0 0"}],
            "renderer": [{"name":"sprite"}],
            "maxcount": 50
        }"#,
        )
        .unwrap()
    }

    /// Step in frame-sized increments, the way the daemon drives it. A single
    /// large `dt` is deliberately clamped, so tests must advance like realtime.
    fn run(sim: &mut Sim, seconds: f32) -> Vec<Sprite> {
        let dt = 1.0 / 60.0;
        let steps = (seconds / dt).round() as usize;
        let mut last = Vec::new();
        for _ in 0..steps {
            last = sim.step(dt);
        }
        last
    }

    #[test]
    fn emission_respects_the_rate_and_the_cap() {
        let mut sim = Sim::new(fog(), vec![], 1);
        // 10/s for 1s -> ~10 particles, never more than maxcount.
        let sprites = run(&mut sim, 1.0);
        assert!(!sprites.is_empty(), "a 10/s emitter must produce particles");
        assert!(sim.live() <= 50);

        let mut fast = fog();
        fast.maxcount = 5;
        let mut sim = Sim::new(fast, vec![], 1);
        run(&mut sim, 1.0);
        assert!(sim.live() <= 5, "the cap must bind: {}", sim.live());
    }

    #[test]
    fn particles_expire_so_the_population_reaches_a_steady_state() {
        // With a rate and a lifetime, a correct simulation settles at about
        // `rate * lifetime` particles. Without expiry it would grow until the
        // cap and stay there, so the cap doubling as the failing bound makes
        // this a real discriminator rather than a smoke test.
        let sys = parse(
            r#"{
            "emitter": [{"name":"sphererandom","rate":100,"distancemax":10}],
            "initializer": [{"name":"lifetimerandom","min":1,"max":1},
                            {"name":"sizerandom","min":10,"max":10}],
            "operator": [{"name":"movement","gravity":"0 0 0"}],
            "renderer": [{"name":"sprite"}], "maxcount": 200
        }"#,
        )
        .unwrap();
        let mut sim = Sim::new(sys, vec![], 1);
        run(&mut sim, 3.0);
        let live = sim.live();
        assert!(live > 0, "an emitter must keep particles alive");
        assert!(
            live < 200,
            "without expiry the population would pin the cap, got {live}"
        );
    }

    #[test]
    fn initializers_set_size_colour_and_alpha() {
        let mut sim = Sim::new(fog(), vec![], 1);
        let sprites = run(&mut sim, 0.5);
        let s = sprites[0];
        assert!((s.size - 20.0).abs() < 1e-3, "size {}", s.size);
        assert!((s.color[0] - 1.0).abs() < 1e-3, "colour {:?}", s.color);
        assert!((s.alpha - 0.5).abs() < 1e-3, "alpha {}", s.alpha);
    }

    #[test]
    fn alphafade_reaches_zero_at_the_end_of_life() {
        let sys = parse(
            r#"{
            "emitter": [{"name":"sphererandom","rate":100,"distancemax":10}],
            "initializer": [{"name":"lifetimerandom","min":1,"max":1},
                            {"name":"sizerandom","min":10,"max":10}],
            "operator": [{"name":"movement","gravity":"0 0 0"},
                         {"name":"alphafade","fadeintime":0.1,"fadeouttime":0.5}],
            "renderer": [{"name":"sprite"}], "maxcount": 200
        }"#,
        )
        .unwrap();
        let mut sim = Sim::new(sys, vec![], 1);
        // A particle is born with alpha 0 (the fade-in has not run yet) and is
        // then culled for a frame, so sample a moment later where the young
        // particles have begun to ramp up but are still well under full.
        let early = run(&mut sim, 0.06);
        assert!(
            early.iter().any(|s| s.alpha < 0.5),
            "fade-in must ramp alpha up from zero"
        );
        // Late in life the fade-out bites.
        let late = run(&mut sim, 0.9);
        assert!(
            late.iter().any(|s| s.alpha < 0.5),
            "fadeout must reduce alpha"
        );
    }

    #[test]
    fn movement_applies_gravity_over_time() {
        let sys = parse(
            r#"{
            "emitter": [{"name":"sphererandom","rate":100,"distancemax":0}],
            "initializer": [{"name":"lifetimerandom","min":5,"max":5},
                            {"name":"sizerandom","min":10,"max":10}],
            "operator": [{"name":"movement","gravity":"0 -100 0"}],
            "renderer": [{"name":"sprite"}], "maxcount": 200
        }"#,
        )
        .unwrap();
        let mut sim = Sim::new(sys, vec![], 1);
        let first = run(&mut sim, 0.05);
        let y0 = first[0].pos[1];
        let later = run(&mut sim, 0.5);
        let y1 = later[0].pos[1];
        assert!(y1 < y0, "gravity must pull down: {y0} -> {y1}");
    }

    #[test]
    fn simulation_is_deterministic_for_a_seed() {
        let mut a = Sim::new(fog(), vec![], 7);
        let mut b = Sim::new(fog(), vec![], 7);
        let sa = run(&mut a, 0.5);
        let sb = run(&mut b, 0.5);
        assert_eq!(sa.len(), sb.len());
        for (x, y) in sa.iter().zip(sb.iter()) {
            assert!((x.pos[0] - y.pos[0]).abs() < 1e-6);
            assert!((x.alpha - y.alpha).abs() < 1e-6);
        }
    }

    #[test]
    fn an_unknown_behaviour_is_skipped_not_fatal() {
        let sys = parse(
            r#"{
            "emitter": [{"name":"sphererandom","rate":10,"distancemax":5}],
            "initializer": [{"name":"lifetimerandom","min":1,"max":1},
                            {"name":"quantum_entangle","min":1,"max":2}],
            "operator": [{"name":"movement","gravity":"0 0 0"},
                         {"name":"teleport_now","x":1}],
            "renderer": [{"name":"sprite"}]
        }"#,
        )
        .unwrap();
        let mut sim = Sim::new(sys, vec![], 1);
        assert!(
            !run(&mut sim, 0.5).is_empty(),
            "unknown behaviours must not stop it"
        );
    }

    #[test]
    fn a_system_waits_for_its_start_time() {
        let sys = parse(
            r#"{"emitter":[{"name":"sphererandom","rate":10,"distancemax":1}],
                "initializer":[{"name":"lifetimerandom","min":5,"max":5}],
                "renderer":[{"name":"sprite"}], "starttime": 2}"#,
        )
        .unwrap();
        let mut sim = Sim::new(sys, vec![], 1);
        assert!(run(&mut sim, 1.0).is_empty(), "nothing before starttime");
        assert!(!run(&mut sim, 1.5).is_empty(), "particles after starttime");
    }
}
