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

//! Pairs z-score mean reversion strategy implementation.

use std::{collections::VecDeque, fmt::Debug};

use nautilus_common::actor::DataActor;
use nautilus_indicators::momentum::bb::fast_std_with_mean;
use nautilus_model::{
    data::QuoteTick,
    enums::OrderSide,
    instruments::{Instrument, InstrumentAny},
    types::Quantity,
};
use rust_decimal::Decimal;

use super::config::PairsZScoreConfig;
use crate::{
    nautilus_strategy,
    strategy::{Strategy, StrategyCore},
};

/// Smallest sample variance of the independent leg for which a slope is estimated.
///
/// Log prices of a liquid instrument carry a variance many orders of magnitude above
/// this over any usable window; reaching it means the leg is effectively constant.
const MIN_VARIANCE: f64 = 1e-16;

/// Smallest residual dispersion for which a z-score is formed.
const MIN_SIGMA: f64 = 1e-12;

/// Ordinary least squares fit of the dependent leg on the independent leg over the
/// rolling window, with the standing residual expressed as a z-score.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpreadState {
    /// Slope of log leg A on log leg B: the hedge ratio.
    pub beta: f64,
    /// Intercept of the fit.
    pub intercept: f64,
    /// Residual of the most recent sample.
    pub residual: f64,
    /// Population standard deviation of the window's residuals.
    pub sigma: f64,
    /// `residual / sigma`; the trading signal.
    pub z: f64,
}

/// Fits the window and returns its [`SpreadState`], or `None` when the fit is degenerate.
///
/// The regression carries an intercept, so the window's residuals have mean zero by
/// construction and `sigma` is their root mean square about zero rather than about a
/// separately estimated mean.
///
/// The most recent sample is part of the window it is scored against, which is the
/// conventional rolling in-sample form: it uses no future data, but the newest
/// observation does influence the mean and dispersion it is measured against.
#[must_use]
pub(super) fn spread_state(log_a: &VecDeque<f64>, log_b: &VecDeque<f64>) -> Option<SpreadState> {
    let n = log_a.len();
    if n < 2 || log_b.len() != n {
        return None;
    }

    let count = n as f64;
    let mean_a = log_a.iter().sum::<f64>() / count;
    let mean_b = log_b.iter().sum::<f64>() / count;

    let (cov, var_b) = log_a
        .iter()
        .zip(log_b.iter())
        .fold((0.0_f64, 0.0_f64), |(cov, var), (a, b)| {
            let deviation_b = b - mean_b;
            (
                (a - mean_a).mul_add(deviation_b, cov),
                deviation_b.mul_add(deviation_b, var),
            )
        });

    if var_b / count <= MIN_VARIANCE {
        return None;
    }

    let beta = cov / var_b;
    let intercept = beta.mul_add(-mean_b, mean_a);
    let residual_of = |a: f64, b: f64| a - beta.mul_add(b, intercept);

    let sigma = fast_std_with_mean(
        log_a
            .iter()
            .zip(log_b.iter())
            .map(|(a, b)| residual_of(*a, *b)),
        0.0,
    );
    if sigma <= MIN_SIGMA {
        return None;
    }

    let residual = residual_of(*log_a.back()?, *log_b.back()?);

    Some(SpreadState {
        beta,
        intercept,
        residual,
        sigma,
        z: residual / sigma,
    })
}

fn push_bounded(window: &mut VecDeque<f64>, value: f64, capacity: usize) {
    if window.len() == capacity {
        window.pop_front();
    }
    window.push_back(value);
}

/// Pairs z-score mean reversion strategy.
///
/// Regresses the log mid of leg A on the log mid of leg B over a rolling window and
/// trades the standing residual. A residual far above the fit means leg A is rich
/// against the relationship, so the strategy sells leg A and takes the hedging side
/// of leg B; far below is the mirror. The position is closed when the residual
/// returns inside `exit_z`, or abandoned at `stop_z` if one is configured.
///
/// Both legs are sampled together: each quote on either leg records one synchronized
/// observation using the latest mid of the other leg. That carries the slower leg
/// forward, which is the standard treatment for two asynchronous quote streams and
/// biases the fit towards the more frequently quoted leg.
///
/// The position state machine assumes this strategy is the only actor on its two
/// instruments. It records the side it intended and reads it back rather than
/// deriving it from fills, so external trading on either leg would desynchronize it;
/// an entry is nonetheless gated on the portfolio being flat on both legs, so a leg
/// stranded by a partial exit cannot be built on.
///
/// **Whether the two legs are actually cointegrated is an input to this strategy, not
/// an output of it.** Nothing here tests that the residual mean-reverts. Applied to a
/// pair whose relationship is not stable, the strategy will still trade, and its
/// results will describe that particular sample rather than an edge.
pub struct PairsZScore {
    pub(super) core: StrategyCore,
    pub(super) config: PairsZScoreConfig,
    pub(super) log_a: VecDeque<f64>,
    pub(super) log_b: VecDeque<f64>,
    pub(super) mid_a: Option<f64>,
    pub(super) mid_b: Option<f64>,
    pub(super) instrument_b: Option<InstrumentAny>,
    pub(super) side_a: Option<OrderSide>,
}

