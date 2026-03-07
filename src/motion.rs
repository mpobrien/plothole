use crate::font::Path as RawPath;
use crate::font::Vec2d as RawVec2d;

pub type Vec2d = RawVec2d<f64>;
type Path = RawPath<f64>;

#[allow(dead_code)]
enum Motion {
    PenRaise,
    PenDrop,
    Step { x: f64, y: f64 },
}

/// A line connecting two points, which will be broken into blocks by the planner
#[allow(dead_code)]
struct Segment<'a> {
    p1: &'a Vec2d,
    p2: &'a Vec2d,
}

impl<'a> Segment<'a> {
    pub fn new(p1: &'a Vec2d, p2: &'a Vec2d) -> Self {
        Self { p1, p2 }
    }
}

/// A primitive that describes motion of constant acceleration between two points for a finite duration.
/// The planner decomposes a path into a sequence of blocks, each with a fixed acceleration vector applied for
/// a fixed duration starting from `initial_velocity`.
#[allow(dead_code)]
struct Block {
    acceleration: Vec2d,
    duration: std::time::Duration,
    initial_velocity: Vec2d,
    start_position: Vec2d,
    end_position: Vec2d,
    distance_m: Option<f64>,
}

/// The kinematic state of a block at a specific instant in time.
pub struct Instant {
    /// Time elapsed since the start of the motion plan (seconds).
    pub t: f64,
    pub position: Vec2d,
    /// Distance traveled along the motion plan (meters).
    pub distance_m: f64,
    /// Signed speed along the segment direction (m/s).
    pub velocity: f64,
    /// Signed acceleration along the segment direction (m/s²).
    pub acceleration: f64,
}

#[allow(dead_code)]
impl Block {
    /// Returns the kinematic state at time `t` within this block.
    ///
    /// `dt` and `ds` are offsets accumulated from preceding blocks in the plan,
    /// added to the output `t` and `distance_m` so the caller gets plan-relative values.
    fn instant(&self, t: f64, dt: f64, ds: f64) -> Instant {
        let duration_s = self.duration.as_secs_f64();
        let t_clamped = t.max(0.0).min(duration_s);

        let total_distance = self.distance_m.unwrap_or(0.0);
        let length = self.start_position.distance(&self.end_position);

        // Project the Vec2d acceleration and initial velocity onto the segment
        // direction to recover their signed scalar magnitudes.
        let (a, vi) = if length < EPSILON {
            (0.0, 0.0)
        } else {
            let dx = (self.end_position.x - self.start_position.x) / length;
            let dy = (self.end_position.y - self.start_position.y) / length;
            let a = self.acceleration.x * dx + self.acceleration.y * dy;
            let vi = self.initial_velocity.x * dx + self.initial_velocity.y * dy;
            (a, vi)
        };

        let v = vi + a * t_clamped;
        let s = (vi * t_clamped + a * t_clamped * t_clamped / 2.0)
            .max(0.0)
            .min(total_distance);

        // Interpolate position along the segment by distance traveled.
        let frac = if total_distance < EPSILON {
            0.0
        } else {
            s / total_distance
        };
        let position = Vec2d::new(
            self.start_position.x + frac * (self.end_position.x - self.start_position.x),
            self.start_position.y + frac * (self.end_position.y - self.start_position.y),
        );

        Instant {
            t: t_clamped + dt,
            position,
            distance_m: s + ds,
            velocity: v,
            acceleration: a,
        }
    }
}

struct Throttler<'a> {
    points: &'a Vec<Vec2d>,
    max_velocity_mps: f64,
    time_slice: std::time::Duration,
    threshold_m: f64,
    distances_m: Vec<f64>,
}

impl<'a> Throttler<'a> {
    pub fn new(
        points: &'a Vec<Vec2d>,
        max_velocity_mps: f64,
        time_slice: std::time::Duration,
        threshold_m: f64,
    ) -> Self {
        let distances: Vec<_> = points
            .windows(2)
            .map(|window| window[0].distance(&window[1]))
            .collect();
        Self {
            points,
            max_velocity_mps,
            time_slice,
            threshold_m,
            distances_m: distances,
        }
    }
}

const EPSILON: f64 = 1e-9;
const TIMESLICE_MS: u32 = 10;

pub struct AccelerationProfile {
    pub acceleration: f64,
    pub maximum_velocity: f64,
    /// Controls how fast the plotter can take corners. Higher = faster cornering.
    pub cornering_factor: f64,
}

