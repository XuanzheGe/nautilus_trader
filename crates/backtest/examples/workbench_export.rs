// -------------------------------------------------------------------------------------------------
//  Copyright (C) 2015-2026 Nautech Systems Pty Ltd. All rights reserved.
//  https://nautechsystems.io
//
//  Licensed under the GNU Lesser General Public License Version 3.0 (the "License");
//  You may not use this file except in compliance with the License.
//  You may obtain a copy of the License at https://www.gnu.org/licenses/lgpl-3.0.en.html
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.
// -------------------------------------------------------------------------------------------------

//! Example: sweeps strategy parameters and exports every run as JSON for a local workbench.
//!
//! The backtest engine is native, so a browser cannot run it. Sweeping the parameter grid here and
//! exporting every combination is what lets the workbench answer instantly: choosing parameters
//! selects a result that already exists, rather than waiting on a run.
//!
//! The equity series comes from [`PortfolioSnapshot`], not `BacktestResult::returns_series`. The
//! latter is daily, so a short intraday run yields at most a couple of points; the snapshot ring
//! samples at `snapshot_interval_ms` while a position is open, which is what a curve needs.
//!
//! Series are emitted as parallel arrays with second offsets, so a whole grid stays small enough to
//! inline into a single page.
//!
//! Run with: `cargo run --release -p nautilus-backtest --features examples --example workbench-export`

#[cfg(feature = "mimalloc")]
mod allocator;
mod workbench_data;

use std::{fs, path::PathBuf};

use nautilus_backtest::{
    config::{BacktestEngineConfig, SimulatedVenueConfig},
    engine::BacktestEngine,
};
use nautilus_model::{
    data::{Data, QuoteTick},
    enums::{AccountType, BookType, OmsType},
    identifiers::{InstrumentId, Venue},
    instruments::{Instrument, InstrumentAny, stubs::audusd_sim},
    types::{Currency, Money, Price, Quantity},
};
use nautilus_portfolio::config::PortfolioConfig;
use nautilus_trading::examples::strategies::{EmaCross, GridMarketMaker, GridMarketMakerConfig};
use serde::Serialize;

use workbench_data::SnapshotSeries;

const VENUE: &str = "SIM";
const STARTING_BALANCE: &str = "1_000_000 USD";
const STARTING_BALANCE_F64: f64 = 1_000_000.0;
/// Sampling cadence for the equity curve. Snapshots are only taken while a position is open.
const SNAPSHOT_INTERVAL_MS: u64 = 1_000;

/// A parameter axis the workbench exposes as a control.
#[derive(Serialize)]
struct Axis {
    key: &'static str,
    label: &'static str,
    unit: &'static str,
    values: Vec<i64>,
}

/// A strategy the workbench can select, with the axes swept for it.
#[derive(Serialize)]
struct StrategyDef {
    id: &'static str,
    label: &'static str,
    description: &'static str,
    axes: Vec<Axis>,
}

/// One point of the sweep, identified by its parameter values.
#[derive(Serialize)]
struct RunExport {
    strategy: &'static str,
    params: Vec<i64>,
    total_orders: u64,
    total_positions: u64,
    stats_pnls: serde_json::Value,
    stats_returns: serde_json::Value,
    stats_general: serde_json::Value,
    schema: &'static str,
    sampling_interval_ms: u64,
    #[serde(flatten)]
    series: SnapshotSeries,
}

#[derive(Serialize)]
struct Workbench {
    schema: &'static str,
    instrument: String,
    currency: &'static str,
    starting_balance: f64,
    strategies: Vec<StrategyDef>,
    runs: Vec<RunExport>,
}

fn quote(instrument_id: InstrumentId, bid: &str, ask: &str, ts: u64) -> Data {
    Data::Quote(QuoteTick::new(
        instrument_id,
        Price::from(bid),
        Price::from(ask),
        Quantity::from("100000"),
        Quantity::from("100000"),
        ts.into(),
        ts.into(),
    ))
}

/// Synthetic quotes: a slow drift carrying two oscillation scales, so a trend follower and a
/// mean-reverting quoter both have something to act on.
fn generate_quotes(instrument_id: InstrumentId) -> Vec<Data> {
    let mut data = Vec::with_capacity(900);
    let mut ts: u64 = 1_735_689_600_000_000_000; // 2025-01-01T00:00:00Z

    for i in 0..900 {
        let x = f64::from(i);
        let mid = 0.670 + 0.004 * (x / 40.0).sin() + 0.0012 * (x / 7.0).sin() + 0.000_015 * x;
        let bid = format!("{:.5}", mid - 0.000_05);
        let ask = format!("{:.5}", mid + 0.000_05);
        data.push(quote(instrument_id, &bid, &ask, ts));
        ts += 1_000_000_000; // one second
    }

    data
}

