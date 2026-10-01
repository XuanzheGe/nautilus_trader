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

//! Configuration for the pairs z-score mean reversion strategy.

use nautilus_model::{
    enums::OmsType,
    identifiers::{InstrumentId, StrategyId},
    types::Quantity,
};

use crate::strategy::StrategyConfig;

/// Configuration for the pairs z-score mean reversion strategy.
#[derive(Debug, Clone, bon::Builder)]
#[cfg_attr(
    feature = "python",
    pyo3::pyclass(module = "nautilus_trader.trading", from_py_object)
)]
#[cfg_attr(
    feature = "python",
    pyo3_stub_gen::derive::gen_stub_pyclass(module = "nautilus_trader.trading")
)]
pub struct PairsZScoreConfig {
    /// Base strategy configuration.
    ///
    /// Defaults to `Netting` so each leg carries a single net position, which is
    /// what makes the two legs readable as one spread position.
    #[builder(default = StrategyConfig {
        strategy_id: Some(StrategyId::from("PAIRS_ZSCORE-001")),
        order_id_tag: Some("001".to_string()),
        oms_type: Some(OmsType::Netting),
        ..Default::default()
    })]
    pub base: StrategyConfig,
    /// Dependent leg of the regression; the leg whose quantity is fixed at `trade_size_a`.
    pub instrument_id_a: InstrumentId,
    /// Independent leg of the regression; the leg whose quantity is derived from the hedge ratio.
    pub instrument_id_b: InstrumentId,
    /// Quantity traded on leg A per entry.
    pub trade_size_a: Quantity,
    /// Number of synchronized samples in the rolling regression window.
    #[builder(default = 240)]
    pub lookback: usize,
    /// Absolute residual z-score at which a flat strategy opens the spread.
    #[builder(default = 2.0)]
    pub entry_z: f64,
    /// Absolute residual z-score at which an open spread is closed. Must be below
    /// `entry_z`; the gap between the two is the hysteresis band that stops the
    /// strategy from churning around the threshold.
    #[builder(default = 0.5)]
    pub exit_z: f64,
    /// Optional absolute z-score at which an open spread is abandoned. A pairs trade
    /// that keeps diverging is the one that ruins the strategy, and nothing in the
    /// entry rule bounds that divergence. When `None`, positions are only closed on
    /// reversion or at `on_stop`.
    pub stop_z: Option<f64>,
    /// Static conversion factor from one unit of leg A quantity to the equivalent
    /// leg B quantity at a hedge ratio of 1.
    ///
    /// The regression hedge ratio is dimensionless, so it cannot by itself bridge
    /// two legs whose quantities are denominated differently (different base
    /// currency, contract size, or lot multiplier). This factor carries that
    /// conversion and the strategy does not attempt to infer it.
    #[builder(default = 1.0)]
    pub hedge_scale: f64,
    /// Cap on the absolute hedge ratio used for sizing.
    ///
    /// A regression against a near-constant independent leg produces an arbitrarily
    /// large slope, which would size leg B without bound. The cap bounds the damage
    /// from a degenerate window; it does not make the signal meaningful there.
    #[builder(default = 5.0)]
    pub max_hedge_ratio: f64,
}