/// Internal per-segment state during planning. Unlike `Segment`, this owns its
/// points and carries mutable planning fields updated during the forward/backtrack passes.
struct PlanSegment {
    p1: Vec2d,
    p2: Vec2d,
    length: f64,
    max_entry_velocity: f64,
    entry_velocity: f64,
    blocks: Vec<Block>,
}

impl PlanSegment {
    fn new(p1: Vec2d, p2: Vec2d) -> Self {
        let length = p1.distance(&p2);
        Self {
            p1,
            p2,
            length,
            max_entry_velocity: f64::INFINITY,
            entry_velocity: 0.0,
            blocks: vec![],
        }
    }

    fn unit_dir(&self) -> (f64, f64) {
        if self.length < EPSILON {
            (0.0, 0.0)
        } else {
            (
                (self.p2.x - self.p1.x) / self.length,
                (self.p2.y - self.p1.y) / self.length,
            )
        }
    }

    fn point_at_distance(&self, d: f64) -> Vec2d {
        let (dx, dy) = self.unit_dir();
        Vec2d::new(self.p1.x + dx * d, self.p1.y + dy * d)
    }
}

/// Returns `points` with consecutive duplicates removed. Two points are considered
/// duplicates if their distance is less than `epsilon`.
fn dedup_points(points: &[Vec2d], epsilon: f64) -> Vec<Vec2d> {
    let mut out: Vec<Vec2d> = vec![];
    for p in points {
        if out
            .last()
            .map_or(true, |last: &Vec2d| last.distance(p) > epsilon)
        {
            out.push(p.clone());
        }
    }
    out
}

/// Maximum entry speed into a corner using the Grbl-style junction-deviation formula.
/// cornering_factor is a tuning constant (units: meters); higher = faster through corners.
fn corner_velocity(
    seg1: &PlanSegment,
    seg2: &PlanSegment,
    v_max: f64,
    accel: f64,
    cornering_factor: f64,
) -> f64 {
    let (dx1, dy1) = seg1.unit_dir();
    let (dx2, dy2) = seg2.unit_dir();
    let cos_theta = dx1 * dx2 + dy1 * dy2;
    let sin_half = ((1.0 - cos_theta) / 2.0).sqrt();
    if sin_half < EPSILON {
        v_max // Straight line, no cornering limit.
    } else {
        (accel * cornering_factor / sin_half).sqrt().min(v_max)
    }
}

struct TriangleResult {
    /// Accel-phase distance. Negative means we entered too fast and must backtrack.
    s1: f64,
    /// Decel-phase distance. <=0 means no room to decelerate; just accelerate through.
    s2: f64,
    v_peak: f64,
    t1: f64,
    t2: f64,
    p1: Vec2d,
    p2: Vec2d, // apex
    p3: Vec2d,
}

/// Compute the triangle (accel → decel) motion profile for one segment.
/// s1 + s2 = distance by construction.
fn compute_triangle(
    distance: f64,
    v_initial: f64,
    v_exit: f64,
    accel: f64,
    p1: &Vec2d,
    p2: &Vec2d,
) -> TriangleResult {
    // From kinematics on both phases and s1+s2=distance:
    //   v_peak² = (vi² + ve² + 2·a·d) / 2
    let v_peak_sq = (v_initial * v_initial + v_exit * v_exit + 2.0 * accel * distance) / 2.0;
    let v_peak = v_peak_sq.max(0.0).sqrt();

    let s1 = (v_peak * v_peak - v_initial * v_initial) / (2.0 * accel);
    let s2 = (v_peak * v_peak - v_exit * v_exit) / (2.0 * accel);
    let t1 = (v_peak - v_initial) / accel;
    let t2 = (v_peak - v_exit) / accel;

    let seg = PlanSegment::new(p1.clone(), p2.clone());
    let apex = seg.point_at_distance(s1.max(0.0));

    TriangleResult {
        s1,
        s2,
        v_peak,
        t1,
        t2,
        p1: p1.clone(),
        p2: apex,
        p3: p2.clone(),
    }
}

struct TrapezoidResult {
    t1: f64,
    t2: f64,
    t3: f64,
    p1: Vec2d,
    p2: Vec2d,
    p3: Vec2d,
    p4: Vec2d,
}

