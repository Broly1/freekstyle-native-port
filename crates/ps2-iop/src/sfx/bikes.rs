//! Bike engines, boost, terrain, bumps and skids as the driver handles them.
//! The bike record and engine rules are in docs/native-audio.md.

use super::{fixed, i32_at, vec3, Placed, Sfx, ONE};

pub const MAX_BIKES: usize = 8;
const RECORDS: usize = 0x2040;
const RECORD: usize = 0x40;

/// Sound ids of the engine samples (0-11), then the others the driver needs
/// before it plays any bike sound. Index 14 is the boost sound.
const ENGINE_IDS: [u16; 18] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 0xe3, 0x143, 0xf5, 0x10, 0xe4];
const BOOST: usize = 14;
/// The surface whose rolling sound is not turned down.
const LOUD_SURFACE_ID: u16 = 0x84;

/// Rolling and bump sound id per terrain (0-28).
const SURFACES: [(u16, u16); 29] = [
    (128, 0), (133, 0), (132, 0), (129, 0), (129, 0), (132, 0), (131, 0), (137, 0), (133, 0), (134, 0),
    (128, 0), (130, 0), (138, 0), (128, 0), (129, 0), (0, 0), (128, 0), (130, 166), (0, 0), (136, 0),
    (129, 0), (132, 0), (134, 0), (130, 0), (129, 0), (0, 0), (140, 0), (130, 0), (129, 0),
];
/// Engine pitch added per terrain, in hundredths.
const TERRAIN_PITCH: [i32; 29] = [0, 8, 0, 0, 10, 15, 8, 8, 0, 15, 25, 10, 8, 0, 12, 8, 0, 8, 0, 8, 15, 30, -8, 8, 20, 15, 8, 8, 20];
const AIRBORNE: i8 = -1;

// The driver's tuning, from its data at 0xd588.
const BOOST_VOLUME: f64 = 0xacdc as f64 / ONE as f64;
const SURFACE_VOLUME: f64 = 0x7ae1 as f64 / ONE as f64;
const BOOST_PITCH: f64 = 0.75;
const SKID_PITCH_SLOPE: f64 = 0x1a as f64 / ONE as f64;
const SKID_START: u16 = 4;
const FORCED_SKID_PITCH: f64 = 1.0 / 16.0;
const NEAR: f64 = 0xf000 as f64 / ONE as f64;
const FAR: f64 = 0x32000 as f64 / ONE as f64;
const FAR_LEVEL: f64 = 0xdb6d as f64 / ONE as f64;
const NEAR_SLOPE: f64 = 0x1d41 as f64 / ONE as f64;
const FAR_SLOPE: f64 = 0.5;
/// Bikes nobody is riding sound at half level.
const OTHER_BIKE: f64 = 0.5;
const AIR_PITCH: f64 = 0x332c as f64 / ONE as f64;
const BOOST_PITCH_UP: f64 = 0x2661 as f64 / ONE as f64;
const BUMP_GAP: (i32, i32) = (0x300, 0xa00);

#[derive(Default, Clone)]
pub struct Bike {
    set: i8,
    rider: i8,
    flags: u8,
    /// Engine state (see the doc's table) and its sample.
    state: u8,
    ready: bool,
    terrain: i8,
    last_terrain: i8,
    surface: Option<usize>,
    bump: Option<usize>,
    pos: [f64; 3],
    speed: i16,
    revs: f64,
    silent: bool,
    restart: u8,
    gas: bool,
    gas_changed: bool,
    boost: bool,
    boost_changed: bool,
    gas_frames: u32,
    travelled: i32,
    last_bump: i32,
    skid_amount: u8,
    skid_input: u8,
    forced_skid: bool,
    skid: u16,
    distance: f64,
    engine: Option<u64>,
    rolling: Option<u64>,
    skid_voice: Option<u64>,
    boost_voice: Option<u64>,
}

