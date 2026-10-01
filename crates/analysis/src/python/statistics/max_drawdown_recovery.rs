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

use std::collections::BTreeMap;

use nautilus_model::position::Position;
use pyo3::prelude::*;

use super::transform_returns;
use crate::{statistic::PortfolioStatistic, statistics::max_drawdown_recovery::MaxDrawdownRecovery};

#[pymethods]
#[pyo3_stub_gen::derive::gen_stub_pymethods]
impl MaxDrawdownRecovery {
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
    #[new]
    fn py_new() -> Self {
        Self::new()
    }

    #[getter]
    #[pyo3(name = "name")]
    fn py_name(&self) -> String {
        self.name()
    }

    #[pyo3(name = "calculate_from_returns")]
    #[expect(clippy::needless_pass_by_value)]
    fn py_calculate_from_returns(&self, raw_returns: BTreeMap<u64, f64>) -> Option<f64> {
        self.calculate_from_returns(&transform_returns(&raw_returns))
    }

    #[pyo3(name = "calculate_from_realized_pnls")]
    fn py_calculate_from_realized_pnls(&self, _realized_pnls: Vec<f64>) -> Option<f64> {
        None
    }

    #[pyo3(name = "calculate_from_positions")]
    fn py_calculate_from_positions(&self, _positions: Vec<Position>) -> Option<f64> {
        None
    }

    fn __repr__(&self) -> String {
        format!("MaxDrawdownRecovery({})", self.name())
    }
}