impl PairsZScore {
    /// Creates a new [`PairsZScore`] instance from config.
    #[must_use]
    pub fn new(config: PairsZScoreConfig) -> Self {
        Self {
            core: StrategyCore::new(config.base.clone()),
            log_a: VecDeque::with_capacity(config.lookback),
            log_b: VecDeque::with_capacity(config.lookback),
            config,
            mid_a: None,
            mid_b: None,
            instrument_b: None,
            side_a: None,
        }
    }

    /// Records one synchronized observation once both legs have quoted.
    pub(super) fn push_sample(&mut self) {
        let (Some(mid_a), Some(mid_b)) = (self.mid_a, self.mid_b) else {
            return;
        };
        if mid_a <= 0.0 || mid_b <= 0.0 {
            return;
        }

        let lookback = self.config.lookback;
        push_bounded(&mut self.log_a, mid_a.ln(), lookback);
        push_bounded(&mut self.log_b, mid_b.ln(), lookback);
    }

    /// Resolves the leg B quantity that hedges `trade_size_a` at the given hedge ratio,
    /// or `None` when it does not round to a tradable size.
    pub(super) fn hedge_quantity(&self, beta: f64) -> Option<Quantity> {
        let instrument = self.instrument_b.as_ref()?;
        let ratio = beta.abs().min(self.config.max_hedge_ratio);
        let raw = self.config.trade_size_a.as_f64() * ratio * self.config.hedge_scale;
        if !raw.is_finite() || raw <= 0.0 {
            return None;
        }

        let quantity = instrument.make_qty(raw, None);
        if quantity.is_zero()
            || instrument
                .min_quantity()
                .is_some_and(|min| quantity < min)
            || instrument
                .max_quantity()
                .is_some_and(|max| quantity > max)
        {
            return None;
        }

        Some(quantity)
    }

    fn is_flat(&self) -> bool {
        let portfolio = self.portfolio();
        portfolio.net_position(&self.config.instrument_id_a) == Decimal::ZERO
            && portfolio.net_position(&self.config.instrument_id_b) == Decimal::ZERO
    }

    fn enter(&mut self, side_a: OrderSide, beta: f64) -> anyhow::Result<()> {
        let Some(size_b) = self.hedge_quantity(beta) else {
            return Ok(());
        };

        // A positive hedge ratio means the legs move together, so the hedge takes the
        // opposite side; a negative one means they move against each other and the
        // hedge takes the same side.
        let side_b = if beta >= 0.0 {
            side_a.opposite()
        } else {
            side_a
        };

        let instrument_id_a = self.config.instrument_id_a;
        let instrument_id_b = self.config.instrument_id_b;
        let size_a = self.config.trade_size_a;

        let (order_a, order_b) = {
            let factory = self.order();
            (
                factory.market(
                    instrument_id_a,
                    side_a,
                    size_a,
                    None, // time_in_force
                    None, // reduce_only
                    None, // quote_quantity
                    None, // exec_algorithm_id
                    None, // exec_algorithm_params
                    None, // tags
                    None, // client_order_id
                ),
                factory.market(
                    instrument_id_b,
                    side_b,
                    size_b,
                    None, // time_in_force
                    None, // reduce_only
                    None, // quote_quantity
                    None, // exec_algorithm_id
                    None, // exec_algorithm_params
                    None, // tags
                    None, // client_order_id
                ),
            )
        };

        // Recorded before submitting so that a failure on the second leg still leaves
        // the strategy knowing it holds a position to unwind.
        self.side_a = Some(side_a);
        self.submit_order(order_a, None, None, None)?;
        self.submit_order(order_b, None, None, None)?;
        Ok(())
    }

