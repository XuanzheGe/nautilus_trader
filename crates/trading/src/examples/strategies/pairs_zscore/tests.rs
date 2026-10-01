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

use std::collections::VecDeque;

use nautilus_common::actor::DataActor;
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::QuoteTick,
    identifiers::InstrumentId,
    instruments::{InstrumentAny, stubs::default_fx_ccy},
    types::{Price, Quantity},
};
use rstest::rstest;

use super::{PairsZScore, PairsZScoreConfig, strategy::spread_state};

fn instrument_id_a() -> InstrumentId {
    InstrumentId::from("EUR/USD.SIM")
}

fn instrument_id_b() -> InstrumentId {
    InstrumentId::from("USD/JPY.SIM")
}

fn create_strategy(lookback: usize) -> PairsZScore {
    PairsZScore::new(
        PairsZScoreConfig::builder()
            .instrument_id_a(instrument_id_a())
            .instrument_id_b(instrument_id_b())
            .trade_size_a(Quantity::from("100000"))
            .lookback(lookback)
            .entry_z(2.0)
            .exit_z(0.5)
            .build(),
    )
}

fn quote(instrument_id: InstrumentId, bid: &str, ask: &str, precision: u8) -> QuoteTick {
    QuoteTick::new(
        instrument_id,
        Price::new(bid.parse().unwrap(), precision),
        Price::new(ask.parse().unwrap(), precision),
        Quantity::from("100000"),
        Quantity::from("100000"),
        UnixNanos::default(),
        UnixNanos::default(),
    )
}

fn windows(pairs: &[(f64, f64)]) -> (VecDeque<f64>, VecDeque<f64>) {
    (
        pairs.iter().map(|(a, _)| *a).collect(),
        pairs.iter().map(|(_, b)| *b).collect(),
    )
}

#[rstest]
fn test_spread_state_rejects_window_below_two_samples() {
    let (log_a, log_b) = windows(&[(1.0, 2.0)]);
    assert!(spread_state(&log_a, &log_b).is_none());
}

#[rstest]
fn test_spread_state_rejects_constant_independent_leg() {
    let (log_a, log_b) = windows(&[(1.0, 5.0), (2.0, 5.0), (3.0, 5.0), (4.0, 5.0)]);
    assert!(spread_state(&log_a, &log_b).is_none());
}

#[rstest]
fn test_spread_state_rejects_exact_fit() {
    // Residuals are identically zero, so no z-score is defined.
    let (log_a, log_b) = windows(&[(3.0, 1.0), (5.0, 2.0), (7.0, 3.0), (9.0, 4.0)]);
    assert!(spread_state(&log_a, &log_b).is_none());
}

#[rstest]
fn test_spread_state_recovers_known_slope_and_intercept() {
    // a = 1 + 2b with a single unit displacement on the final sample.
    let (log_a, log_b) = windows(&[(3.0, 1.0), (5.0, 2.0), (7.0, 3.0), (10.0, 4.0)]);
    let state = spread_state(&log_a, &log_b).unwrap();

    assert!((state.beta - 2.3).abs() < 1e-12, "beta was {}", state.beta);
    assert!(
        (state.intercept - 0.5).abs() < 1e-12,
        "intercept was {}",
        state.intercept
    );
    // The displaced final sample sits above the fit, so the residual is positive.
    assert!(state.residual > 0.0);
    assert!(state.z > 0.0);
    assert!((state.z - state.residual / state.sigma).abs() < 1e-12);
}

#[rstest]
fn test_spread_state_window_residuals_have_zero_mean() {
    let (log_a, log_b) = windows(&[(3.1, 1.0), (4.8, 2.0), (7.3, 3.0), (8.9, 4.0), (11.2, 5.0)]);
    let state = spread_state(&log_a, &log_b).unwrap();

    let mean: f64 = log_a
        .iter()
        .zip(log_b.iter())
        .map(|(a, b)| a - state.beta.mul_add(*b, state.intercept))
        .sum::<f64>()
        / log_a.len() as f64;

    assert!(mean.abs() < 1e-12, "residual mean was {mean}");
}

#[rstest]
fn test_spread_state_is_sign_symmetric_in_the_dependent_leg() {
    let (log_a, log_b) = windows(&[(3.0, 1.0), (5.0, 2.0), (7.0, 3.0), (10.0, 4.0)]);
    let above = spread_state(&log_a, &log_b).unwrap();

    // Mirroring leg A about its mean flips the slope and the residual, not the scale.
    let mean_a = log_a.iter().sum::<f64>() / log_a.len() as f64;
    let mirrored: VecDeque<f64> = log_a.iter().map(|a| 2.0 * mean_a - a).collect();
    let below = spread_state(&mirrored, &log_b).unwrap();

    assert!((below.beta + above.beta).abs() < 1e-12);
    assert!((below.z + above.z).abs() < 1e-12);
    assert!(below.z < 0.0);
}

