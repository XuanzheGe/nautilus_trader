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

//! Example: pairs z-score mean reversion on real 2019 FX quote ticks.
//!
//! Reads two parquet files of recorded quote ticks and runs [`PairsZScore`] across
//! them. The files carry 128-bit fixed-point prices, so the build must enable
//! `high-precision`; the instrument id and precisions come from each file's Arrow
//! schema metadata rather than a column. See [`recorded_fx`] for why the decoded
//! values need a scale correction before they are tradable.
//!
//! EUR/USD and USD/JPY are not a cointegrated pair in any established sense — they
//! share a dollar factor rather than a common underlying. The example prints a
//! diagnostic of the relationship before the run so the result can be read for what
//! it is: the behaviour of the strategy on this sample, not evidence of an edge.
//!
//! Run with:
//! `cargo run --release -p nautilus-backtest --features examples,high-precision --example pairs-zscore-fx`

#[cfg(feature = "mimalloc")]
mod allocator;
mod recorded_fx;

use nautilus_backtest::{
    config::{BacktestEngineConfig, SimulatedVenueConfig},
    engine::BacktestEngine,
};
use nautilus_model::{
    data::Data,
    enums::{AccountType, BookType, OmsType},
    instruments::Instrument,
    types::{Currency, Money, Quantity},
};
use nautilus_portfolio::config::PortfolioConfig;
use nautilus_trading::examples::strategies::{PairsZScore, PairsZScoreConfig};

const STARTING_BALANCE: &str = "1_000_000 USD";
const SNAPSHOT_INTERVAL_MS: u64 = 1_000;

const LOOKBACK: usize = 600;
const ENTRY_Z: f64 = 2.0;
const EXIT_Z: f64 = 0.5;
const STOP_Z: f64 = 4.0;
const TRADE_SIZE: &str = "100000";

/// Pearson correlation of two equal-length series.
fn correlation(xs: &[f64], ys: &[f64]) -> f64 {
    let n = xs.len() as f64;
    if xs.len() < 2 || xs.len() != ys.len() {
        return f64::NAN;
    }
    let mean_x = xs.iter().sum::<f64>() / n;
    let mean_y = ys.iter().sum::<f64>() / n;
    let (cov, var_x, var_y) = xs.iter().zip(ys).fold((0.0, 0.0, 0.0), |acc, (x, y)| {
        let dx = x - mean_x;
        let dy = y - mean_y;
        (dx.mul_add(dy, acc.0), dx.mul_add(dx, acc.1), dy.mul_add(dy, acc.2))
    });
    cov / (var_x * var_y).sqrt()
}

/// Samples both legs the way the strategy does — one observation per quote on either
/// leg, carrying the other leg's latest mid forward — and reports how the two relate.
fn report_relationship(data: &[Data], symbol_a: &str) {
    let (mut log_a, mut log_b) = (Vec::new(), Vec::new());
    let (mut mid_a, mut mid_b) = (None, None);
    let (mut count_a, mut count_b) = (0_usize, 0_usize);

    for item in data {
        let Data::Quote(quote) = item else { continue };
        let mid = f64::midpoint(quote.bid_price.as_f64(), quote.ask_price.as_f64());
        if quote.instrument_id.symbol.as_str() == symbol_a {
            mid_a = Some(mid);
            count_a += 1;
        } else {
            mid_b = Some(mid);
            count_b += 1;
        }
        if let (Some(a), Some(b)) = (mid_a, mid_b) {
            log_a.push(a.ln());
            log_b.push(b.ln());
        }
    }

    let level_corr = correlation(&log_a, &log_b);
    let returns_a: Vec<f64> = log_a.windows(2).map(|w| w[1] - w[0]).collect();
    let returns_b: Vec<f64> = log_b.windows(2).map(|w| w[1] - w[0]).collect();
    let return_corr = correlation(&returns_a, &returns_b);

    println!("\n--- PAIR DIAGNOSTIC (full sample, before the run) -------------------");
    println!("  quotes                       {count_a} leg A / {count_b} leg B");
    println!("  synchronized samples         {}", log_a.len());
    println!("  corr(log level A, log level B)   {level_corr:>8.4}");
    println!("  corr(log return A, log return B) {return_corr:>8.4}");
    println!(
        "  Level correlation is not evidence of a tradable relationship: two\n  \
         independent random walks produce high level correlation routinely. The\n  \
         return correlation is the one that speaks to a shared factor, and neither\n  \
         figure tests whether the residual mean-reverts. No stationarity test gates\n  \
         entry in this strategy."
    );
}

fn main() -> anyhow::Result<()> {
    #[cfg(feature = "mimalloc")]
    allocator::register();

    let data = recorded_fx::load()?;
    let (instrument_a, instrument_b) = recorded_fx::instruments();
    let venue = instrument_a.id().venue;

    report_relationship(&data, instrument_a.id().symbol.as_str());

    let config = BacktestEngineConfig::builder()
        .portfolio(PortfolioConfig {
            snapshot_interval_ms: Some(SNAPSHOT_INTERVAL_MS),
            ..PortfolioConfig::default()
        })
        .build();
    let mut engine = BacktestEngine::new(config)?;

    engine.add_venue(
        SimulatedVenueConfig::builder()
            .venue(venue)
            .oms_type(OmsType::Netting)
            .account_type(AccountType::Margin)
            .base_currency(Currency::USD())
            .book_type(BookType::L1_MBP)
            .starting_balances(vec![Money::from(STARTING_BALANCE)])
            .build()?,
    )?;

    engine.add_instrument(&instrument_a)?;
    engine.add_instrument(&instrument_b)?;

    engine.add_strategy(PairsZScore::new(
        PairsZScoreConfig::builder()
            .instrument_id_a(instrument_a.id())
            .instrument_id_b(instrument_b.id())
            .trade_size_a(Quantity::from(TRADE_SIZE))
            .lookback(LOOKBACK)
            .entry_z(ENTRY_Z)
            .exit_z(EXIT_Z)
            .stop_z(STOP_Z)
            .build(),
    ))?;

    engine.add_data(data, None, true, true)?;
    engine.run(None, None, None, false)?;

    Ok(())
}
