mod scenario;
mod orderbook;
mod oracle;
mod agents;
mod trace;

use agents::{build_agents, Agent, AgentKind};
use orderbook::{OrderBook, RestingOrder, Side};
use oracle::Oracle;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use rand_distr::{Distribution, LogNormal};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::PathBuf;

use trace::{write_trace, write_message_trace, write_events_json, MsgRow};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Event {
    t_ns: i64,
    seq: u64,
    agent_id: i32,
}

impl Ord for Event {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.t_ns, self.seq).cmp(&(other.t_ns, other.seq))
    }
}
impl PartialOrd for Event {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let mut config_path = "";
    let mut out_path = "";
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => { config_path = &args[i + 1]; i += 2; }
            "--out" => { out_path = &args[i + 1]; i += 2; }
            _ => { i += 1; }
        }
    }

    let s = scenario::load(config_path)?;
    let out = PathBuf::from(out_path);
    let out_dir = out.parent().unwrap_or_else(|| std::path::Path::new("."));

    let t0 = std::time::Instant::now();

    let date_ns: i64 = 1_612_512_600_000_000_000;
    let mkt_open = date_ns + 9 * 3600 * 1_000_000_000 + 30 * 60 * 1_000_000_000;
    let mkt_close = mkt_open + s.horizon_ns;
    let stop_time = mkt_close + 1_000_000_000;

    let op = &s.oracle_config.params;
    let kappa_per_ns = if op.kappa > 0.0 { op.kappa / 1e9 } else { 1.67e-16 };
    let mega_lambda_per_ns = if op.jump_intensity > 0.0 {
        op.jump_intensity / 1e9
    } else {
        2.77778e-18
    };

    let mut oracle = Oracle::new(
        op.initial_price as f64,
        kappa_per_ns,
        op.sigma,
        mega_lambda_per_ns,
        1000.0,
        50_000.0,
        s.seed,
        mkt_open,
        mkt_close,
    );

    let mut agent_list: Vec<Agent> = build_agents(&s, mkt_open);
    let mut agent_resting: HashMap<i32, Vec<i64>> = HashMap::new();
    eprintln!("built {} agents", agent_list.len());

    let mut book = OrderBook::new();

    let mut queue: BinaryHeap<Reverse<Event>> = BinaryHeap::new();
    let mut seq: u64 = 0;
    for a in &agent_list {
        queue.push(Reverse(Event { t_ns: a.next_wakeup, seq, agent_id: a.id }));
        seq += 1;
    }

    let mut trace_t: Vec<i64> = Vec::new();
    let mut trace_a: Vec<i32> = Vec::new();
    let mut trace_m: Vec<&'static str> = Vec::new();
    let mut trace_s: Vec<&'static str> = Vec::new();
    let mut trace_p: Vec<i64> = Vec::new();
    let mut trace_q: Vec<i64> = Vec::new();
    let mut trace_o: Vec<i64> = Vec::new();

    let mut msg_rows: Vec<MsgRow> = Vec::new();
    let mut msg_id: i64 = 1;
    let mut order_id: i64 = 0;

    let mut rng = ChaCha8Rng::seed_from_u64(s.seed);
    let (lat_mean, lat_sigma, lat_min, lat_max) = match s.latency_config.as_ref() {
        Some(cfg) => (
            if cfg.params.mean_ns > 0 { cfg.params.mean_ns as f64 } else { 500.0 },
            if cfg.params.sigma > 0.0 { cfg.params.sigma } else { 0.3 },
            if cfg.params.min_ns > 0 { cfg.params.min_ns } else { 100 },
            if cfg.params.max_ns > 0 { cfg.params.max_ns } else { 2_000_000 },
        ),
        None => (500.0, 0.3, 100, 2_000_000),
    };
    let log_dist = LogNormal::new(lat_mean.ln().max(1e-9), lat_sigma).unwrap();
    let sample_lat = move |rng: &mut ChaCha8Rng| -> i64 {
        let v: f64 = log_dist.sample(rng);
        v.max(lat_min as f64).min(lat_max as f64).round() as i64
    };

    let empty_resting: Vec<i64> = Vec::new();
    let dummy_params = serde_json::json!({});

    while let Some(Reverse(ev)) = queue.pop() {
        if ev.t_ns > stop_time { break; }

        msg_rows.push(MsgRow {
            seq: msg_rows.len() as i64,
            t_recv_ns: ev.t_ns,
            t_send_ns: None,
            latency_ns: 0,
            src_id: ev.agent_id,
            dst_id: ev.agent_id,
            message_id: msg_id,
            msg_type: "AGENT_WAKEUP".to_string(),
            order_id: None,
            causal_parent: None,
        });
        msg_id += 1;

        let idx = match agent_list.iter().position(|a| a.id == ev.agent_id) {
            Some(i) => i,
            None => continue,
        };

        let mut agent = std::mem::replace(
            &mut agent_list[idx],
            Agent::new(0, AgentKind::NoiseTrader, &dummy_params, 0, 0, 0),
        );

        let requests = agent.act(ev.t_ns, &book, &mut oracle, &empty_resting);
        let next_wake = agent.next_wakeup;
        agent_list[idx] = agent;

        for req in requests {
            let ord = RestingOrder {
                order_id,
                agent_id: ev.agent_id,
                side: req.side,
                price: req.price,
                quantity: req.size,
                t_ns: ev.t_ns,
            };
            order_id += 1;

            trace_t.push(ev.t_ns);
            trace_a.push(ev.agent_id);
            trace_m.push("ORDER_SUBMITTED");
            trace_s.push(if req.side == Side::Bid { "BID" } else { "ASK" });
            trace_p.push(req.price);
            trace_q.push(req.size);
            trace_o.push(ord.order_id);

            let lat1 = sample_lat(&mut rng);
            msg_rows.push(MsgRow {
                seq: msg_rows.len() as i64,
                t_recv_ns: ev.t_ns + lat1,
                t_send_ns: Some(ev.t_ns),
                latency_ns: lat1,
                src_id: ev.agent_id,
                dst_id: 0,
                message_id: msg_id,
                msg_type: "LIMIT_ORDER".to_string(),
                order_id: Some(ord.order_id),
                causal_parent: None,
            });
            msg_id += 1;

            let lat2 = sample_lat(&mut rng);
            msg_rows.push(MsgRow {
                seq: msg_rows.len() as i64,
                t_recv_ns: ev.t_ns + lat2,
                t_send_ns: Some(ev.t_ns),
                latency_ns: lat2,
                src_id: 0,
                dst_id: ev.agent_id,
                message_id: msg_id,
                msg_type: "ORDER_ACCEPTED".to_string(),
                order_id: Some(ord.order_id),
                causal_parent: None,
            });
            msg_id += 1;

            for _ in 0..5 {
                let dst = rng.gen_range(1..=20i32);
                if dst == ev.agent_id { continue; }
                let lat3 = sample_lat(&mut rng);
                msg_rows.push(MsgRow {
                    seq: msg_rows.len() as i64,
                    t_recv_ns: ev.t_ns + lat3,
                    t_send_ns: Some(ev.t_ns),
                    latency_ns: lat3,
                    src_id: 0,
                    dst_id: dst,
                    message_id: msg_id,
                    msg_type: "MARKET_DATA".to_string(),
                    order_id: None,
                    causal_parent: None,
                });
                msg_id += 1;
            }

            let oid = ord.order_id;
            let ord_side = ord.side;
            let ord_price = ord.price;
            let ord_size = ord.quantity;
            let ord_agent = ord.agent_id;
            let fills = book.handle_limit(ord);
            let total_filled: i64 = fills.iter().map(|(_, take, _)| *take).sum();

            if total_filled < ord_size {
                trace_t.push(ev.t_ns);
                trace_a.push(ord_agent);
                trace_m.push("ORDER_ACCEPTED");
                trace_s.push(if ord_side == Side::Bid { "BID" } else { "ASK" });
                trace_p.push(ord_price);
                trace_q.push(ord_size - total_filled);
                trace_o.push(oid);
            }

            for (rest, take, px) in fills {
                trace_t.push(ev.t_ns);
                trace_a.push(rest.agent_id);
                trace_m.push("PARTIAL_FILL");
                trace_s.push(if rest.side == Side::Bid { "BID" } else { "ASK" });
                trace_p.push(px);
                trace_q.push(take);
                trace_o.push(rest.order_id);
            }
        }

        if let Some(b) = book.best_bid() {
            trace_t.push(ev.t_ns);
            trace_a.push(0);
            trace_m.push("QUOTE_UPDATE");
            trace_s.push("BID");
            trace_p.push(b);
            trace_q.push(book.best_bid_qty());
            trace_o.push(-1);
        }
        if let Some(a) = book.best_ask() {
            trace_t.push(ev.t_ns);
            trace_a.push(0);
            trace_m.push("QUOTE_UPDATE");
            trace_s.push("ASK");
            trace_p.push(a);
            trace_q.push(book.best_ask_qty());
            trace_o.push(-1);
        }

        if next_wake <= stop_time {
            queue.push(Reverse(Event { t_ns: next_wake, seq, agent_id: ev.agent_id }));
            seq += 1;
        }
    }

    let mut last_fill_idx: HashMap<i64, usize> = HashMap::new();
    for i in 0..trace_m.len() {
        if trace_m[i] == "PARTIAL_FILL" {
            last_fill_idx.insert(trace_o[i], i);
        }
    }
    let final_set: HashSet<usize> = last_fill_idx.values().copied().collect();
    for i in 0..trace_m.len() {
        if final_set.contains(&i) {
            trace_m[i] = "ORDER_FILLED";
        }
    }

    let mut indices: Vec<usize> = (0..trace_t.len()).collect();
    indices.sort_by_key(|&i| (trace_t[i], trace_o[i]));
    let trace_t: Vec<i64> = indices.iter().map(|&i| trace_t[i]).collect();
    let trace_a: Vec<i32> = indices.iter().map(|&i| trace_a[i]).collect();
    let trace_m: Vec<&'static str> = indices.iter().map(|&i| trace_m[i]).collect();
    let trace_s: Vec<&'static str> = indices.iter().map(|&i| trace_s[i]).collect();
    let trace_p: Vec<i64> = indices.iter().map(|&i| trace_p[i]).collect();
    let trace_q: Vec<i64> = indices.iter().map(|&i| trace_q[i]).collect();
    let trace_o: Vec<i64> = indices.iter().map(|&i| trace_o[i]).collect();

    eprintln!("generated {} trace events, {} messages", trace_t.len(), msg_rows.len());

    let wall = t0.elapsed().as_secs_f64();

    let trace_sha = write_trace(
        &out,
        &trace_t, &trace_a, &trace_m, &trace_s, &trace_p, &trace_q, &trace_o,
    )?;

    let msg_path = out_dir.join("message_trace.parquet");
    let msg_sha = write_message_trace(&msg_path, &msg_rows)?;

    let events_path = out_dir.join("events.json");
    write_events_json(
        &events_path,
        &s.scenario_id,
        s.seed,
        trace_t.len(),
        msg_rows.len(),
        wall,
        &trace_sha,
        &msg_sha,
    )?;

    eprintln!(
        "rust_sim: {} events, {} messages, {:.3}s wall, {:.0} ev/s",
        trace_t.len(),
        msg_rows.len(),
        wall,
        trace_t.len() as f64 / wall.max(1e-9)
    );
    Ok(())
}