/// Compute the trapezoid (accel → cruise → decel) motion profile for one segment.
/// Used when the triangle peak velocity would exceed `v_cruise` (i.e. `v_max`).
fn compute_trapezoid(
    distance: f64,
    v_initial: f64,
    v_cruise: f64,
    v_exit: f64,
    accel: f64,
    p1: &Vec2d,
    p2: &Vec2d,
) -> TrapezoidResult {
    let s_accel = (v_cruise * v_cruise - v_initial * v_initial) / (2.0 * accel);
    let s_decel = (v_cruise * v_cruise - v_exit * v_exit) / (2.0 * accel);
    let s_cruise = distance - s_accel - s_decel;

    let t1 = (v_cruise - v_initial) / accel;
    let t2 = s_cruise / v_cruise;
    let t3 = (v_cruise - v_exit) / accel;

    let seg = PlanSegment::new(p1.clone(), p2.clone());
    let pp2 = seg.point_at_distance(s_accel);
    let pp3 = seg.point_at_distance(s_accel + s_cruise);

    TrapezoidResult {
        t1,
        t2,
        t3,
        p1: p1.clone(),
        p2: pp2,
        p3: pp3,
        p4: p2.clone(),
    }
}

/// Build a Block from scalar speed/accel values, projecting onto the segment direction.
fn make_block(accel_scalar: f64, t: f64, v_initial_scalar: f64, p1: Vec2d, p2: Vec2d) -> Block {
    let length = p1.distance(&p2);
    let (dx, dy) = if length < EPSILON {
        (0.0, 0.0)
    } else {
        ((p2.x - p1.x) / length, (p2.y - p1.y) / length)
    };
    Block {
        acceleration: Vec2d::new(dx * accel_scalar, dy * accel_scalar),
        duration: std::time::Duration::from_secs_f64(t.max(0.0)),
        initial_velocity: Vec2d::new(dx * v_initial_scalar, dy * v_initial_scalar),
        start_position: p1,
        end_position: p2,
        distance_m: Some(length),
    }
}

struct Planner {}

impl Planner {
    /// Plan a constant-acceleration motion profile for a sequence of points.
    ///
    /// 1. Removes duplicate points.
    /// 2. Caps entry speed at each corner based on the turn angle.
    /// 3. Iterates forward, choosing triangle (accel→decel) or trapezoid
    ///    (accel→cruise→decel) profiles per segment, backtracking when a
    ///    segment was entered too fast to exit at the required speed.
    pub fn plan(&self, points: &[Vec2d], profile: &AccelerationProfile) -> Vec<Block> {
        let deduped = dedup_points(points, EPSILON);

        if deduped.len() <= 1 {
            return vec![];
        }

        let mut segments: Vec<PlanSegment> = deduped
            .windows(2)
            .map(|w| PlanSegment::new(w[0].clone(), w[1].clone()))
            .collect();

        let accel = profile.acceleration;
        let v_max = profile.maximum_velocity;
        let cornering = profile.cornering_factor;

        // Set max entry velocity at each interior corner based on angle.
        for i in 1..segments.len() {
            let cv = corner_velocity(&segments[i - 1], &segments[i], v_max, accel, cornering);
            segments[i].max_entry_velocity = cv;
        }

        // Sentinel: zero-length segment forces exit velocity to zero at the end.
        // max_entry_velocity must be 0 — if left as INFINITY it propagates NaN
        // into the trapezoid calculation via INFINITY - INFINITY.
        let last = deduped.last().unwrap().clone();
        let mut sentinel = PlanSegment::new(last.clone(), last);
        sentinel.max_entry_velocity = 0.0;
        segments.push(sentinel);

        let mut i: usize = 0;
        while i < segments.len() - 1 {
            let distance = segments[i].length;
            let v_initial = segments[i].entry_velocity;
            let v_exit = segments[i + 1].max_entry_velocity;
            let p1 = segments[i].p1.clone();
            let p2 = segments[i].p2.clone();

            let m = compute_triangle(distance, v_initial, v_exit, accel, &p1, &p2);

            if m.s1 < -EPSILON {
                // We'd have to start decelerating before this segment even begins.
                // Reduce the max entry velocity and backtrack to replan the previous segment.
                let new_max = (v_exit * v_exit + 2.0 * accel * distance).sqrt();
                segments[i].max_entry_velocity = new_max;
                if i > 0 {
                    i -= 1;
                }
            } else if m.s2 <= 0.0 {
                // No room to decelerate — just accelerate the whole segment.
                let v_final = (v_initial * v_initial + 2.0 * accel * distance).sqrt();
                let t = (v_final - v_initial) / accel;
                segments[i].blocks = vec![make_block(accel, t, v_initial, p1, p2)];
                segments[i + 1].entry_velocity = v_final;
                i += 1;
            } else if m.v_peak > v_max {
                // Triangle profile exceeds v_max — top out at v_max (trapezoid).
                let z = compute_trapezoid(distance, v_initial, v_max, v_exit, accel, &p1, &p2);
                segments[i].blocks = vec![
                    make_block(accel, z.t1, v_initial, z.p1, z.p2.clone()),
                    make_block(0.0, z.t2, v_max, z.p2.clone(), z.p3.clone()),
                    make_block(-accel, z.t3, v_max, z.p3, z.p4),
                ];
                segments[i + 1].entry_velocity = v_exit;
                i += 1;
            } else {
                // Triangle: accelerate to v_peak, then decelerate to v_exit.
                segments[i].blocks = vec![
                    make_block(accel, m.t1, v_initial, m.p1, m.p2.clone()),
                    make_block(-accel, m.t2, m.v_peak, m.p2, m.p3),
                ];
                segments[i + 1].entry_velocity = v_exit;
                i += 1;
            }
        }

        segments
            .into_iter()
            .flat_map(|seg| seg.blocks)
            .filter(|b| b.duration.as_secs_f64() > EPSILON)
            .collect()
    }
}

