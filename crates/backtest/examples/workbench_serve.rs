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

//! Example: serves a local workbench that runs a backtest per request.
//!
//! The engine is native, so a page cannot run it. This serves the page and an endpoint beside it,
//! so moving a control runs a real backtest with those parameters and draws the result. A single
//! run takes well under a second, which is why arbitrary parameters work here while the exported
//! static page is limited to a pre-computed grid.
//!
//! Serving the page from the same origin as the endpoint is what avoids CORS entirely; this is also
//! why the published artifact cannot do this, as it may not reach a local port.
//!
//! Deliberately built on [`std::net::TcpListener`] rather than a web framework: one local user,
//! two routes, and no reason to put an HTTP stack into the engine's dependency graph.
//!
//! The pairs scenario reads recorded quote ticks whose prices are 128-bit fixed point,
//! which is why the whole example requires `high-precision`.
//!
//! Run with:
//! `cargo run --release -p nautilus-backtest --features examples,high-precision --example workbench-serve`

#[cfg(feature = "mimalloc")]
mod allocator;
mod recorded_fx;
mod workbench_data;

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::OnceLock,
    time::Duration,
};

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
use nautilus_trading::examples::strategies::{
    EmaCross, GridMarketMaker, GridMarketMakerConfig, PairsZScore, PairsZScoreConfig,
};
use serde::Serialize;

use workbench_data::SnapshotSeries;

const ADDR: &str = "127.0.0.1:8787";
const VENUE: &str = "SIM";
const STARTING_BALANCE: &str = "1_000_000 USD";
const STARTING_BALANCE_F64: f64 = 1_000_000.0;
const SNAPSHOT_INTERVAL_MS: u64 = 1_000;
const PAGE: &str = include_str!("workbench.html");

/// Exit band for the pairs strategy; the page varies only the entry threshold.
const PAIRS_EXIT_Z: f64 = 0.5;

/// Recorded quote ticks, decoded once and shared by every pairs run.
///
/// Decoding costs far more than a backtest over the same data, so a per-request
/// decode would dominate the response time the workbench is built around.
static REAL_QUOTES: OnceLock<Vec<Data>> = OnceLock::new();