impl Bike {
    /// Ridden by a player on this console.
    fn local(&self) -> bool {
        self.rider >= 0 || self.flags & 0x40 != 0
    }

    fn voices(&mut self) -> [&mut Option<u64>; 4] {
        [&mut self.engine, &mut self.rolling, &mut self.skid_voice, &mut self.boost_voice]
    }

    /// Picks the engine sound.
    fn sample(&self, roll: u32) -> usize {
        let s = match self.state {
            0 => 5,
            1 | 2 if self.restart >= 2 => 4,
            1..=3 => 0,
            4 | 10 => 4,
            5 | 9 => 1,
            6 => 2,
            7 | 8 => if roll < 95 { 3 } else { 2 },
            11 => if roll < 25 { 3 } else { 2 },
            _ => 0,
        };
        if self.set == 2 { s + 6 } else { s }
    }
}

#[derive(Default)]
pub struct Bikes {
    pub list: Vec<Bike>,
    /// Setup entry (op 0x13) byte +4: when 0 a local bike's engine plays at double level.
    quiet_local: bool,
    cycles: u32,
}

impl Sfx {
    /// Op 0x13: the bikes of a new race.
    pub(super) fn bikes_init(&mut self, e: &[u8]) {
        let count = (e[2] as usize).min(MAX_BIKES);
        self.bikes.quiet_local = e[4] != 0;
        self.bikes_kill();
        self.bikes.list = (0..count)
            .map(|i| {
                let r = &e[5 + i * 4..9 + i * 4];
                Bike { set: r[0] as i8, rider: r[1] as i8, flags: r[3], terrain: AIRBORNE, ..Bike::default() }
            })
            .collect();
    }

    pub(super) fn bikes_stop(&mut self) {
        let mut ids = Vec::new();
        for b in &mut self.bikes.list {
            for v in b.voices() {
                ids.extend(v.take());
            }
        }
        ids.into_iter().for_each(|id| self.release(id));
    }

    pub(super) fn bikes_kill(&mut self) {
        self.bikes_stop();
        self.bikes.list.clear();
    }

    fn engine_categories(&self) -> Option<[usize; 18]> {
        let mut c = [0; 18];
        for (slot, &id) in c.iter_mut().zip(&ENGINE_IDS) {
            *slot = self.category(id)?;
        }
        Some(c)
    }

    /// Reads each active bike record.
    pub(super) fn bikes_update(&mut self, b: &[u8]) {
        if b[0x24] == 0 {
            return;
        }
        for i in 0..self.bikes.list.len() {
            let r = &b[RECORDS + i * RECORD..RECORDS + (i + 1) * RECORD];
            if r[0] != 0 {
                self.bike_update(i, r);
            }
        }
    }

    fn bike_update(&mut self, i: usize, r: &[u8]) {
        if r[0x3c] != 0 && r[0x3c] as i8 != self.bikes.list[i].set {
            let ids: Vec<u64> = self.bikes.list[i].voices().into_iter().filter_map(|v| v.take()).collect();
            ids.into_iter().for_each(|id| self.release(id));
        }
        let first = r[0x20] != 0;
        let distance = if first { self.bike_place(&self.bikes.list[i], vec3(r, 4)).distance } else { 0.0 };
        let surfaces_changed;
        {
            let b = &mut self.bikes.list[i];
            if r[0x3c] != 0 {
                b.set = r[0x3c] as i8;
            }
            if first && b.set > 0 {
                b.terrain = AIRBORNE;
                b.surface = None;
                b.bump = None;
                b.skid = 0;
                b.state = 1;
                b.distance = distance;
                b.ready = true;
            }
            let gas = r[2] != 0;
            b.gas_changed = gas != b.gas;
            b.gas = gas;
            if gas {
                b.gas_frames += 1;
            } else {
                b.gas_frames = 0;
            }
            let boost = r[0x3e] != 0;
            b.boost_changed = boost != b.boost;
            b.boost = boost;
            b.pos = vec3(r, 4);
            b.speed = i16::from_le_bytes([r[0x10], r[0x11]]);
            b.skid_amount = r[0x1e];
            b.skid_input = r[0x2a];
            b.forced_skid = r[3] != 0;
            b.last_terrain = b.terrain;
            b.terrain = r[0x1f] as i8;
            b.travelled = i32_at(r, 0x24);
            b.silent = r[0x18] != 0;
            b.restart = r[0x22];
            b.revs = fixed(r, 0x14);
            surfaces_changed = b.terrain != b.last_terrain;
        }
        if surfaces_changed {
            let t = self.bikes.list[i].terrain;
            let (surface, bump) = match SURFACES.get(t as usize).filter(|_| t >= 0) {
                Some(&(s, b)) => (self.category(s), self.category(b)),
                None => (None, None),
            };
            let b = &mut self.bikes.list[i];
            (b.surface, b.bump) = (surface, bump);
        }
    }