/// A complete motion plan: an ordered sequence of [`Block`]s that together
/// describe a continuous constant-acceleration trajectory.
pub struct Plan {
    blocks: Vec<Block>,
}

impl Plan {
    fn new(blocks: Vec<Block>) -> Self {
        Self { blocks }
    }

    /// Total duration of the plan in seconds.
    pub fn duration(&self) -> f64 {
        self.blocks.iter().map(|b| b.duration.as_secs_f64()).sum()
    }

    /// Returns the kinematic state at plan-relative time `t`.
    ///
    /// Finds the block that owns `t` and delegates to [`Block::instant`],
    /// passing in the accumulated time and distance offsets from all preceding blocks.
    pub fn instant(&self, t: f64) -> Instant {
        let mut accumulated_t = 0.0;
        let mut accumulated_s = 0.0;
        for (i, block) in self.blocks.iter().enumerate() {
            let block_duration = block.duration.as_secs_f64();
            let is_last = i == self.blocks.len() - 1;
            if t < accumulated_t + block_duration || is_last {
                return block.instant(t - accumulated_t, accumulated_t, accumulated_s);
            }
            accumulated_t += block_duration;
            accumulated_s += block.distance_m.unwrap_or(0.0);
        }
        // Empty plan.
        Instant {
            t: 0.0,
            position: Vec2d::new(0.0, 0.0),
            distance_m: 0.0,
            velocity: 0.0,
            acceleration: 0.0,
        }
    }
}

/// Execute a motion plan by sampling it at [`TIMESLICE_MS`] intervals.
///
/// `steps_per_unit` converts meters to stepper motor steps.
/// `error` carries the sub-step fractional remainder across slices so that
/// rounding errors accumulate correctly rather than being dropped.
/// Returns one [`Motion::Step`] per time slice.
#[allow(dead_code)]
fn run_plan(plan: &Plan, steps_per_unit: f64, error: &mut (f64, f64)) -> Vec<Motion> {
    let step_s = TIMESLICE_MS as f64 / 1000.0;
    let mut t = 0.0;
    let total = plan.duration();
    let mut out = vec![];

    while t < total {
        let i1 = plan.instant(t);
        let i2 = plan.instant(t + step_s);

        let dx = i2.position.x - i1.position.x;
        let dy = i2.position.y - i1.position.y;

        let total_x = dx * steps_per_unit + error.0;
        let total_y = dy * steps_per_unit + error.1;

        error.0 = total_x.fract();
        error.1 = total_y.fract();

        out.push(Motion::Step { x: total_x.trunc(), y: total_y.trunc() });
        t += step_s;
    }

    out
}

/// Plan a constant-acceleration trajectory through `points` and return the resulting [`Plan`].
///
/// This is the public entry point for the motion planner. `points` should be in
/// whatever coordinate units the caller uses; `profile` values must be in the same units.
pub fn plan_path(points: &[Vec2d], profile: &AccelerationProfile) -> Plan {
    Plan::new(Planner {}.plan(points, profile))
}