#[rstest]
fn test_push_sample_waits_for_both_legs() {
    let mut strategy = create_strategy(4);

    strategy.mid_a = Some(1.1400);
    strategy.push_sample();
    assert!(strategy.log_a.is_empty());
    assert!(strategy.log_b.is_empty());

    strategy.mid_b = Some(109.700);
    strategy.push_sample();
    assert_eq!(strategy.log_a.len(), 1);
    assert_eq!(strategy.log_b.len(), 1);
    assert!((strategy.log_a[0] - 1.1400_f64.ln()).abs() < 1e-12);
}

#[rstest]
fn test_push_sample_bounds_the_window_at_lookback() {
    let mut strategy = create_strategy(3);
    strategy.mid_b = Some(109.700);

    for i in 0..6 {
        strategy.mid_a = Some(1.1400 + f64::from(i) * 0.0001);
        strategy.push_sample();
    }

    assert_eq!(strategy.log_a.len(), 3);
    assert_eq!(strategy.log_b.len(), 3);
    // The oldest three samples were evicted, leaving the newest first.
    assert!((strategy.log_a[0] - (1.1400_f64 + 3.0 * 0.0001).ln()).abs() < 1e-12);
}

#[rstest]
fn test_on_quote_ignores_an_unrelated_instrument() {
    let mut strategy = create_strategy(4);
    let unrelated = quote(InstrumentId::from("GBP/USD.SIM"), "1.30000", "1.30010", 5);

    strategy.on_quote(&unrelated).unwrap();

    assert!(strategy.mid_a.is_none());
    assert!(strategy.mid_b.is_none());
    assert!(strategy.log_a.is_empty());
}

#[rstest]
fn test_on_quote_records_each_leg_mid() {
    let mut strategy = create_strategy(4);

    strategy
        .on_quote(&quote(instrument_id_a(), "1.14000", "1.14020", 5))
        .unwrap();
    strategy
        .on_quote(&quote(instrument_id_b(), "109.700", "109.720", 3))
        .unwrap();

    assert!((strategy.mid_a.unwrap() - 1.14010).abs() < 1e-9);
    assert!((strategy.mid_b.unwrap() - 109.710).abs() < 1e-9);
    assert_eq!(strategy.log_a.len(), 1);
}

#[rstest]
fn test_hedge_quantity_needs_a_resolved_instrument() {
    let strategy = create_strategy(4);
    assert!(strategy.hedge_quantity(1.0).is_none());
}

#[rstest]
#[case(1.0, "100000")]
#[case(0.5, "50000")]
#[case(-1.5, "150000")]
fn test_hedge_quantity_scales_with_the_absolute_hedge_ratio(
    #[case] beta: f64,
    #[case] expected: &str,
) {
    let mut strategy = create_strategy(4);
    strategy.instrument_b = Some(InstrumentAny::CurrencyPair(default_fx_ccy(
        instrument_id_b().symbol,
        Some(instrument_id_b().venue),
    )));

    assert_eq!(
        strategy.hedge_quantity(beta).unwrap(),
        Quantity::from(expected)
    );
}

#[rstest]
fn test_hedge_quantity_is_capped_by_max_hedge_ratio() {
    let mut strategy = PairsZScore::new(
        PairsZScoreConfig::builder()
            .instrument_id_a(instrument_id_a())
            .instrument_id_b(instrument_id_b())
            .trade_size_a(Quantity::from("100000"))
            .max_hedge_ratio(2.0)
            .build(),
    );
    strategy.instrument_b = Some(InstrumentAny::CurrencyPair(default_fx_ccy(
        instrument_id_b().symbol,
        Some(instrument_id_b().venue),
    )));

    assert_eq!(
        strategy.hedge_quantity(50.0).unwrap(),
        Quantity::from("200000")
    );
}

#[rstest]
fn test_hedge_quantity_rejects_a_size_below_the_instrument_minimum() {
    let mut strategy = PairsZScore::new(
        PairsZScoreConfig::builder()
            .instrument_id_a(instrument_id_a())
            .instrument_id_b(instrument_id_b())
            .trade_size_a(Quantity::from("100"))
            .build(),
    );
    strategy.instrument_b = Some(InstrumentAny::CurrencyPair(default_fx_ccy(
        instrument_id_b().symbol,
        Some(instrument_id_b().venue),
    )));

    // `default_fx_ccy` carries a minimum quantity of 100, so a tenth of leg A's size
    // rounds to 10 and is not tradable.
    assert!(strategy.hedge_quantity(0.1).is_none());
}
