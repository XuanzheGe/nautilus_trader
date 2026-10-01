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

use std::{collections::BTreeMap, fmt::Display};

use nautilus_core::{UnixNanos, datetime::NANOSECONDS_IN_DAY};
use nautilus_model::position::Position;

use crate::statistic::PortfolioStatistic;

/// Time taken to climb back from the deepest drawdown's trough to its prior peak, in days.
///
/// Pairs with max drawdown: depth says how far equity fell, this says how long it took to undo.
/// Measured from the trough of the single deepest episode, not from the peak, so it reports the
/// recovery leg only.
///
/// Returns `NaN` when the deepest drawdown had not recovered by the end of the series, because
/// an unrecovered drawdown has no recovery time and reporting `0` would read as instant recovery.
/// Returns `0` when the series never went underwater.
///
/// # References
///
/// - Bacon, C. R. (2008). *Practical Portfolio Performance Measurement and Attribution*
///   (2nd ed.). Wiley.
#[repr(C)]
#[derive(Debug, Clone, Default)]
#[cfg_attr(
    feature = "python",
    pyo3::pyclass(module = "nautilus_trader.analysis", from_py_object)
)]
#[cfg_attr(
    feature = "python",
    pyo3_stub_gen::derive::gen_stub_pyclass(module = "nautilus_trader.analysis")
)]
pub struct MaxDrawdownRecovery {}

impl MaxDrawdownRecovery {
    /// Creates a new [`MaxDrawdownRecovery`] instance.
    #[must_use]
    pub fn new() -> Self {
        Self {}
    }
}

impl Display for MaxDrawdownRecovery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Max Drawdown Recovery (days)")
    }
}

impl PortfolioStatistic for MaxDrawdownRecovery {
    type Item = f64;

    fn name(&self) -> String {
        self.to_string()
    }

    fn calculate_from_returns(&self, returns: &BTreeMap<UnixNanos, f64>) -> Option<Self::Item> {
        if returns.is_empty() {
            return Some(0.0);
        }

        let mut cumulative = 1.0;
        let mut running_max = 1.0;
        let mut worst_drawdown = 0.0;
        // Trough of the deepest episode seen so far. `pending` carries the level to regain.
        let mut worst_trough_ts: Option<UnixNanos> = None;
        // Recovery for the episode currently being tracked, once it completes.
        let mut recovery_ns: Option<u64> = None;
        let mut pending: Option<(UnixNanos, f64)> = None;

        for (&ts, &ret) in returns {
            cumulative *= 1.0 + ret;

            if cumulative >= running_max {
                // Peak regained: if the deepest episode was the one just closed, record its leg.
                if let Some((trough_ts, trough_peak)) = pending
                    && cumulative >= trough_peak
                {
                    recovery_ns = Some(ts.as_u64().saturating_sub(trough_ts.as_u64()));
                    pending = None;
                }
                running_max = cumulative;
                continue;
            }

            let drawdown = (running_max - cumulative) / running_max;
            if drawdown > worst_drawdown {
                worst_drawdown = drawdown;
                worst_trough_ts = Some(ts);
                // A new deepest trough supersedes any earlier episode's pending recovery.
                recovery_ns = None;
                pending = Some((ts, running_max));
            }
        }

        if worst_trough_ts.is_none() {
            return Some(0.0); // Never underwater
        }

        // Deepest episode never regained its peak.
        Some(recovery_ns.map_or(f64::NAN, |ns| ns as f64 / NANOSECONDS_IN_DAY as f64))
    }

    fn calculate_from_realized_pnls(&self, _realized_pnls: &[f64]) -> Option<Self::Item> {
        None
    }

    fn calculate_from_positions(&self, _positions: &[Position]) -> Option<Self::Item> {
        None
    }
}

////////////////////////////////////////////////////////////////////////////////
// Tests
////////////////////////////////////////////////////////////////////////////////
#[cfg(test)]
mod tests {
    use nautilus_core::{UnixNanos, datetime::NANOSECONDS_IN_DAY};
    use rstest::rstest;

    use super::*;
    use crate::Returns;

    fn daily_returns(values: &[f64]) -> Returns {
        let mut returns = Returns::new();
        for (i, &value) in values.iter().enumerate() {
            returns.insert(UnixNanos::from(i as u64 * NANOSECONDS_IN_DAY), value);
        }
        returns
    }

    #[rstest]
    fn test_empty_returns_is_zero() {
        let stat = MaxDrawdownRecovery::new();
        assert_eq!(stat.calculate_from_returns(&daily_returns(&[])), Some(0.0));
    }

    #[rstest]
    fn test_never_underwater_is_zero() {
        let stat = MaxDrawdownRecovery::new();
        let returns = daily_returns(&[0.01, 0.01, 0.01]);
        assert_eq!(stat.calculate_from_returns(&returns), Some(0.0));
    }

    /// Trough at day 1, peak regained at day 3: the recovery leg is 2 days.
    #[rstest]
    fn test_measures_trough_to_recovery_leg() {
        let stat = MaxDrawdownRecovery::new();
        let returns = daily_returns(&[0.0, -0.20, 0.10, 0.20]);
        assert_eq!(stat.calculate_from_returns(&returns), Some(2.0));
    }

    /// An unrecovered deepest drawdown has no recovery time; `0` would read as instant recovery.
    #[rstest]
    fn test_unrecovered_deepest_drawdown_is_nan() {
        let stat = MaxDrawdownRecovery::new();
        let returns = daily_returns(&[0.0, -0.20, 0.01, 0.01]);
        let result = stat.calculate_from_returns(&returns).unwrap();
        assert!(result.is_nan(), "expected NaN, got {result}");
    }

    /// A later, deeper episode supersedes an earlier recovered one.
    #[rstest]
    fn test_deeper_later_episode_supersedes_earlier() {
        let stat = MaxDrawdownRecovery::new();
        // Shallow dip recovered in 1 day, then a deeper dip recovered in 3 days.
        let returns = daily_returns(&[0.0, -0.05, 0.10, -0.30, 0.05, 0.05, 0.40]);
        assert_eq!(stat.calculate_from_returns(&returns), Some(3.0));
    }

    #[rstest]
    fn test_name() {
        assert_eq!(
            MaxDrawdownRecovery::new().name(),
            "Max Drawdown Recovery (days)"
        );
    }
}
