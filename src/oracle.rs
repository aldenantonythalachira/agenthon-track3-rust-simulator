//! Faithful Rust port of abides_markets SparseMeanRevertingOracle.
//!
//! OU transition density (exact, not Euler):
//!     v = Normal( mu + (pv - mu) * exp(-gamma * d),
//!                 sqrt( (theta^2 / (2*gamma)) * (1 - exp(-2*gamma*d)) ) )
//! then clamp >=0, round to int.
//!
//! Megashocks: Poisson process, inter-arrival ~ Exp(lambda),
//! value ~ N(mean, sqrt(var)) with random sign.

use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use rand_distr::{Distribution, Exp, Normal};

#[derive(Clone, Copy)]
pub struct Megashock {
    pub time_ns: f64,
    pub value: f64,
}

pub struct Oracle {
    pub r_bar: f64,
    pub kappa: f64,            // per-ns
    pub fund_vol: f64,         // theta
    pub megashock_lambda: f64, // per-ns (>= 1e-17 to avoid div by 0)
    pub megashock_mean: f64,
    pub megashock_std: f64,
    pub rng: ChaCha8Rng,
    pub mkt_open: i64,
    pub mkt_close: i64,
    pub last_t: i64,
    pub last_v: f64,
    pub megashocks: Vec<Megashock>,
}

impl Oracle {
    pub fn new(
        r_bar: f64,
        kappa: f64,
        fund_vol: f64,
        megashock_lambda: f64,
        megashock_mean: f64,
        megashock_var: f64,
        seed: u64,
        mkt_open: i64,
        mkt_close: i64,
    ) -> Self {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let megashock_std = megashock_var.max(0.0).sqrt();
        // Python does `lambda=0` -> exponential(scale=inf) -> essentially never; guard anyway.
        let lam = megashock_lambda.max(1e-18);

        // First megashock time (Python uses global RNG here — we use our seeded RNG)
        let gap: f64 = Exp::new(lam).unwrap().sample(&mut rng);
        let first_t = mkt_open as f64 + gap;

        let dist = Normal::new(megashock_mean, megashock_std).unwrap();
        let mut first_v = dist.sample(&mut rng);
        if rng.gen::<u32>() % 2 != 0 {
            first_v = -first_v;
        }

        Oracle {
            r_bar,
            kappa,
            fund_vol,
            megashock_lambda: lam,
            megashock_mean,
            megashock_std,
            rng,
            mkt_open,
            mkt_close,
            last_t: mkt_open,
            last_v: r_bar,
            megashocks: vec![Megashock {
                time_ns: first_t,
                value: first_v,
            }],
        }
    }

    /// OU advance from (pt, pv) to ts, then add v_adj, clamp >=0, round.
    fn ou_step(&mut self, ts: i64, v_adj: f64, pt: i64, pv: f64) -> f64 {
        let d = (ts - pt) as f64;
        if d <= 0.0 {
            let v = (pv + v_adj).max(0.0);
            return v.round();
        }

        let mu = self.r_bar;
        let gamma = self.kappa;
        let theta = self.fund_vol;

        let mean = mu + (pv - mu) * (-gamma * d).exp();
        let var = (theta * theta / (2.0 * gamma)) * (1.0 - (-2.0 * gamma * d).exp());
        let std = var.max(0.0).sqrt();

        // Python uses random_state.normal(loc=mean, scale=std).
        // If std == 0, normal returns mean exactly.
        let mut v = if std > 0.0 {
            Normal::new(mean, std).unwrap().sample(&mut self.rng)
        } else {
            mean
        };
        v += v_adj;
        if v < 0.0 {
            v = 0.0;
        }
        v.round()
    }

    /// Advance the fundamental from self.last_t to current_time, consuming
    /// megashocks along the way. Returns the integer fundamental at current_time.
    pub fn advance(&mut self, current_time: i64) -> i64 {
        if current_time <= self.last_t {
            return self.last_v as i64;
        }

        let mut pt = self.last_t;
        let mut pv = self.last_v;

        let last_ms = *self.megashocks.last().unwrap();
        let mut mst = last_ms.time_ns;
        let mut msv = last_ms.value;

        while (mst as i64) < current_time {
            let v = self.ou_step(mst as i64, msv, pt, pv);
            pt = mst as i64;
            pv = v;

            // Schedule the next megashock
            let gap: f64 = Exp::new(self.megashock_lambda)
                .unwrap()
                .sample(&mut self.rng);
            mst = pt as f64 + gap;

            let dist = Normal::new(self.megashock_mean, self.megashock_std).unwrap();
            let mut val = dist.sample(&mut self.rng);
            if self.rng.gen::<u32>() % 2 != 0 {
                val = -val;
            }
            msv = val;

            self.megashocks.push(Megashock {
                time_ns: mst,
                value: msv,
            });
        }

        let v = self.ou_step(current_time, 0.0, pt, pv);
        self.last_t = current_time;
        self.last_v = v;
        v as i64
    }

    /// Public observation: noisy fundamental at current_time.
    /// sigma_n is a VARIANCE, not std (matches ABIDES).
    pub fn observe_price(&mut self, current_time: i64, sigma_n: f64) -> i64 {
        let r_t = if current_time >= self.mkt_close {
            self.advance(self.mkt_close - 1)
        } else {
            self.advance(current_time)
        };

        if sigma_n == 0.0 {
            r_t
        } else {
            let std = sigma_n.max(0.0).sqrt();
            if std > 0.0 {
                let dist = Normal::new(r_t as f64, std).unwrap();
                dist.sample(&mut self.rng).round() as i64
            } else {
                r_t
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oracle_produces_values() {
        // Same params as t3-as01-base-mix
        let r_bar = 100_000.0;
        let kappa = 0.05 / 1e9;
        let fund_vol = 0.001;
        let mega_lambda = 2.77778e-18;
        let mega_mean = 1000.0;
        let mega_var = 50_000.0;
        let mkt_open = 1_612_487_400_000_000_000i64; // arbitrary ns
        let mkt_close = mkt_open + 20_000_000_000i64;

        let mut o = Oracle::new(
            r_bar, kappa, fund_vol, mega_lambda, mega_mean, mega_var,
            42, mkt_open, mkt_close,
        );

        let mut prev = r_bar as i64;
        for k in 1..=20 {
            let t = mkt_open + k * 1_000_000_000; // every 1 second
            let v = o.observe_price(t, 0.0);
            println!("t={} fundamental={}", k, v);
            assert!(v >= 0);
            // value should stay in a broad band around r_bar
            assert!((v - prev).abs() < 100_000);
            prev = v;
        }
    }
}