#[derive(Serialize)]
struct RunResponse {
    strategy: String,
    params: Vec<i64>,
    instrument: String,
    /// Where the quotes came from, so the page never presents synthetic data as real.
    data_source: &'static str,
    quotes: usize,
    currency: &'static str,
    starting_balance: f64,
    total_orders: u64,
    total_positions: u64,
    stats_pnls: serde_json::Value,
    stats_returns: serde_json::Value,
    stats_general: serde_json::Value,
    schema: &'static str,
    sampling_interval_ms: u64,
    #[serde(flatten)]
    series: SnapshotSeries,
    elapsed_ms: u128,
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
fn generate_quotes(instrument_id: InstrumentId, bars: u32) -> Vec<Data> {
    let mut data = Vec::with_capacity(bars as usize);
    let mut ts: u64 = 1_735_689_600_000_000_000; // 2025-01-01T00:00:00Z

    for i in 0..bars {
        let x = f64::from(i);
        let mid = 0.670 + 0.004 * (x / 40.0).sin() + 0.0012 * (x / 7.0).sin() + 0.000_015 * x;
        let bid = format!("{:.5}", mid - 0.000_05);
        let ask = format!("{:.5}", mid + 0.000_05);
        data.push(quote(instrument_id, &bid, &ask, ts));
        ts += 1_000_000_000;
    }

    data
}

/// One runnable configuration: the venue it trades on, the instruments it needs, and
/// the data it sees. Strategies differ in all three, so they are resolved together.
struct Scenario {
    venue: SimulatedVenueConfig,
    instruments: Vec<InstrumentAny>,
    data: Vec<Data>,
    label: String,
    source: &'static str,
}

fn synthetic_venue() -> anyhow::Result<SimulatedVenueConfig> {
    Ok(SimulatedVenueConfig::builder()
        .venue(Venue::from(VENUE))
        .oms_type(OmsType::Hedging)
        .account_type(AccountType::Margin)
        .book_type(BookType::L1_MBP)
        .starting_balances(vec![Money::from(STARTING_BALANCE)])
        .build()?)
}

fn synthetic_scenario(bars: u32) -> anyhow::Result<Scenario> {
    let instrument = InstrumentAny::CurrencyPair(audusd_sim());
    let data = generate_quotes(instrument.id(), bars);
    Ok(Scenario {
        venue: synthetic_venue()?,
        label: instrument.id().to_string(),
        instruments: vec![instrument],
        data,
        source: "synthetic",
    })
}

fn pairs_scenario(quotes: usize) -> anyhow::Result<Scenario> {
    let all = REAL_QUOTES
        .get()
        .ok_or_else(|| anyhow::anyhow!("recorded quotes were not loaded at startup"))?;
    anyhow::ensure!(!all.is_empty(), "no recorded quotes were decoded");

    let (instrument_a, instrument_b) = recorded_fx::instruments();

    // A single base currency keeps equity on one axis; the two legs settle in USD and
    // JPY, so a multi-currency account would report two equity figures per snapshot.
    let venue = SimulatedVenueConfig::builder()
        .venue(Venue::from(VENUE))
        .oms_type(OmsType::Netting)
        .account_type(AccountType::Margin)
        .base_currency(Currency::USD())
        .book_type(BookType::L1_MBP)
        .starting_balances(vec![Money::from(STARTING_BALANCE)])
        .build()?;

    Ok(Scenario {
        venue,
        label: format!("{} × {}", instrument_a.id(), instrument_b.id()),
        instruments: vec![instrument_a, instrument_b],
        data: all[..quotes.min(all.len())].to_vec(),
        source: "recorded 2019 ticks",
    })
}

fn run_backtest(strategy: &str, p0: i64, p1: i64, bars: u32) -> anyhow::Result<RunResponse> {
    let started = std::time::Instant::now();

    let Scenario {
        venue,
        instruments,
        data,
        label,
        source,
    } = match strategy {
        "pairs_zscore" => pairs_scenario(bars as usize)?,
        _ => synthetic_scenario(bars)?,
    };

    let config = BacktestEngineConfig::builder()
        .portfolio(PortfolioConfig {
            snapshot_interval_ms: Some(SNAPSHOT_INTERVAL_MS),
            ..PortfolioConfig::default()
        })
        .build();
    let mut engine = BacktestEngine::new(config)?;

    engine.add_venue(venue)?;
    for instrument in &instruments {
        engine.add_instrument(instrument)?;
    }
    let instrument_id = instruments[0].id();

    match strategy {
        "ema_cross" => {
            anyhow::ensure!((2..=400).contains(&p0), "fast period out of range");
            anyhow::ensure!(p1 > p0 && p1 <= 600, "slow period must exceed fast");
            engine.add_strategy(EmaCross::new(
                instrument_id,
                Quantity::from("100000"),
                usize::try_from(p0)?,
                usize::try_from(p1)?,
            ))?;
        }
        "grid_mm" => {
            anyhow::ensure!((1..=10).contains(&p0), "grid levels out of range");
            anyhow::ensure!((1..=50).contains(&p1), "grid step out of range");
            engine.add_strategy(GridMarketMaker::new(
                GridMarketMakerConfig::builder()
                    .instrument_id(instrument_id)
                    .trade_size(Quantity::from("50000"))
                    .num_levels(usize::try_from(p0)?)
                    .grid_step_bps(u32::try_from(p1)?)
                    .max_position(Quantity::from("500000"))
                    .build(),
            ))?;
        }
        "pairs_zscore" => {
            anyhow::ensure!((60..=4_000).contains(&p0), "lookback out of range");
            anyhow::ensure!((10..=50).contains(&p1), "entry z-score out of range");
            // The page exposes the entry threshold in tenths so it can ride an integer
            // control; exit and stop are pinned to it rather than given their own axes.
            let entry_z = p1 as f64 / 10.0;
            engine.add_strategy(PairsZScore::new(
                PairsZScoreConfig::builder()
                    .instrument_id_a(instrument_id)
                    .instrument_id_b(instruments[1].id())
                    .trade_size_a(Quantity::from("100000"))
                    .lookback(usize::try_from(p0)?)
                    .entry_z(entry_z)
                    .exit_z(PAIRS_EXIT_Z)
                    .stop_z(entry_z * 2.0)
                    .build(),
            ))?;
        }
        other => anyhow::bail!("unknown strategy '{other}'"),
    }

    let quotes = data.len();
    engine.add_data(data, None, true, true)?;
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

    Ok(RunResponse {
        strategy: strategy.to_string(),
        params: vec![p0, p1],
        instrument: label,
        data_source: source,
        quotes,
        currency: "USD",
        starting_balance: STARTING_BALANCE_F64,
        total_orders: result.total_orders as u64,
        total_positions: result.total_positions as u64,
        stats_pnls: serde_json::to_value(&result.stats_pnls)?,
        stats_returns: serde_json::to_value(&result.stats_returns)?,
        stats_general: serde_json::to_value(&result.stats_general)?,
        schema: "nautilus-workbench/v3",
        sampling_interval_ms: SNAPSHOT_INTERVAL_MS,
        series,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

fn query(target: &str, key: &str) -> Option<String> {
    let (_, qs) = target.split_once('?')?;
    qs.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

fn run_parameters(target: &str) -> anyhow::Result<(String, i64, i64, u32)> {
    let strategy = query(target, "strategy").unwrap_or_else(|| "ema_cross".to_string());
    anyhow::ensure!(
        matches!(strategy.as_str(), "ema_cross" | "grid_mm" | "pairs_zscore"),
        "unknown strategy '{strategy}'"
    );
    let integer = |key: &str, default: i64| -> anyhow::Result<i64> {
        query(target, key).map_or(Ok(default), |value| {
            value
                .parse()
                .map_err(|_| anyhow::anyhow!("{key} must be an integer"))
        })
    };
    let p0 = integer("p0", 10)?;
    let p1 = integer("p1", 30)?;
    let ceiling = if strategy == "pairs_zscore" {
        20_000
    } else {
        5_000
    };
    let bars = integer("bars", 900)?;
    anyhow::ensure!(
        (120..=ceiling).contains(&bars),
        "data length must be between 120 and {ceiling}"
    );
    Ok((strategy, p0, p1, u32::try_from(bars)?))
}

fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &str) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

fn handle(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    })
    .take(8193);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.len() > 8192 {
        return;
    }
    let mut request = line.split_whitespace();
    if request.next() != Some("GET") {
        respond(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            "only GET is supported",
        );
        return;
    }
    let target = request.next().unwrap_or("/").to_string();

    if target == "/" || target.starts_with("/?") {
        respond(&mut stream, "200 OK", "text/html; charset=utf-8", PAGE);
        return;
    }

    if target.split('?').next() == Some("/api/run") {
        let (strategy, p0, p1, bars) = match run_parameters(&target) {
            Ok(parameters) => parameters,
            Err(e) => {
                respond(
                    &mut stream,
                    "400 Bad Request",
                    "application/json",
                    &serde_json::json!({"error": e.to_string()}).to_string(),
                );
                return;
            }
        };

        let body = match run_backtest(&strategy, p0, p1, bars) {
            Ok(run) => {
                println!(
                    "{strategy} [{p0},{p1}] bars={bars} -> {} pos in {}ms",
                    run.total_positions, run.elapsed_ms
                );
                serde_json::to_string(&run)
                    .unwrap_or_else(|e| serde_json::json!({"error": e.to_string()}).to_string())
            }
            Err(e) => {
                println!("{strategy} [{p0},{p1}] rejected: {e}");
                respond(
                    &mut stream,
                    "400 Bad Request",
                    "application/json",
                    &serde_json::json!({"error": e.to_string()}).to_string(),
                );
                return;
            }
        };
        respond(&mut stream, "200 OK", "application/json", &body);
        return;
    }

    respond(&mut stream, "404 Not Found", "text/plain", "not found");
}

fn main() -> anyhow::Result<()> {
    #[cfg(feature = "mimalloc")]
    allocator::register();

    match recorded_fx::load() {
        Ok(quotes) => {
            println!(
                "decoded {} recorded quote ticks for the pairs scenario",
                quotes.len()
            );
            REAL_QUOTES
                .set(quotes)
                .map_err(|_| anyhow::anyhow!("recorded quotes were already loaded"))?;
        }
        Err(e) => {
            eprintln!("Recorded pairs data unavailable: {e}; synthetic scenarios remain available")
        }
    }

    let address: SocketAddr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ADDR.to_string())
        .parse()?;
    anyhow::ensure!(
        address.ip().is_loopback(),
        "workbench must listen on a loopback address"
    );
    let listener = TcpListener::bind(address)?;
    println!("workbench listening on http://{}", listener.local_addr()?);
    println!("each control change runs a real backtest; Ctrl-C to stop");

    for stream in listener.incoming() {
        match stream {
            Ok(s) => handle(s),
            Err(e) => eprintln!("connection failed: {e}"),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("/api/run?p0=oops")]
    #[case("/api/run?p1=1.5")]
    #[case("/api/run?bars=-120")]
    #[case("/api/run?bars=5001")]
    #[case("/api/run?strategy=unknown")]
    fn rejects_invalid_run_parameters(#[case] target: &str) {
        assert!(run_parameters(target).is_err());
    }

    #[rstest]
    fn accepts_strategy_specific_data_limit() {
        assert_eq!(
            run_parameters("/api/run?strategy=pairs_zscore&p0=240&p1=20&bars=20000").unwrap(),
            ("pairs_zscore".to_string(), 240, 20, 20_000)
        );
    }

    #[rstest]
    fn synthetic_run_records_floating_and_realized_pnl() {
        let run = run_backtest("ema_cross", 10, 30, 120).unwrap();
        let series = &run.series;
        let length = series.eq.len();
        assert!(length > 2);
        assert_eq!(series.ts_ns.len(), length);
        assert_eq!(series.un.len(), length);
        assert_eq!(series.realized.len(), length);
        assert!(series.un.iter().flatten().any(|pnl| *pnl != 0.0));
        assert!(series.t.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(series.eq[0], Some(STARTING_BALANCE_F64));
        assert!(!series.stale.iter().any(|stale| *stale));
        for ((equity, unrealized), realized) in series
            .equity_exact
            .iter()
            .zip(&series.unrealized_exact)
            .zip(&series.realized_exact)
        {
            assert_eq!(
                equity.unwrap().checked_sub(Money::from(STARTING_BALANCE)),
                realized.unwrap().checked_add(unrealized.unwrap())
            );
        }
    }
}
