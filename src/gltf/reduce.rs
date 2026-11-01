use glam::DQuat;

pub const VALUE_ERROR: f64 = 1e-5;
pub const ROTATION_ERROR: f64 = 1e-4;

fn quaternion(value: &[f32]) -> DQuat {
    DQuat::from_xyzw(
        value[0] as f64,
        value[1] as f64,
        value[2] as f64,
        value[3] as f64,
    )
    .normalize()
}

fn error(actual: &[f32], a: &[f32], b: &[f32], t: f64, rotation: bool) -> f64 {
    if rotation {
        let predicted = quaternion(a).slerp(quaternion(b), t);
        2.0 * predicted
            .dot(quaternion(actual))
            .abs()
            .clamp(0.0, 1.0)
            .acos()
            / ROTATION_ERROR
    } else {
        actual
            .iter()
            .zip(a)
            .zip(b)
            .map(|((actual, a), b)| {
                (f64::from(*actual) - (f64::from(*a) * (1.0 - t) + f64::from(*b) * t)).abs()
                    / VALUE_ERROR
            })
            .fold(0.0, f64::max)
    }
}

pub fn samples(times: &mut Vec<f32>, values: &mut Vec<f32>, width: usize, rotation: bool) {
    if times.len() <= 1 || !values.iter().all(|v| v.is_finite()) {
        return;
    }
    let row = |i: usize| &values[i * width..(i + 1) * width];
    if (1..times.len()).all(|i| error(row(i), row(0), row(0), 0.0, rotation) <= 1.0) {
        times.clear();
        times.push(0.0);
        values.truncate(width);
        return;
    }

    let mut keep = vec![false; times.len()];
    keep[0] = true;
    keep[times.len() - 1] = true;
    let mut stack = vec![(0, times.len() - 1)];
    while let Some((first, last)) = stack.pop() {
        if last <= first + 1 {
            continue;
        }
        let dt = f64::from(times[last]) - f64::from(times[first]);
        let mut worst = 1.0;
        let mut split = None;
        for i in first + 1..last {
            let t = (f64::from(times[i]) - f64::from(times[first])) / dt;
            let deviation = error(row(i), row(first), row(last), t, rotation);
            if deviation > worst {
                worst = deviation;
                split = Some(i);
            }
        }
        if let Some(i) = split {
            keep[i] = true;
            stack.extend([(first, i), (i, last)]);
        }
    }

    let mut out_times = Vec::new();
    let mut out_values = Vec::new();
    for (i, keep) in keep.into_iter().enumerate() {
        if keep {
            out_times.push(times[i]);
            out_values.extend_from_slice(row(i));
        }
    }
    *times = out_times;
    *values = out_values;
}
