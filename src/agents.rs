//! Port of abides_fork/agents.py — the four sclet aggressive = self.rng.gen_range(0..10) < 8;enario-driven agents.
//!
//! Each agent wakes at a fixed interval, reads the current book, and posts
//! orders. We skip the query-spread message round-trip because the kernel is
//! single-threaded — reading the book directly is the same information.

use crate::oracle::Oracle;
use crate::orderbook::{OrderBook, Side};
use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentKind {
    NoiseTrader,
    MarketMaker,
    ValueTrader,
    MomentumTrader,
}

#[derive(Clone, Copy, Debug)]
pub struct OrderRequest {
    pub side: Side,
    pub price: i64,
    pub size: i64,
}

pub struct Agent {
    pub id: i32,
    pub kind: AgentKind,
    pub next_wakeup: i64,
    pub interval_ns: i64,
    pub rng: ChaCha8Rng,

    // Params (only the ones the agent's kind uses)
    pub order_size_mean: f64,
    pub order_size_std: f64,
    pub price_offset_ticks: i64,
    pub reference_price: i64,
    pub spread_ticks: i64,
    pub depth_levels: i64,
    pub size_per_level: i64,
    pub threshold_ticks: i64,
    pub lookback: usize,
    pub price_history: Vec<i64>,
}

fn get_f64(v: &serde_json::Value, key: &str, default: f64) -> f64 {
    v.get(key).and_then(|x| x.as_f64()).unwrap_or(default)
}
fn get_i64(v: &serde_json::Value, key: &str, default: i64) -> i64 {
    v.get(key).and_then(|x| x.as_i64()).unwrap_or(default)
}
fn interval_from_params(params: &serde_json::Value) -> i64 {
    if let Some(ns) = params.get("rebalance_interval_ns").and_then(|x| x.as_i64()) {
        return ns;
    }
    let hz = get_f64(params, "arrival_rate_hz", 1.0);
    if hz > 0.0 { (1e9 / hz) as i64 } else { 1_000_000_000 }
}

impl Agent {
    pub fn new(
        id: i32,
        kind: AgentKind,
        params: &serde_json::Value,
        reference_price: i64,
        mkt_open: i64,
        seed: u64,
    ) -> Self {
        let interval_ns = interval_from_params(params);
        let mut rng = ChaCha8Rng::seed_from_u64(seed);

        let lookback = get_i64(params, "lookback", 5).max(1) as usize;
        let order_size_mean = get_f64(
            params,
            "order_size_mean",
            match kind {
                AgentKind::ValueTrader => 25.0,
                AgentKind::MomentumTrader => 15.0,
                _ => 10.0,
            },
        );

        // Stagger first wakeups so agents don't all fire at t=0
        let jitter = rng.gen_range(0..=interval_ns);

        Agent {
            id,
            kind,
            next_wakeup: mkt_open + jitter,
            interval_ns,
            rng,
            order_size_mean,
            order_size_std: get_f64(params, "order_size_std", 2.0),
            price_offset_ticks: get_i64(params, "price_offset_ticks", 5),
            reference_price,
            spread_ticks: get_i64(params, "spread_ticks", 2).max(2),
            depth_levels: get_i64(params, "depth_levels", 3).max(1),
            size_per_level: get_i64(params, "size_per_level", 10).max(1),
            threshold_ticks: get_i64(params, "threshold_ticks", 2),
            lookback,
            price_history: Vec::with_capacity(lookback),
        }
    }

    /// Called on each wakeup. Returns orders to submit.
    /// Also re-arms next_wakeup for the caller.
    pub fn act(
        &mut self,
        t_ns: i64,
        book: &OrderBook,
        oracle: &mut Oracle,
        my_resting: &[i64],
    ) -> Vec<OrderRequest> {
        self.next_wakeup = t_ns + self.interval_ns;
        match self.kind {
            AgentKind::NoiseTrader => self.act_noise(book),
            AgentKind::MarketMaker => self.act_market_maker(book),
            AgentKind::ValueTrader => self.act_value(t_ns, book, oracle),
            AgentKind::MomentumTrader => self.act_momentum(book, my_resting),
        }
    }

    // ---- NoiseTrader ------------------------------------------------------
    fn act_noise(&mut self, book: &OrderBook) -> Vec<OrderRequest> {
        let bid = book.best_bid();
        let ask = book.best_ask();

        // Halve the size: MM quotes survive a single hit, so the mid moves less.
let size = ((self.order_size_mean * 0.85)
    + self.rng.gen_range(-1.0..1.0) * self.order_size_std)
    .round()
    .max(1.0) as i64;
 // Rare, very large aggressive orders — fattens the return tail
let size = if self.rng.gen_range(0..20) == 0 { size * 5 } else { size };
        let buy = self.rng.gen::<bool>();

        // 60% passive (inside spread), 40% aggressive (crosses).
// Passive orders rest, don't move the mid.
let aggressive = self.rng.gen_range(0..10) < 8;
let offset = self.rng.gen_range(0..=self.price_offset_ticks*2);

        let (side, price) = if buy {
    // Aggressive buy crosses the ask; passive buy rests below the bid.
    let anchor = if aggressive {
        ask.or(bid).unwrap_or(self.reference_price)
    } else {
        bid.or(ask).unwrap_or(self.reference_price)
    };
    let delta = if aggressive { offset } else { -offset };
    (Side::Bid, anchor + delta)
} else {
    let anchor = if aggressive {
        bid.or(ask).unwrap_or(self.reference_price)
    } else {
        ask.or(bid).unwrap_or(self.reference_price)
    };
    let delta = if aggressive { offset } else { -offset };
    (Side::Ask, anchor - delta)
};

        vec![OrderRequest { side, price, size }]
    }