    fn bike_place(&self, b: &Bike, pos: [f64; 3]) -> Placed {
        let mut att = [0.0; 2];
        let (mut distance, mut pan) = (0.0, 0.0);
        let mut nearest = 0.0;
        for i in (0..(self.players as usize).clamp(1, 2)).rev() {
            let m = &self.mics[i];
            let v = [pos[0] - m.pos[0], pos[1] - m.pos[1], pos[2] - m.pos[2]];
            let d = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            let mut a = if d > FAR {
                FAR_LEVEL - (d - FAR) * FAR_SLOPE
            } else if d > NEAR {
                1.0 - (d - NEAR) * NEAR_SLOPE
            } else {
                1.0
            };
            if !b.local() {
                a *= OTHER_BIKE;
            }
            att[i] = a.max(0.0);
            if i == 0 || att[i] > nearest {
                distance = d;
            }
            nearest = nearest.max(att[i]);
            if i == 0 {
                let x = v[0] * m.m[0] + v[1] * m.m[3] + v[2] * m.m[6];
                if d > 2.0 / ONE as f64 {
                    pan = (-x / d).clamp(-1.0, 1.0);
                }
            }
        }
        Placed { att, pan, distance }
    }

    /// Bike update, once per command block.
    pub(super) fn bikes_cycle(&mut self) {
        self.bikes.cycles = self.bikes.cycles.wrapping_add(1);
        let Some(cats) = self.engine_categories() else { return };
        for i in 0..self.bikes.list.len() {
            if !self.bikes.list[i].ready {
                continue;
            }
            let b = self.bikes.list[i].clone();
            let placed = self.bike_place(&b, b.pos);
            if placed.att[0].max(placed.att[1]) <= 0.0 {
                let ids: Vec<u64> = self.bikes.list[i].voices().into_iter().filter_map(|v| v.take()).collect();
                ids.into_iter().for_each(|id| self.release(id));
                continue;
            }
            // Doppler, as for ambient sounds.
            let closing = ((b.distance - placed.distance) * 32.0).min(1.0);
            let doppler = super::DOPPLER_K / (super::DOPPLER_K - closing);
            self.bikes.list[i].distance = placed.distance;
            self.bike_engine(i, &placed, doppler, &cats);
            if b.local() {
                self.bike_boost(i, &placed, doppler, cats[BOOST]);
                self.bike_surface(i, &placed, doppler);
                self.bike_skid(i, &placed);
            }
        }
    }

    fn alive(&mut self, v: Option<u64>) -> Option<u64> {
        v.filter(|&id| self.voice(id).is_some())
    }