fn strategies() -> Vec<StrategyDef> {
    vec![
        StrategyDef {
            id: "ema_cross",
            label: "EMA Cross",
            description: "Takes the fast/slow crossover and reverses on the opposite cross.",
            axes: vec![
                Axis {
                    key: "fast",
                    label: "Fast period",
                    unit: "ticks",
                    values: vec![5, 10, 15, 20],
                },
                Axis {
                    key: "slow",
                    label: "Slow period",
                    unit: "ticks",
                    values: vec![30, 50, 80, 120],
                },
            ],
        },
        StrategyDef {
            id: "grid_mm",
            label: "Grid Market Maker",
            description: "Quotes a symmetric ladder around mid and re-quotes as mid drifts.",
            axes: vec![
                Axis {
                    key: "levels",
                    label: "Grid levels",
                    unit: "per side",
                    values: vec![1, 2, 3, 5],
                },
                Axis {
                    key: "step",
                    label: "Grid step",
                    unit: "bps",
                    values: vec![1, 2, 5, 10],
                },
            ],
        },
    ]
}

fn build_engine() -> anyhow::Result<(BacktestEngine, InstrumentId)> {
    let config = BacktestEngineConfig::builder()
        .portfolio(PortfolioConfig {
            snapshot_interval_ms: Some(SNAPSHOT_INTERVAL_MS),
            ..PortfolioConfig::default()
        })
        .build();
    let mut engine = BacktestEngine::new(config)?;

    engine.add_venue(
        SimulatedVenueConfig::builder()
            .venue(Venue::from(VENUE))
            .oms_type(OmsType::Hedging)
            .account_type(AccountType::Margin)
            .book_type(BookType::L1_MBP)
            .starting_balances(vec![Money::from(STARTING_BALANCE)])
            .build()?,
    )?;

    let instrument = InstrumentAny::CurrencyPair(audusd_sim());
    let instrument_id = instrument.id();
    engine.add_instrument(&instrument)?;

    Ok((engine, instrument_id))
}

fn run_point(strategy: &'static str, params: &[i64]) -> anyhow::Result<RunExport> {
    let (mut engine, instrument_id) = build_engine()?;

    match strategy {
        "ema_cross" => {
            engine.add_strategy(EmaCross::new(
                instrument_id,
                Quantity::from("100000"),
                usize::try_from(params[0])?,
                usize::try_from(params[1])?,
            ))?;
        }
        "grid_mm" => {
            engine.add_strategy(GridMarketMaker::new(
                GridMarketMakerConfig::builder()
                    .instrument_id(instrument_id)
                    .trade_size(Quantity::from("50000"))
                    .num_levels(usize::try_from(params[0])?)
                    .grid_step_bps(u32::try_from(params[1])?)
                    .max_position(Quantity::from("500000"))
                    .build(),
            ))?;
        }
        other => anyhow::bail!("unknown strategy {other}"),
    }

    engine.add_data(generate_quotes(instrument_id), None, true, true)?;
    engine.run(None, None, None, false)?;

    let result = engine.get_result();

    let series = {
        let kernel = engine.kernel_mut();
        let portfolio = kernel.portfolio.borrow();
        let cache = kernel.cache.borrow();
        let snapshots = cache
            .accounts_all_owned()
            .iter()
            .flat_map(|account| portfolio.snapshots(&account.id()))
            .collect();
        SnapshotSeries::new(snapshots, Currency::USD())?
    };

    Ok(RunExport {
        strategy,
        params: params.to_vec(),
        total_orders: result.total_orders as u64,
        total_positions: result.total_positions as u64,
        stats_pnls: serde_json::to_value(&result.stats_pnls)?,
        stats_returns: serde_json::to_value(&result.stats_returns)?,
        stats_general: serde_json::to_value(&result.stats_general)?,
        schema: "nautilus-workbench/v3",
        sampling_interval_ms: SNAPSHOT_INTERVAL_MS,
        series,
    })
}

fn main() -> anyhow::Result<()> {
    #[cfg(feature = "mimalloc")]
    allocator::register();

    let defs = strategies();
    let mut runs = Vec::new();

    for def in &defs {
        // Two axes per strategy; their cartesian product is the sweep.
        for a in &def.axes[0].values {
            for b in &def.axes[1].values {
                let params = vec![*a, *b];
                print!("{} {params:?} ... ", def.id);
                match run_point(def.id, &params) {
                    Ok(run) => {
                        println!(
                            "{} pos, {} equity points",
                            run.total_positions,
                            run.series.eq.len()
                        );
                        runs.push(run);
                    }
                    // One failing point must not lose the rest of the sweep.
                    Err(e) => println!("FAILED: {e}"),
                }
            }
        }
    }

    let total = runs.len();
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "workbench_data.json".to_string()),
    );
    fs::write(
        &out,
        serde_json::to_string(&Workbench {
            schema: "nautilus-workbench/v3",
            instrument: "AUD/USD.SIM".to_string(),
            currency: "USD",
            starting_balance: STARTING_BALANCE_F64,
            strategies: defs,
            runs,
        })?,
    )?;
    println!("wrote {} ({total} runs)", out.display());

    Ok(())
}
