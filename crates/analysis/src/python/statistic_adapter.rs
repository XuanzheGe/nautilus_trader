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

//! Bridges a Python-defined statistic into the Rust [`PortfolioStatistic`] trait.

use std::collections::BTreeMap;

use nautilus_core::python::to_pyvalue_err;
use nautilus_model::{orders::Order, position::Position};
use pyo3::prelude::*;

use std::sync::Arc;

use crate::{
    Returns,
    analyzer::Statistic,
    statistic::PortfolioStatistic,
    statistics::{
        alpha::Alpha, beta_ratio::BetaRatio, cagr::CAGR, calmar_ratio::CalmarRatio,
        down_capture_ratio::DownCaptureRatio, expectancy::Expectancy,
        expected_shortfall::ExpectedShortfall, information_ratio::InformationRatio,
        long_ratio::LongRatio, loser_avg::AvgLoser, loser_max::MaxLoser, loser_min::MinLoser,
        max_drawdown::MaxDrawdown, max_drawdown_duration::MaxDrawdownDuration,
        max_drawdown_recovery::MaxDrawdownRecovery, omega_ratio::OmegaRatio, profit_factor::ProfitFactor,
        returns_avg::ReturnsAverage, returns_avg_loss::ReturnsAverageLoss,
        returns_avg_win::ReturnsAverageWin, returns_kurtosis::ReturnsKurtosis,
        returns_skewness::ReturnsSkewness, returns_volatility::ReturnsVolatility,
        risk_return_ratio::RiskReturnRatio, sharpe_ratio::SharpeRatio, sortino_ratio::SortinoRatio,
        tail_ratio::TailRatio, tracking_error::TrackingError, treynor_ratio::TreynorRatio,
        ulcer_index::UlcerIndex, up_capture_ratio::UpCaptureRatio, value_at_risk::ValueAtRisk,
        win_rate::WinRate, winner_avg::AvgWinner, winner_max::MaxWinner, winner_min::MinWinner,
    },
};

/// Wraps a Python object so it can be registered as a portfolio statistic.
///
/// Each calculation hook forwards to the same-named method on the wrapped object. A hook the
/// object does not define, or one that raises, yields `None` for that statistic rather than
/// propagating: the analyzer calls every hook on every registered statistic, so a partially
/// implemented statistic must not abort the whole computation. This is the opposite of the trait's
/// Rust defaults, which panic — appropriate for a Rust implementor that forgot a hook, wrong for a
/// Python statistic that only ever implements one.
///
/// An exception raised inside a hook is reported through `sys.unraisablehook` rather than being
/// swallowed silently, so a buggy statistic is visible without taking down the run.
#[derive(Debug)]
pub struct PyStatisticAdapter {
    inner: Py<PyAny>,
    /// Resolved once at construction: [`PortfolioStatistic::name`] is infallible and is used as
    /// the registry key, so it must not need the GIL or be able to fail.
    name: String,
}

impl PyStatisticAdapter {
    /// Creates a new [`PyStatisticAdapter`] wrapping `statistic`.
    ///
    /// # Errors
    ///
    /// Returns an error if the object exposes no usable `name`.
    pub fn new(py: Python<'_>, statistic: Py<PyAny>) -> PyResult<Self> {
        let name = resolve_statistic_name(py, &statistic)?;
        Ok(Self {
            inner: statistic,
            name,
        })
    }

    /// Calls `method` with `args`, mapping a missing method or any raised exception to `None`.
    fn call_hook<A>(&self, method: &str, args: A) -> Option<f64>
    where
        A: for<'py> pyo3::call::PyCallArgs<'py>,
    {
        Python::attach(|py| match self.inner.call_method1(py, method, args) {
            Ok(value) => value.extract::<Option<f64>>(py).unwrap_or_else(|err| {
                Self::report(py, &self.inner, err);
                None
            }),
            Err(err) => {
                if !err.is_instance_of::<pyo3::exceptions::PyAttributeError>(py) {
                    // A missing hook is expected; anything else is a bug worth surfacing.
                    Self::report(py, &self.inner, err);
                }
                None
            }
        })
    }

