use crate::{
    geometry::{packed_floats, packed_ints},
    value::Fields,
};
use anyhow::{Result, ensure};
use rmpv::Value;
use std::collections::{BTreeSet, HashSet};

#[derive(Clone)]
pub struct Key {
    pub time: f64,
    value: Vec<f64>,
    incoming: Vec<f64>,
    outgoing: Vec<f64>,
}

#[derive(Clone)]
pub struct Curve {
    pub keys: Vec<Key>,
    before: i64,
    after: i64,
}

pub struct Track {
    pub path: String,
    pub property: String,
    pub class: i64,
    pub curve: Curve,
}

pub struct Clip {
    pub name: String,
    pub duration: f64,
    pub tracks: Vec<Track>,
    pub times: Vec<f32>,
}

fn vector(value: &Value) -> Vec<f64> {
    if value.is_number() {
        return vec![value.number()];
    }
    ["x", "y", "z", "w"]
        .iter()
        .filter(|k| !value[**k].is_nil())
        .map(|k| value[*k].number())
        .collect()
}

impl Curve {
    fn ordinary(value: &Value) -> Result<Self> {
        let keys: Vec<_> = value["m_Curve"]
            .array()
            .iter()
            .map(|k| Key {
                time: k["time"].number(),
                value: vector(&k["value"]),
                incoming: vector(&k["inSlope"]),
                outgoing: vector(&k["outSlope"]),
            })
            .collect();
        Self::new(
            keys,
            value["m_PreInfinity"].integer(),
            value["m_PostInfinity"].integer(),
        )
    }

    fn new(keys: Vec<Key>, before: i64, after: i64) -> Result<Self> {
        ensure!(
            keys.iter().all(|k| k.time.is_finite()
                && !k.value.is_empty()
                && k.value.len() == k.incoming.len()
                && k.value.len() == k.outgoing.len()),
            "Invalid animation key"
        );
        ensure!(
            keys.windows(2).all(|w| w[0].time <= w[1].time),
            "Animation keys are out of order"
        );
        Ok(Self {
            keys,
            before,
            after,
        })
    }

    fn compressed(value: &Value) -> Result<Self> {
        let times = packed_ints(&value["m_Times"])?;
        let count = usize::try_from(value["m_Values"]["m_NumItems"].integer())?;
        let data = value["m_Values"]["m_Data"].binary();
        let slopes = packed_floats(&value["m_Slopes"])?;
        ensure!(
            times.len() == count && data.len() >= count * 4 && slopes.len() == count * 4,
            "Compressed rotation channel lengths differ"
        );
        let mut total = 0u64;
        let mut keys = Vec::with_capacity(count);
        for (i, delta) in times.into_iter().enumerate() {
            total += u64::from(delta);
            let mut word = u32::from_le_bytes(data[i * 4..i * 4 + 4].try_into().unwrap());
            let largest = (word & 3) as usize;
            let negative = word & 4 != 0;
            word >>= 3;
            let mut q = vec![0.0; 4];
            for (j, v) in q.iter_mut().enumerate() {
                if j == largest {
                    continue;
                }
                let bits = if j == (largest + 1) % 4 { 9 } else { 10 };
                *v = f64::from(word & ((1 << bits) - 1)) / f64::from((1 << bits) - 1) * 2.0 - 1.0;
                word >>= bits;
            }
            let remaining = (1.0 - q.iter().map(|v| v * v).sum::<f64>()).max(0.0).sqrt();
            q[largest] = if negative { -remaining } else { remaining };
            keys.push(Key {
                time: total as f64 / 100.0,
                value: q,
                incoming: slopes[i * 4..i * 4 + 4].to_vec(),
                outgoing: slopes[i * 4..i * 4 + 4].to_vec(),
            });
        }
        Self::new(
            keys,
            value["m_PreInfinity"].integer(),
            value["m_PostInfinity"].integer(),
        )
    }

