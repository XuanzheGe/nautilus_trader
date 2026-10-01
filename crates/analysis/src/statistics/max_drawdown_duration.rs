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

/// The longest time spent below a prior equity peak, in days.
///
/// Depth alone does not describe a drawdown: a shallow decline that lasts a year is a different
/// risk from a sharp one that recovers in a week. This measures the longest single underwater
/// stretch, from the peak that started it to the point the peak was regained.
///
/// An episode that is still underwater at the end of the series is measured up to the final
/// observation, so the result is a lower bound in that case.
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
pub struct MaxDrawdownDuration {}

impl MaxDrawdownDuration {
    /// Creates a new [`MaxDrawdownDuration`] instance.
    #[must_use]
    pub fn new() -> Self {
        Self {}
    }
}

impl Display for MaxDrawdownDuration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Max Drawdown Duration (days)")
    }
}

impl PortfolioStatistic for MaxDrawdownDuration {
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
        // The timestamp of the peak that the current underwater stretch descends from.
        let mut peak_ts: Option<UnixNanos> = None;
        let mut longest_ns: u64 = 0;

        for (&ts, &ret) in returns {
            cumulative *= 1.0 + ret;

            if cumulative >= running_max {
                // Peak regained (or extended): close any open episode and reset the reference.
                running_max = cumulative;
                peak_ts = Some(ts);
                continue;
            }

            // Underwater. Anchor the episode at the last peak; a series that opens below its
            // starting value has no observed peak, so anchor at the first observation instead.
            let start = *peak_ts.get_or_insert(ts);
            longest_ns = longest_ns.max(ts.as_u64().saturating_sub(start.as_u64()));
        }

        Some(longest_ns as f64 / NANOSECONDS_IN_DAY as f64)
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
        let stat = MaxDrawdownDuration::new();
        assert_eq!(stat.calculate_from_returns(&daily_returns(&[])), Some(0.0));
    }

    #[rstest]
    fn test_monotonic_gains_never_go_underwater() {
        let stat = MaxDrawdownDuration::new();
        let returns = daily_returns(&[0.01, 0.01, 0.01, 0.01]);
        assert_eq!(stat.calculate_from_returns(&returns), Some(0.0));
    }

    /// Peak at day 0, underwater days 1-3, regained at day 4: the stretch is 3 days.
    #[rstest]
    fn test_measures_peak_to_recovery_span() {
        let stat = MaxDrawdownDuration::new();
        let returns = daily_returns(&[0.0, -0.10, -0.05, 0.05, 0.50]);
        assert_eq!(stat.calculate_from_returns(&returns), Some(3.0));
    }

    /// A shallow but long episode must beat a deep but brief one.
    #[rstest]
    fn test_longest_episode_wins_over_deepest() {
        let stat = MaxDrawdownDuration::new();
        // Deep 1-day dip, recovery, then a shallow 4-day stretch.
        let returns = daily_returns(&[0.0, -0.50, 1.20, -0.01, -0.01, -0.01, -0.01, 0.20]);
        assert_eq!(stat.calculate_from_returns(&returns), Some(4.0));
    }

    /// Still underwater at the end: measured to the final observation, i.e. a lower bound.
    #[rstest]
    fn test_unrecovered_episode_measured_to_last_observation() {
        let stat = MaxDrawdownDuration::new();
        let returns = daily_returns(&[0.0, -0.05, -0.05, -0.05]);
        assert_eq!(stat.calculate_from_returns(&returns), Some(3.0));
    }

    #[rstest]
    fn test_name() {
        assert_eq!(
            MaxDrawdownDuration::new().name(),
            "Max Drawdown Duration (days)"
        );
    }
}