    fn bike_engine(&mut self, i: usize, placed: &Placed, doppler: f64, cats: &[usize; 18]) {
        let local = self.bikes.list[i].local();
        let double = local && !self.bikes.quiet_local;
        let volume = self.volumes[5] as f64 * 128.0 * if double { 2.0 } else { 1.0 };
        let mut gain = self.gains_with(placed, volume);
        if double {
            gain = gain.map(|g| g.clamp(-0x3fff, 0x3fff));
        }
        let voice = self.alive(self.bikes.list[i].engine);
        let roll = self.below(100);
        let cycles = self.bikes.cycles;
        let b = &mut self.bikes.list[i];
        b.engine = voice;
        let mut pitch = doppler * 1.5;
        if b.boost {
            pitch += pitch * BOOST_PITCH_UP;
        }
        if b.terrain == AIRBORNE {
            pitch += AIR_PITCH;
        } else if b.gas || b.gas_changed {
            pitch += TERRAIN_PITCH.get(b.terrain as usize).copied().unwrap_or(0) as f64 * 0.01;
        }
        if b.silent || b.restart != 0 {
            if let Some(v) = b.engine.take() {
                self.release(v);
            }
            if self.bikes.list[i].silent {
                return;
            }
        }
        let b = &mut self.bikes.list[i];
        let air = b.terrain == AIRBORNE;
        let was_air = b.last_terrain == AIRBORNE;
        let terrain_moved = (air && !was_air) || (!air && b.terrain != b.last_terrain);
        let mut restart = false;
        if b.engine.is_none() || b.gas_changed || terrain_moved {
            restart = true;
            b.state = match (b.gas, b.gas_changed) {
                (false, false) => if air { 3 } else { 2 },
                (true, false) if air && !was_air => 4,
                (true, false) if !air && b.terrain != b.last_terrain => 6,
                (true, false) if air => 3,
                (true, false) => b.state,
                (true, true) => if air { 4 } else { 10 },
                (false, true) => if air { 5 } else { 9 },
            };
        }
        // Held throttle settles from the rev-up sample into the cruise one.
        if b.gas && !b.gas_changed && b.gas_frames > 30 * ((cycles & 1) + 1) && b.state == 10 {
            b.state = 8;
            b.gas_frames = 0;
            restart = true;
        }
        pitch += b.speed.clamp(i16::MIN, 200) as f64 * 8.0 / ONE as f64;
        pitch = pitch.clamp(0.75, 2.0);
        if b.state == 6 || b.state == 8 {
            pitch *= 0.9375 + b.revs / 4.0;
        }
        if restart {
            let old = b.engine.take();
            let cat = cats[b.sample(roll)];
            if let Some(v) = old {
                self.release(v);
            }
            self.bikes.list[i].engine = self.start(cat, gain, pitch, 0);
        } else if let Some(v) = b.engine {
            self.update(v, gain, pitch);
        }
    }

    fn bike_boost(&mut self, i: usize, placed: &Placed, doppler: f64, cat: usize) {
        let gain = self.gains_with(placed, self.volumes[1] as f64 * 128.0 * BOOST_VOLUME);
        let pitch = BOOST_PITCH * doppler;
        let b = &self.bikes.list[i];
        let (boost, changed, voice) = (b.boost, b.boost_changed, b.boost_voice);
        let voice = if changed {
            if boost {
                // The previous boost sound plays out.
                self.start(cat, gain, pitch, 0)
            } else {
                if let Some(v) = voice {
                    self.release(v);
                }
                None
            }
        } else {
            voice.filter(|&v| self.update(v, gain, pitch))
        };
        self.bikes.list[i].boost_voice = voice;
    }