    pub fn sample(&self, mut time: f64) -> Vec<f64> {
        let first = &self.keys[0];
        let last = self.keys.last().unwrap();
        let mode = if time < first.time {
            self.before
        } else if time > last.time {
            self.after
        } else {
            2
        };
        let length = last.time - first.time;
        if length > 0.0 && mode != 2 {
            let offset = (time - first.time).rem_euclid(length * if mode == 0 { 2.0 } else { 1.0 });
            time = first.time
                + if mode == 0 && offset > length {
                    2.0 * length - offset
                } else {
                    offset
                };
        }
        let index = self
            .keys
            .partition_point(|k| (k.time as f32) <= time as f32);
        if index == 0 {
            return first.value.clone();
        }
        if index == self.keys.len() {
            return last.value.clone();
        }
        let a = &self.keys[index - 1];
        let b = &self.keys[index];
        let dt = b.time - a.time;
        if dt <= 0.0 {
            return b.value.clone();
        }
        let t = (time - a.time) / dt;
        let h00 = 2.0 * t * t * t - 3.0 * t * t + 1.0;
        let h10 = t * t * t - 2.0 * t * t + t;
        let h01 = -2.0 * t * t * t + 3.0 * t * t;
        let h11 = t * t * t - t * t;
        (0..a.value.len())
            .map(|i| {
                if !a.outgoing[i].is_finite() || !b.incoming[i].is_finite() {
                    a.value[i]
                } else {
                    h00 * a.value[i]
                        + h10 * dt * a.outgoing[i]
                        + h01 * b.value[i]
                        + h11 * dt * b.incoming[i]
                }
            })
            .collect()
    }
}

impl Clip {
    pub fn decode(raw: &Value) -> Result<Self> {
        let mut tracks = Vec::new();
        for (field, property) in [
            ("m_PositionCurves", "translation"),
            ("m_RotationCurves", "rotation"),
            ("m_ScaleCurves", "scale"),
            ("m_FloatCurves", ""),
        ] {
            for value in raw[field].array() {
                let curve = Curve::ordinary(&value["curve"])?;
                if curve.keys.is_empty() {
                    continue;
                }
                let property = if property.is_empty() {
                    value["attribute"].text()
                } else {
                    property
                };
                let expected = match property {
                    "rotation" => 4,
                    "translation" | "scale" => 3,
                    _ => 1,
                };
                ensure!(
                    curve.keys.iter().all(|k| k.value.len() == expected),
                    "Animation curve width differs"
                );
                tracks.push(Track {
                    path: value["path"].text().into(),
                    property: property.into(),
                    class: value["classID"].integer(),
                    curve,
                });
            }
        }
        for value in raw["m_CompressedRotationCurves"].array() {
            let path = value["m_Path"].text();
            if tracks
                .iter()
                .any(|t| t.property == "rotation" && t.path == path)
            {
                continue;
            }
            let curve = Curve::compressed(value)?;
            if !curve.keys.is_empty() {
                tracks.push(Track {
                    path: path.into(),
                    property: "rotation".into(),
                    class: 4,
                    curve,
                });
            }
        }

        // Unity's ordinary curve maps keep the last curve for a target/property.
        let mut seen = HashSet::new();
        tracks.reverse();
        tracks.retain(|track| seen.insert((track.path.clone(), track.property.clone())));
        tracks.reverse();

        let duration = tracks
            .iter()
            .filter_map(|t| t.curve.keys.last().map(|k| k.time))
            .fold(0.0, f64::max);
        let rate = raw["m_SampleRate"].number();
        let rate = if rate.is_finite() && rate > 0.0 {
            rate
        } else {
            30.0
        };
        ensure!(
            duration * rate <= 10_000_000.0,
            "Animation sample count exceeds limit"
        );
        let mut times = BTreeSet::new();
        for frame in 0..=(duration * rate).floor() as usize {
            times.insert(((frame as f64 / rate) as f32).to_bits());
        }
        times.insert((duration as f32).to_bits());
        for track in &tracks {
            for (i, key) in track.curve.keys.iter().enumerate() {
                if key.time < 0.0 {
                    continue;
                }
                let bits = if key.time == 0.0 {
                    0
                } else {
                    (key.time as f32).to_bits()
                };
                times.insert(bits);
                if i > 0
                    && bits > 0
                    && (track.curve.keys[i - 1]
                        .outgoing
                        .iter()
                        .any(|v| !v.is_finite())
                        || key.incoming.iter().any(|v| !v.is_finite()))
                {
                    times.insert(bits - 1);
                }
            }
        }
        Ok(Self {
            name: raw["m_Name"].text().into(),
            duration,
            tracks,
            times: times.into_iter().map(f32::from_bits).collect(),
        })
    }
}
