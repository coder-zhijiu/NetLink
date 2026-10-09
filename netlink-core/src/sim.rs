//! 数据仿真：波形求值。
//!
//! 与前端 `web/src/api/mock.ts` 的 `evalSource` 行为保持一致，
//! 保证浏览器演示模式与 Rust 引擎模式数值路径相同。

use crate::model::ValueSource;

/// 求值一个点位在仿真时刻 `t`（秒）的值。
/// `last` 为该点当前值（随机游走/斜坡等有状态波形需要）。
pub fn evaluate(src: &ValueSource, t: f64, last: f64) -> f64 {
    match *src {
        ValueSource::Static { value } => value,
        ValueSource::Sine { period_s, amplitude, offset } => {
            let period = period_s.max(0.1);
            offset + amplitude * (std::f64::consts::TAU * t / period).sin()
        }
        ValueSource::Ramp { step, min, max } => {
            let span = (max - min).max(1e-9);
            let mut v = last + step.max(0.0);
            if v > max {
                v = min + ((v - min) % span);
            }
            v
        }
        ValueSource::RandomWalk { step, min, max } => {
            use rand::Rng;
            let s = step.max(0.0);
            let delta = rand::rng().random_range(-s..=s);
            (last + delta).clamp(min, max)
        }
        ValueSource::Swing { min, max, period_s } => {
            let period = period_s.max(0.1);
            let mid = (min + max) / 2.0;
            let amp = (max - min) / 2.0;
            mid + amp * (std::f64::consts::TAU * t / period).sin()
        }
    }
}