    fn report(py: Python<'_>, context: &Py<PyAny>, err: PyErr) {
        err.write_unraisable(py, Some(&context.bind(py).clone()));
    }
}

impl PortfolioStatistic for PyStatisticAdapter {
    type Item = f64;

    fn name(&self) -> String {
        self.name.clone()
    }

    fn calculate_from_returns(&self, returns: &Returns) -> Option<Self::Item> {
        let raw: BTreeMap<u64, f64> = returns
            .iter()
            .map(|(key, value)| (key.as_u64(), *value))
            .collect();
        self.call_hook("calculate_from_returns", (raw,))
    }

    fn calculate_from_realized_pnls(&self, realized_pnls: &[f64]) -> Option<Self::Item> {
        self.call_hook("calculate_from_realized_pnls", (realized_pnls.to_vec(),))
    }

    fn calculate_from_positions(&self, positions: &[Position]) -> Option<Self::Item> {
        self.call_hook("calculate_from_positions", (positions.to_vec(),))
    }

    /// Always `None`.
    ///
    /// The order hook has no caller anywhere in the engine and is not part of the Python statistic
    /// surface, so there is nothing to forward to.
    fn calculate_from_orders(&self, _orders: Vec<Box<dyn Order>>) -> Option<Self::Item> {
        None
    }

    fn calculate_from_returns_with_benchmark(
        &self,
        returns: &Returns,
        benchmark: &Returns,
    ) -> Option<Self::Item> {
        let to_raw = |series: &Returns| -> BTreeMap<u64, f64> {
            series
                .iter()
                .map(|(key, value)| (key.as_u64(), *value))
                .collect()
        };
        self.call_hook(
            "calculate_from_returns_with_benchmark",
            (to_raw(returns), to_raw(benchmark)),
        )
    }
}

/// Resolves a statistic's registry key from a Python object.
///
/// Accepts a `name` attribute (property or plain value) or a `name()` method, falling back to the
/// class name. Registration and deregistration both go through this, so an object is always removed
/// under the key it was added with.
///
/// # Errors
///
/// Returns an error if the object exposes no usable name at all.
pub fn resolve_statistic_name(py: Python<'_>, statistic: &Py<PyAny>) -> PyResult<String> {
    if let Ok(attr) = statistic.getattr(py, "name") {
        if let Ok(text) = attr.extract::<String>(py) {
            return Ok(text);
        }
        if let Ok(result) = attr.call0(py)
            && let Ok(text) = result.extract::<String>(py)
        {
            return Ok(text);
        }
    }

    statistic
        .getattr(py, "__class__")?
        .getattr(py, "__name__")?
        .extract::<String>(py)
}

/// Returns `true` when `statistic` looks like a usable Python statistic.
///
/// Requires at least one calculation hook, so a plainly wrong argument still fails registration
/// loudly instead of being accepted as a statistic that silently computes nothing.
#[must_use]
pub fn is_python_statistic(py: Python<'_>, statistic: &Py<PyAny>) -> bool {
    const HOOKS: [&str; 4] = [
        "calculate_from_returns",
        "calculate_from_realized_pnls",
        "calculate_from_positions",
        "calculate_from_returns_with_benchmark",
    ];

    HOOKS.iter().any(|hook| {
        statistic
            .getattr(py, *hook)
            .is_ok_and(|attr| attr.bind(py).is_callable())
    })
}

////////////////////////////////////////////////////////////////////////////////
// Tests
////////////////////////////////////////////////////////////////////////////////
#[cfg(test)]
mod tests {
    use std::ffi::CString;

    use pyo3::types::PyDict;
    use rstest::rstest;

    use super::*;