    // ---- MarketMaker ------------------------------------------------------
    fn act_market_maker(&mut self, book: &OrderBook) -> Vec<OrderRequest> {
        let bid = book.best_bid();
        let ask = book.best_ask();
        let mid = match (bid, ask) {
            (Some(b), Some(a)) => (b + a) / 2,
            _ => self.reference_price,
        };

                let half = self.spread_ticks / 2;
        let mut out = Vec::with_capacity((self.depth_levels * 2) as usize);

        // 10% of MM wakes post one outsized quote — fattens the depth tail
        let big = self.rng.gen_range(0..10) == 0;
        let big_mult = if big { self.rng.gen_range(2..4) } else { 1 };

                for lvl in 0..self.depth_levels {
                    let sz = (self.size_per_level / 4).max(1) * big_mult;
                        out.push(OrderRequest {
                side: Side::Bid,
                price: mid - half - lvl,
                size: sz,
            });
            out.push(OrderRequest {
                side: Side::Ask,
                price: mid + half + lvl,
                size: sz,
            });
        }
        out
    }

    // ---- ValueTrader ------------------------------------------------------
            fn act_value(&mut self, t_ns: i64, book: &OrderBook, oracle: &mut Oracle) -> Vec<OrderRequest> {
        let bid = book.best_bid();
        let ask = book.best_ask();
        let mid = match (bid, ask) {
            (Some(b), Some(a)) => (b + a) / 2,
            (Some(b), None) => b,
            (None, Some(a)) => a,
            (None, None) => return Vec::new(),
        };

        let sigma_n = 1000.0;
        let fundamental = oracle.observe_price(t_ns, sigma_n);
        let size = self.order_size_mean.round().max(1.0) as i64;

        if mid < fundamental - self.threshold_ticks {
            if let Some(a) = ask {
                return vec![OrderRequest { side: Side::Bid, price: a, size }];
            }
        } else if mid > fundamental + self.threshold_ticks {
            if let Some(b) = bid {
                return vec![OrderRequest { side: Side::Ask, price: b, size }];
            }
        }
        Vec::new()
    }

    // ---- MomentumTrader ---------------------------------------------------
    fn act_momentum(&mut self, book: &OrderBook, my_resting: &[i64]) -> Vec<OrderRequest> {
        let bid = book.best_bid();
        let ask = book.best_ask();
        let mid = match (bid, ask) {
            (Some(b), Some(a)) => (b + a) / 2,
            (Some(b), None) => b,
            (None, Some(a)) => a,
            (None, None) => return Vec::new(),
        };

        self.price_history.push(mid);
        if self.price_history.len() > self.lookback {
            self.price_history.remove(0);
        }
        if self.price_history.len() < self.lookback {
            return Vec::new();
        }

        let first = self.price_history[0];
        let last = *self.price_history.last().unwrap();
        let size = self.order_size_mean.round().max(1.0) as i64;

        // Uptrend: buy side, at/below best bid. Downtrend: sell at/above best ask.
        if last > first + self.threshold_ticks {
            if let Some(b) = bid {
                return vec![OrderRequest { side: Side::Bid, price: b, size }];
            }
        } else if last < first - self.threshold_ticks {
            if let Some(a) = ask {
                return vec![OrderRequest { side: Side::Ask, price: a, size }];
            }
        }

        // Cancel-on-repost: MarketMaker-style agents retract; here we just
        // return nothing on no-trade wakeups. Keeps resting orders on book.
        let _ = my_resting;
        Vec::new()
    }
}

/// Construct every agent from the scenario's agent_configs.
pub fn build_agents(
    scenario: &crate::scenario::Scenario,
    mkt_open: i64,
) -> Vec<Agent> {
    let reference_price = scenario.oracle_config.params.initial_price;
    let mut out = Vec::new();
    let mut id: i32 = 1;
    let mut seed_off: u64 = 1_000;

    for cfg in &scenario.agent_configs {
        let kind = match cfg.agent_type.as_str() {
            "NoiseTrader" => AgentKind::NoiseTrader,
            "MarketMaker" => AgentKind::MarketMaker,
            "ValueTrader" => AgentKind::ValueTrader,
            "MomentumTrader" => AgentKind::MomentumTrader,
            _ => continue,
        };
        for _ in 0..cfg.count {
            out.push(Agent::new(
                id,
                kind,
                &cfg.params,
                reference_price,
                mkt_open,
                scenario.seed.wrapping_add(seed_off),
            ));
            id += 1;
            seed_off += 1;
        }
    }
    out
}
