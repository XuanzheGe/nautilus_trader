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

//! Pairs z-score mean reversion strategy over two quote streams.
//!
//! Subscribes to quotes for two instruments and regresses the log mid of leg A on
//! the log mid of leg B over a rolling window. The standing residual, divided by the
//! window's residual dispersion, is the signal: the strategy opens the spread when
//! the residual sits beyond `entry_z` and closes it when the residual returns inside
//! `exit_z`, with an optional `stop_z` for a spread that keeps widening.
//!
//! The regression runs in log space so the two legs need no common price scale, and
//! it carries an intercept so the residuals have mean zero by construction. The
//! slope is the hedge ratio: leg A trades a fixed `trade_size_a` and leg B trades
//! `|beta| * hedge_scale` times that, on the opposite side when the slope is positive
//! and on the same side when it is negative. `hedge_scale` carries the unit
//! conversion between the two legs' quantities, which a dimensionless slope cannot.
//!
//! **The strategy assumes the pair is cointegrated; it does not establish it.** No
//! stationarity test gates entry, so on a pair whose relationship is unstable the
//! signal still fires and the result describes the sample rather than an edge.

pub mod config;
pub mod strategy;

#[cfg(test)]
mod tests;

pub use config::PairsZScoreConfig;
pub use strategy::{PairsZScore, SpreadState};