    /// Evaluates `source` and returns the object bound to `stat`.
    fn python_statistic(py: Python<'_>, source: &str) -> Py<PyAny> {
        let locals = PyDict::new(py);
        let code = CString::new(source).expect("test source should be a valid CString");
        py.run(code.as_c_str(), None, Some(&locals))
            .expect("test statistic should define cleanly");
        locals
            .get_item("stat")
            .expect("lookup should succeed")
            .expect("test source must bind `stat`")
            .unbind()
    }

    #[rstest]
    fn test_forwards_returns_hook_and_resolves_name() {
        Python::initialize();
        Python::attach(|py| {
            let obj = python_statistic(
                py,
                r"
class Custom:
    name = 'Custom Metric'

    def calculate_from_returns(self, returns):
        return sum(returns.values())

stat = Custom()
",
            );
            let adapter = PyStatisticAdapter::new(py, obj).unwrap();

            assert_eq!(adapter.name(), "Custom Metric");

            let mut returns = Returns::new();
            returns.insert(nautilus_core::UnixNanos::from(1), 0.25);
            returns.insert(nautilus_core::UnixNanos::from(2), 0.75);
            assert_eq!(adapter.calculate_from_returns(&returns), Some(1.0));
        });
    }

    /// A statistic implementing only one hook must yield `None` elsewhere, never abort the run.
    /// The Rust trait defaults panic here, which is why the adapter overrides every hook.
    #[rstest]
    fn test_missing_hook_is_none_not_panic() {
        Python::initialize();
        Python::attach(|py| {
            let obj = python_statistic(
                py,
                r"
class OnlyReturns:
    def calculate_from_returns(self, returns):
        return 1.0

stat = OnlyReturns()
",
            );
            let adapter = PyStatisticAdapter::new(py, obj).unwrap();

            assert_eq!(adapter.calculate_from_realized_pnls(&[1.0, -2.0]), None);
            assert_eq!(adapter.calculate_from_positions(&[]), None);
        });
    }

    /// A raising hook must not propagate; the statistic is simply absent.
    #[rstest]
    fn test_raising_hook_yields_none() {
        Python::initialize();
        Python::attach(|py| {
            let obj = python_statistic(
                py,
                r"
class Boom:
    def calculate_from_returns(self, returns):
        raise ValueError('boom')

stat = Boom()
",
            );
            let adapter = PyStatisticAdapter::new(py, obj).unwrap();
            assert_eq!(adapter.calculate_from_returns(&Returns::new()), None);
        });
    }

    /// Falls back to the class name when no `name` is exposed.
    #[rstest]
    fn test_name_falls_back_to_class_name() {
        Python::initialize();
        Python::attach(|py| {
            let obj = python_statistic(
                py,
                r"
class Unnamed:
    def calculate_from_returns(self, returns):
        return 0.0

stat = Unnamed()
",
            );
            let adapter = PyStatisticAdapter::new(py, obj).unwrap();
            assert_eq!(adapter.name(), "Unnamed");
        });
    }

    /// An object with no calculation hook must be rejected at registration, not accepted as a
    /// statistic that silently computes nothing.
    /// Every built-in must be reachable by the native fast path. A statistic added to the crate
    /// but omitted from `resolve_builtin` silently degrades to the adapter instead of failing, so
    /// this is the guard for that omission.
    #[rstest]
    fn test_builtins_resolve_natively() {
        Python::initialize();
        Python::attach(|py| {
            let cases: Vec<(&str, Py<PyAny>)> = vec![
                (
                    stringify!(MaxDrawdown),
                    Py::new(py, MaxDrawdown::new()).unwrap().into_any(),
                ),
                (
                    stringify!(MaxDrawdownDuration),
                    Py::new(py, MaxDrawdownDuration::new()).unwrap().into_any(),
                ),
                (
                    stringify!(MaxDrawdownRecovery),
                    Py::new(py, MaxDrawdownRecovery::new()).unwrap().into_any(),
                ),
                (
                    stringify!(SharpeRatio),
                    Py::new(py, SharpeRatio::new(Some(252), Some(0.01)))
                        .unwrap()
                        .into_any(),
                ),
            ];

            for (type_name, obj) in cases {
                let resolved = resolve_builtin(py, &obj, type_name)
                    .expect("resolution should not raise")
                    .unwrap_or_else(|| panic!("{type_name} must resolve on the native fast path"));
                assert!(!resolved.name().is_empty());
            }
        });
    }

