use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct Scenario {
    pub scenario_id: String,
    pub seed: u64,
    pub horizon_ns: i64,
    pub exchange_config: ExchangeConfig,
    pub oracle_config: OracleConfig,
    pub latency_config: Option<LatencyConfig>,
    pub agent_configs: Vec<AgentConfig>,
}

#[derive(Deserialize, Debug)]
pub struct ExchangeConfig {
    #[serde(default = "default_symbol")]
    pub symbol: String,
    #[serde(default = "one")]
    pub tick_size: i64,
    #[serde(default = "one")]
    pub lot_size: i64,
    #[serde(default)]
    pub stp_policy: Option<String>,
}

fn default_symbol() -> String { "ABM".to_string() }
fn one() -> i64 { 1 }

#[derive(Deserialize, Debug)]
pub struct OracleConfig {
    pub params: OracleParams,
}

#[derive(Deserialize, Debug)]
pub struct OracleParams {
    #[serde(default = "default_initial_price")]
    pub initial_price: i64,
    #[serde(default)]
    pub kappa: f64,
    #[serde(default = "default_sigma")]
    pub sigma: f64,
    #[serde(default)]
    pub jump_intensity: f64,
    #[serde(default)]
    pub jump_sigma: f64,
}

fn default_initial_price() -> i64 { 100_000 }
fn default_sigma() -> f64 { 5e-5 }

#[derive(Deserialize, Debug)]
pub struct LatencyConfig {
    pub model: String,
    #[serde(default)]
    pub params: LatencyParams,
}

#[derive(Deserialize, Debug, Default)]
pub struct LatencyParams {
    #[serde(default)]
    pub mean_ns: i64,
    #[serde(default)]
    pub sigma: f64,
    #[serde(default)]
    pub min_ns: i64,
    #[serde(default = "default_max_ns")]
    pub max_ns: i64,
    #[serde(default = "default_alpha")]
    pub alpha: f64,
}

fn default_max_ns() -> i64 { 1_000_000_000_000 }
fn default_alpha() -> f64 { 1.5 }

#[derive(Deserialize, Debug)]
pub struct AgentConfig {
    pub agent_type: String,
    pub count: u32,
    #[serde(default)]
    pub params: serde_json::Value,
}

pub fn load(path: &str) -> Result<Scenario, Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(path)?;
    let s: Scenario = serde_json::from_str(&raw)?;
    Ok(s)
}