    fn exit(&mut self) -> anyhow::Result<()> {
        let instrument_id_a = self.config.instrument_id_a;
        let instrument_id_b = self.config.instrument_id_b;
        self.side_a = None;
        self.close_all_positions(instrument_id_a, None, None, None, None, None, None, None)?;
        self.close_all_positions(instrument_id_b, None, None, None, None, None, None, None)
    }

    pub(super) fn evaluate(&mut self) -> anyhow::Result<()> {
        if self.log_a.len() < self.config.lookback {
            return Ok(());
        }

        let Some(state) = spread_state(&self.log_a, &self.log_b) else {
            return Ok(());
        };

        if self.side_a.is_some() {
            let reverted = state.z.abs() <= self.config.exit_z;
            let stopped = self
                .config
                .stop_z
                .is_some_and(|stop_z| state.z.abs() >= stop_z);
            return if reverted || stopped { self.exit() } else { Ok(()) };
        }

        if state.z.abs() < self.config.entry_z || !self.is_flat() {
            return Ok(());
        }

        // A residual above the fit means leg A is rich against leg B.
        let side_a = if state.z > 0.0 {
            OrderSide::Sell
        } else {
            OrderSide::Buy
        };
        self.enter(side_a, state.beta)
    }

    fn validate_config(&self) -> anyhow::Result<()> {
        let config = &self.config;
        anyhow::ensure!(
            config.instrument_id_a != config.instrument_id_b,
            "`instrument_id_a` and `instrument_id_b` must differ, both were {}",
            config.instrument_id_a
        );
        anyhow::ensure!(
            config.lookback >= 2,
            "`lookback` must be at least 2, was {}",
            config.lookback
        );
        anyhow::ensure!(
            config.exit_z >= 0.0,
            "`exit_z` must not be negative, was {}",
            config.exit_z
        );
        anyhow::ensure!(
            config.entry_z > config.exit_z,
            "`entry_z` must exceed `exit_z`, was {} against {}",
            config.entry_z,
            config.exit_z
        );
        if let Some(stop_z) = config.stop_z {
            anyhow::ensure!(
                stop_z > config.entry_z,
                "`stop_z` must exceed `entry_z`, was {stop_z} against {}",
                config.entry_z
            );
        }
        anyhow::ensure!(
            config.hedge_scale > 0.0,
            "`hedge_scale` must be positive, was {}",
            config.hedge_scale
        );
        anyhow::ensure!(
            config.max_hedge_ratio > 0.0,
            "`max_hedge_ratio` must be positive, was {}",
            config.max_hedge_ratio
        );
        Ok(())
    }
}

nautilus_strategy!(PairsZScore);

impl Debug for PairsZScore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct(stringify!(PairsZScore))
            .field("instrument_id_a", &self.config.instrument_id_a)
            .field("instrument_id_b", &self.config.instrument_id_b)
            .field("lookback", &self.config.lookback)
            .field("entry_z", &self.config.entry_z)
            .field("exit_z", &self.config.exit_z)
            .field("side_a", &self.side_a)
            .finish()
    }
}

impl DataActor for PairsZScore {
    fn on_start(&mut self) -> anyhow::Result<()> {
        self.validate_config()?;

        let instrument_id_a = self.config.instrument_id_a;
        let instrument_id_b = self.config.instrument_id_b;

        let instrument_b = {
            let cache = self.cache();
            cache.try_instrument(&instrument_id_b)?
        };
        self.instrument_b = Some(instrument_b);

        self.subscribe_quotes(instrument_id_a, None, None);
        self.subscribe_quotes(instrument_id_b, None, None);
        Ok(())
    }

    fn on_stop(&mut self) -> anyhow::Result<()> {
        let instrument_id_a = self.config.instrument_id_a;
        let instrument_id_b = self.config.instrument_id_b;
        self.exit()?;
        self.unsubscribe_quotes(instrument_id_a, None, None);
        self.unsubscribe_quotes(instrument_id_b, None, None);
        Ok(())
    }

    fn on_quote(&mut self, quote: &QuoteTick) -> anyhow::Result<()> {
        let mid = f64::midpoint(quote.bid_price.as_f64(), quote.ask_price.as_f64());

        if quote.instrument_id == self.config.instrument_id_a {
            self.mid_a = Some(mid);
        } else if quote.instrument_id == self.config.instrument_id_b {
            self.mid_b = Some(mid);
        } else {
            return Ok(());
        }

        self.push_sample();
        self.evaluate()
    }

    fn on_reset(&mut self) -> anyhow::Result<()> {
        self.log_a.clear();
        self.log_b.clear();
        self.mid_a = None;
        self.mid_b = None;
        self.side_a = None;
        Ok(())
    }
}