    /// An unknown class name must fall through rather than being mistaken for a built-in.
    #[rstest]
    fn test_unknown_class_name_is_not_a_builtin() {
        Python::initialize();
        Python::attach(|py| {
            let obj = Py::new(py, MaxDrawdown::new()).unwrap().into_any();
            assert!(resolve_builtin(py, &obj, "NotAStatistic").unwrap().is_none());
        });
    }

    #[rstest]
    fn test_object_without_hooks_is_not_a_statistic() {
        Python::initialize();
        Python::attach(|py| {
            let obj = python_statistic(py, "stat = object()");
            assert!(!is_python_statistic(py, &obj));
        });
    }

    #[rstest]
    fn test_object_with_one_hook_is_a_statistic() {
        Python::initialize();
        Python::attach(|py| {
            let obj = python_statistic(
                py,
                r"
class Ok:
    def calculate_from_realized_pnls(self, pnls):
        return 0.0

stat = Ok()
",
            );
            assert!(is_python_statistic(py, &obj));
        });
    }
}

/// Unwraps a built-in statistic out of its Python wrapper.
///
/// Mirrors the class-name dispatch used for order events (`crates/model/src/python/events/order`):
/// `stringify!` derives each arm's string from the type itself, so the two cannot drift apart the
/// way a hand-written string literal can.
fn extract_as<T>(py: Python<'_>, statistic: &Py<PyAny>) -> PyResult<Statistic>
where
    T: PortfolioStatistic<Item = f64> + Send + Sync + 'static,
    T: for<'a, 'py> FromPyObject<'a, 'py>,
    for<'a, 'py> PyErr: From<<T as FromPyObject<'a, 'py>>::Error>,
{
    Ok(Arc::new(statistic.extract::<T>(py)?))
}

/// Resolves a built-in statistic by Python class name, or `None` if it is not a built-in.
///
/// Unwrapping to the native Rust implementation means computing the statistic costs no Python call.
/// This is an optimization: every built-in also exposes its calculation methods to Python, so one
/// missing here still works through [`PyStatisticAdapter`], just across the GIL.
fn resolve_builtin(
    py: Python<'_>,
    statistic: &Py<PyAny>,
    type_name: &str,
) -> PyResult<Option<Statistic>> {
    match type_name {
        stringify!(Alpha) => extract_as::<Alpha>(py, statistic).map(Some),
        stringify!(BetaRatio) => extract_as::<BetaRatio>(py, statistic).map(Some),
        stringify!(CAGR) => extract_as::<CAGR>(py, statistic).map(Some),
        stringify!(CalmarRatio) => extract_as::<CalmarRatio>(py, statistic).map(Some),
        stringify!(DownCaptureRatio) => extract_as::<DownCaptureRatio>(py, statistic).map(Some),
        stringify!(Expectancy) => extract_as::<Expectancy>(py, statistic).map(Some),
        stringify!(ExpectedShortfall) => extract_as::<ExpectedShortfall>(py, statistic).map(Some),
        stringify!(InformationRatio) => extract_as::<InformationRatio>(py, statistic).map(Some),
        stringify!(LongRatio) => extract_as::<LongRatio>(py, statistic).map(Some),
        stringify!(AvgLoser) => extract_as::<AvgLoser>(py, statistic).map(Some),
        stringify!(MaxLoser) => extract_as::<MaxLoser>(py, statistic).map(Some),
        stringify!(MinLoser) => extract_as::<MinLoser>(py, statistic).map(Some),
        stringify!(MaxDrawdown) => extract_as::<MaxDrawdown>(py, statistic).map(Some),
        stringify!(MaxDrawdownDuration) => extract_as::<MaxDrawdownDuration>(py, statistic).map(Some),
        stringify!(MaxDrawdownRecovery) => extract_as::<MaxDrawdownRecovery>(py, statistic).map(Some),
        stringify!(OmegaRatio) => extract_as::<OmegaRatio>(py, statistic).map(Some),
        stringify!(ProfitFactor) => extract_as::<ProfitFactor>(py, statistic).map(Some),
        stringify!(ReturnsAverage) => extract_as::<ReturnsAverage>(py, statistic).map(Some),
        stringify!(ReturnsAverageLoss) => extract_as::<ReturnsAverageLoss>(py, statistic).map(Some),
        stringify!(ReturnsAverageWin) => extract_as::<ReturnsAverageWin>(py, statistic).map(Some),
        stringify!(ReturnsKurtosis) => extract_as::<ReturnsKurtosis>(py, statistic).map(Some),
        stringify!(ReturnsSkewness) => extract_as::<ReturnsSkewness>(py, statistic).map(Some),
        stringify!(ReturnsVolatility) => extract_as::<ReturnsVolatility>(py, statistic).map(Some),
        stringify!(RiskReturnRatio) => extract_as::<RiskReturnRatio>(py, statistic).map(Some),
        stringify!(SharpeRatio) => extract_as::<SharpeRatio>(py, statistic).map(Some),
        stringify!(SortinoRatio) => extract_as::<SortinoRatio>(py, statistic).map(Some),
        stringify!(TailRatio) => extract_as::<TailRatio>(py, statistic).map(Some),
        stringify!(TrackingError) => extract_as::<TrackingError>(py, statistic).map(Some),
        stringify!(TreynorRatio) => extract_as::<TreynorRatio>(py, statistic).map(Some),
        stringify!(UlcerIndex) => extract_as::<UlcerIndex>(py, statistic).map(Some),
        stringify!(UpCaptureRatio) => extract_as::<UpCaptureRatio>(py, statistic).map(Some),
        stringify!(ValueAtRisk) => extract_as::<ValueAtRisk>(py, statistic).map(Some),
        stringify!(WinRate) => extract_as::<WinRate>(py, statistic).map(Some),
        stringify!(AvgWinner) => extract_as::<AvgWinner>(py, statistic).map(Some),
        stringify!(MaxWinner) => extract_as::<MaxWinner>(py, statistic).map(Some),
        stringify!(MinWinner) => extract_as::<MinWinner>(py, statistic).map(Some),
        _ => Ok(None),
    }
}

/// Turns a Python object into a registrable [`Statistic`].
///
/// A built-in is unwrapped to its native Rust implementation; anything else exposing a calculation
/// hook is bridged through [`PyStatisticAdapter`]. Both registration entry points go through here,
/// so the Portfolio and the standalone analyzer accept exactly the same inputs.
///
/// # Errors
///
/// Returns an error if `statistic` exposes no calculation hook at all.
pub fn resolve_statistic(py: Python<'_>, statistic: Py<PyAny>) -> PyResult<Statistic> {
    let type_name = statistic
        .getattr(py, "__class__")?
        .getattr(py, "__name__")?
        .extract::<String>(py)?;

    if let Some(builtin) = resolve_builtin(py, &statistic, &type_name)? {
        return Ok(builtin);
    }

    if is_python_statistic(py, &statistic) {
        return Ok(Arc::new(PyStatisticAdapter::new(py, statistic)?));
    }

    Err(to_pyvalue_err(format!(
        "Unknown statistic type: {type_name} (a custom statistic must define at least one \
         `calculate_from_*` method)"
    )))
}