/*
function constantAccelerationPlan(points: Vec2[], profile: AccelerationProfile): XYMotion {
  const dedupedPoints = dedupPoints(points, epsilon);
  if (dedupedPoints.length === 1) {
    return new XYMotion([new Block(0, 0, 0, dedupedPoints[0], dedupedPoints[0])]);
  }
  const segments = dedupedPoints.slice(1).map((a, i) => new Segment(dedupedPoints[i], a));

  const accel = profile.acceleration;
  const vMax = profile.maximumVelocity;
  const cornerFactor = profile.corneringFactor;

  // Calculate the maximum entry velocity for each segment based on the angle between it
  // and the previous segment.
  segments.slice(1).forEach((seg2, i) => {
    const seg1 = segments[i];
    seg2.maxEntryVelocity = cornerVelocity(seg1, seg2, vMax, accel, cornerFactor);
  });

  // This is to force the velocity to zero at the end of the path.
  const lastPoint = dedupedPoints[dedupedPoints.length - 1];
  segments.push(new Segment(lastPoint, lastPoint));

  let i = 0;
  while (i < segments.length - 1) {
    const segment = segments[i];
    const nextSegment = segments[i + 1];
    const distance = segment.length();
    const vInitial = segment.entryVelocity;
    const vExit = nextSegment.maxEntryVelocity;
    const p1 = segment.p1;
    const p2 = segment.p2;

    const m = computeTriangle(distance, vInitial, vExit, accel, p1, p2);
    if (m.s1 < -epsilon) {
      // We'd have to start decelerating _before we started on this segment_. backtrack.
      // In order enter this segment slow enough to be leaving it at vExit, we need to
      // compute a maximum entry velocity s.t. we can slow down in the distance we have.
      // TODO: verify this equation.
      segment.maxEntryVelocity = Math.sqrt(vExit * vExit + 2 * accel * distance);
      i -= 1;
    } else if (m.s2 <= 0) {
      // No deceleration.
      // TODO: shouldn't we check vMax here and maybe do trapezoid? should the next case below come first?
      const vFinal = Math.sqrt(vInitial * vInitial + 2 * accel * distance);
      const t = (vFinal - vInitial) / accel;
      segment.blocks = [new Block(accel, t, vInitial, p1, p2)];
      nextSegment.entryVelocity = vFinal;
      i += 1;
    } else if (m.vMax > vMax) {
      // Triangle profile would exceed maximum velocity, so top out at vMax.
      const z = computeTrapezoid(distance, vInitial, vMax, vExit, accel, p1, p2);
      segment.blocks = [
        new Block(accel, z.t1, vInitial, z.p1, z.p2),
        new Block(0, z.t2, vMax, z.p2, z.p3),
        new Block(-accel, z.t3, vMax, z.p3, z.p4),
      ];
      nextSegment.entryVelocity = vExit;
      i += 1;
    } else {
      // Accelerate, then decelerate.
      segment.blocks = [new Block(accel, m.t1, vInitial, m.p1, m.p2), new Block(-accel, m.t2, m.vMax, m.p2, m.p3)];
      nextSegment.entryVelocity = vExit;
      i += 1;
    }
  }
  const blocks: Block[] = [];
  for (const segment of segments) {
    for (const block of segment.blocks) {
      if (block.duration > epsilon) {
        blocks.push(block);
      }
    }
  }
  return new XYMotion(blocks);
}
* */

#[allow(dead_code)]
fn plan(
    drawing: Vec<Path>,
    _steps_per_revolution: u64,
    profile: &AccelerationProfile,
) -> Vec<Motion> {
    let mut out: Vec<Motion> = vec![];
    let mut start_position_m: Vec2d = Vec2d::new(0.0, 0.0);
    let planner = Planner {};

    for path in drawing {
        // Plan the pen-up move to the start of this path.
        let _move_to_start_blocks =
            planner.plan(&[start_position_m, path.start().clone()], profile);
        // TODO: convert _move_to_start_blocks into Motion::Step values

        // Drop the pen to start drawing.
        out.push(Motion::PenDrop);

        // Plan the drawing path.
        let _draw_blocks = planner.plan(path.points(), profile);
        // TODO: convert _draw_blocks into Motion::Step values

        // Raise the pen so we can move to the next path.
        out.push(Motion::PenRaise);

        start_position_m = path.end().clone();
    }
    out
}