    fn bike_surface(&mut self, i: usize, placed: &Placed, doppler: f64) {
        let b = self.bikes.list[i].clone();
        let mut voice = self.alive(b.rolling);
        let mut changed = false;
        if let Some(v) = voice {
            if b.terrain != b.last_terrain || b.terrain == AIRBORNE {
                self.release(v);
                voice = None;
                changed = true;
            }
        }
        self.bikes.list[i].rolling = voice;
        if b.terrain == AIRBORNE {
            return;
        }
        let sfx = self.volumes[1] as f64;
        if let Some(cat) = b.surface {
            let mut volume = ((b.speed as i32) << 6) as i16 as f64 * sfx * 512.0 / ONE as f64;
            if Some(cat) != self.category(LOUD_SURFACE_ID) {
                volume *= SURFACE_VOLUME;
            }
            let gain = self.gains_with(placed, volume);
            let voice = match voice {
                Some(v) if self.update(v, gain, doppler) => Some(v),
                _ => self.start(cat, gain, doppler, 0),
            };
            self.bikes.list[i].rolling = voice;
        }
        if let Some(cat) = b.bump {
            if changed {
                self.bikes.list[i].last_bump = b.travelled;
            }
            let last = self.bikes.list[i].last_bump;
            let speed = b.speed.min(128) as i32;
            let gap = BUMP_GAP.0 + ((speed * (BUMP_GAP.1 - BUMP_GAP.0)) >> 7);
            if b.travelled - last <= gap && !changed {
                return;
            }
            let r = self.below(0x4000) as f64 / ONE as f64;
            let volume = ((speed * 3) << 13 >> 7) as f64 / ONE as f64 + 0.5 - r;
            let gain = self.gains_with(placed, volume * SURFACE_VOLUME * sfx * 128.0);
            self.bikes.list[i].last_bump = b.travelled;
            self.start(cat, gain, 1.0 - r / 2.0, 0);
        }
    }

    fn bike_skid(&mut self, i: usize, placed: &Placed) {
        let voice = self.alive(self.bikes.list[i].skid_voice);
        let b = &mut self.bikes.list[i];
        let mut amount = (b.skid_input as u16) << 4;
        if b.skid_amount > 16 {
            amount += (b.skid_amount as u16) << 3;
        }
        let amount = if b.forced_skid { 0x80 } else { amount.min(0x100) };
        b.skid = ((amount as u32 * 0x4ccc + b.skid as u32 * 0xb333) >> 16) as u16;
        let start_at = if voice.is_some() { 0 } else { SKID_START };
        let on = b.surface.is_some() && b.skid > start_at && !b.silent;
        let keep = b.terrain == b.last_terrain && b.terrain != AIRBORNE && on;
        let (skid, forced, cat) = (b.skid, b.forced_skid, b.surface);
        let mut voice = voice;
        if let Some(v) = voice {
            if !keep {
                self.release(v);
                voice = None;
            }
        }
        if on {
            let mut pitch = 1.0 + SKID_PITCH_SLOPE * skid as f64;
            if forced {
                pitch += FORCED_SKID_PITCH;
            }
            let volume = skid as f64 / 256.0 * self.volumes[1] as f64 * 128.0;
            let gain = self.gains_with(placed, volume);
            voice = match voice {
                Some(v) if self.update(v, gain, pitch) => Some(v),
                _ => cat.and_then(|c| self.start(c, gain, pitch, 0)),
            };
        }
        self.bikes.list[i].skid_voice = voice;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_samples_follow_the_driver() {
        let mut b = Bike { state: 8, ..Bike::default() };
        assert_eq!(b.sample(10), 3);
        assert_eq!(b.sample(99), 2);
        b.state = 2;
        b.restart = 2;
        assert_eq!(b.sample(0), 4);
        b.set = 2;
        b.state = 9;
        assert_eq!(b.sample(0), 7);
    }

    #[test]
    fn other_bikes_are_quieter_and_fade_out() {
        let mut s = Sfx::new();
        s.mics[0].m = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        let mine = Bike { rider: 0, ..Bike::default() };
        let other = Bike { rider: -1, ..Bike::default() };
        assert_eq!(s.bike_place(&mine, [0.5, 0.0, 0.0]).att[0], 1.0);
        assert_eq!(s.bike_place(&other, [0.5, 0.0, 0.0]).att[0], 0.5);
        assert_eq!(s.bike_place(&mine, [6.0, 0.0, 0.0]).att[0], 0.0);
    }
}
