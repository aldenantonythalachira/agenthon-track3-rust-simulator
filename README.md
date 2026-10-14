# Agenthon 2026 — Track 3 Market Simulator

A ground-up Rust reimplementation of the ABIDES discrete-event market
simulator, built for the Track 3 "Semantic-Preserving Market Simulation"
competition.

**Result: ~863,500 events/second** on the reference scenario — a
**~117× speedup** over the ABIDES Python baseline (~7,400 ev/s) while
preserving every statistical property the track measures.

---

## The problem

Track 3 hands you ABIDES — an open-source market simulator that is
correct and slow — and asks for a faster one that still *behaves* like a
real market. Speed alone scores nothing. The trace your simulator emits
must match the reference within tight statistical tolerances:

| Stylized fact | What it measures | Ceiling |
|---|---|---|
| KS distance | Return distribution shape | ≤ 0.08 |
| ACF of \|returns\| | Volatility clustering | ≤ 0.12 |
| Hill index | Tail heaviness | ≤ 1.5 |
| Depth JS divergence | Order-book depth shape | ≤ 0.10 |


---

## What I built

A complete simulator in Rust, ~1,500 lines across six modules:

| Module | Role |
|---|---|
| `main.rs` | Event loop, wakeup scheduler, output emission |
| `scenario.rs` | Scenario JSON → typed Rust structs |
| `oracle.rs` | Fundamental price process (OU + Poisson megashocks) |
| `orderbook.rs` | Price-time-priority limit order book |
| `agents.rs` | Four agent types matching the scenario schema |
| `trace.rs` | Trace and message-ledger writers |

Every piece of the ABIDES semantics that the gates measure is
reproduced: order lifecycle, fill pricing, quote updates, message
causality, latency realization, and the oracle's specific stochastic
process. Everything that does not affect admissibility — the Python
object model, the logging framework, the intermediate dataframes — is
removed.

---

## Where the speedup came from

Profiling the ABIDES baseline revealed that **half of its runtime was
not simulation at all**. It was interpreter overhead:

| Cost centre | Share of baseline runtime |
|---|---|
| Debug-log string formatting (thrown away at WARNING level) | ~18% |
| `deepcopy` on every logged event | ~12% |
| `np.clip` wrapper cost per latency draw | ~8% |
| Priority-queue push/pop | ~6% |
| **Actual simulation work** | **~50%** |

Roughly half the baseline was formatting strings nobody would read,
copying objects nobody would mutate, and wrapping single floats in
NumPy arrays. Rust eliminates all of it — the equivalent work is a
handful of machine instructions.

The remaining 50% — the real simulation — runs faster too, because
Rust's `BTreeMap`-based order book and `BinaryHeap` event queue have
no interpreter between them and the CPU.

---

## The engineering challenges

Three problems had to be solved before the simulator could pass the
semantic gates. Each required understanding the reference deeply
enough to reproduce its behavior, not just its output.

**1. The oracle must use the exact OU transition density.**

The obvious way to advance a mean-reverting price is a step-wise
update. That produces a plausible-looking price series with the wrong
return distribution — the KS check rejects it. The correct approach
uses the conditional Gaussian of the Ornstein–Uhlenbeck process over
the whole interval, so that the variance of the return scales with the
square root of elapsed time the way real price processes do.

**2. Megashocks must fire on a Poisson schedule, not on a fixed cadence.**

The fat tails that the Hill index measures come from occasional large
jumps superimposed on the OU process. Sampling them on any regular
schedule averages them out and produces Gaussian tails. Sampling their
inter-arrival times exponentially produces the correct heavy tail.
The oracle walks this schedule forward, splitting the OU interval at
each shock time so the post-shock value initializes the next interval.

**3. Parquet columns must carry the pandas metadata blob.**

This one took a while to find. PyArrow reads a nullable `int64` column
as `float64` if the parquet footer has no pandas metadata. `float64`
can't represent a 19-digit nanosecond timestamp exactly — the low two
or three digits round. That silently broke the kernel invariant
`t_recv_ns − t_send_ns == latency_ns` on the message ledger. The fix
was to write the metadata blob into the parquet footer so pandas
restores `Int64` and the arithmetic is exact.

---

## The result

Across the 65 public scenarios the self-reported rate ranges from
~530,000 to ~1,340,000 events/sec, with a median near 900,000.

On the reference scenario `t3-as01-base-mix`, all four stylized-fact
metrics pass:
KS = 0.075 ≤ 0.08
ACF |r| = 0.016 ≤ 0.12
Hill = 0.609 ≤ 1.5
Depth JS = 0.039 ≤ 0.10


The simulator is packaged as a `linux/amd64` Docker image, pinned by
immutable digest, running fully offline with no network access and
completing the reference scenario in under 30 milliseconds.

---

## Licence

Apache-2.0
