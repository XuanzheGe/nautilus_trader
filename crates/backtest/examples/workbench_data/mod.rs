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

//! Shared, currency-aware snapshot export for the strategy workbench examples.

use nautilus_model::{
    events::PortfolioSnapshot,
    types::{Currency, Money},
};
use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct SnapshotSeries {
    pub t: Vec<f64>,
    pub ts_ns: Vec<String>,
    pub eq: Vec<Option<f64>>,
    pub un: Vec<Option<f64>>,
    pub realized: Vec<Option<f64>>,
    pub stale: Vec<bool>,
    pub unpriced: Vec<Vec<String>>,
    /// Money serializes as a decimal string with its currency, preserving the engine's precision.
    pub equity_exact: Vec<Option<Money>>,
    pub unrealized_exact: Vec<Option<Money>>,
    pub realized_exact: Vec<Option<Money>>,
}

impl SnapshotSeries {
    /// Exports one account in one currency without inventing FX conversions or zero valuations.
    pub(crate) fn new(mut snapshots: Vec<PortfolioSnapshot>, currency: Currency) -> anyhow::Result<Self> {
        anyhow::ensure!(
            snapshots
                .first()
                .is_none_or(|first| snapshots.iter().all(|s| s.account_id == first.account_id)),
            "workbench series requires a single account"
        );
        snapshots.sort_by_key(|s| s.ts_event);
        let first = snapshots.first().map_or(0, |s| s.ts_event.as_u64());
        let mut series = Self {
            t: Vec::new(),
            ts_ns: Vec::new(),
            eq: Vec::new(),
            un: Vec::new(),
            realized: Vec::new(),
            stale: Vec::new(),
            unpriced: Vec::new(),
            equity_exact: Vec::new(),
            unrealized_exact: Vec::new(),
            realized_exact: Vec::new(),
        };

        for snapshot in snapshots {
            let complete = snapshot.unpriced_instruments.is_empty();
            let equity = complete
                .then(|| amount(&snapshot.total_equity, currency, false))
                .flatten();
            let unrealized = complete
                .then(|| amount(&snapshot.unrealized_pnls, currency, true))
                .flatten();
            let realized = amount(&snapshot.realized_pnls, currency, true);
            series
                .t
                .push((snapshot.ts_event.as_u64() - first) as f64 / 1_000_000_000.0);
            series.ts_ns.push(snapshot.ts_event.as_u64().to_string());
            series.eq.push(equity.map(|m| m.as_f64()));
            series.un.push(unrealized.map(|m| m.as_f64()));
            series.realized.push(realized.map(|m| m.as_f64()));
            series.stale.push(
                snapshot.is_stale || equity.is_none() || unrealized.is_none() || realized.is_none(),
            );
            series.unpriced.push(
                snapshot
                    .unpriced_instruments
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            );
            series.equity_exact.push(equity);
            series.unrealized_exact.push(unrealized);
            series.realized_exact.push(realized);
        }

        Ok(series)
    }
}

fn amount(values: &[Money], currency: Currency, empty_is_zero: bool) -> Option<Money> {
    // A foreign nonzero component means the aggregate is incomplete in the requested currency
    if values.iter().any(|m| m.currency != currency && m.raw != 0) {
        return None;
    }

    let mut selected = values.iter().filter(|m| m.currency == currency).copied();
    let Some(first) = selected.next() else {
        return empty_is_zero.then(|| Money::zero(currency));
    };
    selected.try_fold(first, Money::checked_add)
}

#[cfg(test)]
mod tests {
    use nautilus_core::UUID4;
    use nautilus_model::{
        enums::AccountType,
        identifiers::{AccountId, InstrumentId},
    };
    use rstest::rstest;

    use super::*;

    fn snapshot(ts: u64) -> PortfolioSnapshot {
        PortfolioSnapshot::new(
            AccountId::new("SIM-001"),
            AccountType::Margin,
            Some(Currency::USD()),
            Vec::new(),
            Vec::new(),
            vec![Money::from("-12.34 USD")],
            vec![Money::from("5.67 USD")],
            vec![Money::from("993.33 USD")],
            Some(Money::from("993.33 USD")),
            false,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            UUID4::new(),
            ts.into(),
            ts.into(),
        )
    }

    #[rstest]
    fn preserves_subsecond_samples_and_exact_money() {
        let start = 1_735_689_600_123_456_789;
        let series = SnapshotSeries::new(
            vec![snapshot(start + 250_000_000), snapshot(start)],
            Currency::USD(),
        )
        .unwrap();
        let value = serde_json::to_value(&series).unwrap();

        assert_eq!(series.t, vec![0.0, 0.25]);
        assert_eq!(
            series.ts_ns,
            vec![start.to_string(), (start + 250_000_000).to_string()]
        );
        assert_eq!(series.eq, vec![Some(993.33), Some(993.33)]);
        assert_eq!(series.un, vec![Some(-12.34), Some(-12.34)]);
        assert_eq!(series.realized, vec![Some(5.67), Some(5.67)]);
        assert_eq!(value["unrealized_exact"][0], "-12.34 USD");
        assert_eq!(value["equity_exact"][0], "993.33 USD");
        assert_eq!(value["realized_exact"][0], "5.67 USD");
    }

    #[rstest]
    fn unpriced_is_missing_while_carried_values_remain_flagged() {
        let mut unpriced = snapshot(1);
        unpriced
            .unpriced_instruments
            .push(InstrumentId::from("AUD/USD.SIM"));
        let mut carried = snapshot(2);
        carried.is_stale = true;
        let series = SnapshotSeries::new(vec![unpriced, carried], Currency::USD()).unwrap();

        assert_eq!(series.eq, vec![None, Some(993.33)]);
        assert_eq!(series.un, vec![None, Some(-12.34)]);
        assert_eq!(series.realized, vec![Some(5.67), Some(5.67)]);
        assert_eq!(series.stale, vec![true, true]);
        assert_eq!(series.unpriced[0], vec!["AUD/USD.SIM"]);
    }

    #[rstest]
    fn selects_currency_and_never_labels_foreign_pnl_as_usd() {
        let mut zero_foreign = snapshot(1);
        zero_foreign.unrealized_pnls.insert(0, Money::from("0 JPY"));
        let mut foreign = snapshot(2);
        foreign.unrealized_pnls = vec![Money::from("100 JPY")];
        foreign.total_equity.push(Money::from("100 JPY"));
        let mut flat = snapshot(3);
        flat.unrealized_pnls.clear();
        let series =
            SnapshotSeries::new(vec![zero_foreign, foreign, flat], Currency::USD()).unwrap();

        assert_eq!(series.un, vec![Some(-12.34), None, Some(0.0)]);
        assert_eq!(series.eq, vec![Some(993.33), None, Some(993.33)]);
        assert_eq!(series.stale, vec![false, true, false]);
    }

    #[rstest]
    fn rejects_interleaved_accounts() {
        let mut other = snapshot(2);
        other.account_id = AccountId::new("SIM-002");
        assert!(SnapshotSeries::new(vec![snapshot(1), other], Currency::USD()).is_err());
    }
}
