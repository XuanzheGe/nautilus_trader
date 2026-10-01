# Use the Strategy Workbench

Run the local workbench from the repository root:

```bash
cargo run --release -p nautilus-backtest --features examples,high-precision \
  --example workbench-serve
```

Open <http://127.0.0.1:8787>. To choose another local port, append
`-- 127.0.0.1:8788`. The example accepts loopback addresses only.

## Explore a strategy

Select EMA crossover, grid market making, or pairs z-score. Adjust the parameters and data
length, then run the backtest. Auto-run applies changes when a control is released; turn it off
to edit several parameters before running.

EMA and grid runs use deterministic synthetic AUD/USD quotes. The pairs example uses the
repository's recorded EUR/USD and USD/JPY quotes from 2019. If those files cannot be loaded,
the synthetic scenarios remain available and the pairs scenario reports the missing data.
The page identifies the data source for every result.

Inspect equity, realized and unrealized PnL, and drawdown on a shared time axis. Move the cursor
or use the chart's arrow keys to inspect samples. Use the range controls to zoom. Summary
statistics describe the complete run; zoom changes the chart view.

Use run history to restore earlier results or select a baseline for comparison. Baselines must
share the same data source, instrument, currency, and starting balance. Curves align by elapsed
time; data lengths can differ. History lasts for the current page session. Export JSON to retain a complete run or CSV for its samples.

## Interpret the values

The workbench uses the engine's `PortfolioSnapshot` events. Prices, money and fees continue to
use the core's existing domain types and arithmetic. The page converts values to numbers for
charting; JSON also retains decimal money strings and exact nanosecond timestamps.

- **Equity** is account balance plus marked open-position PnL for margin accounts. Cash account
  equity uses the value of holdings according to the portfolio's existing accounting model.
- **Unrealized PnL** is the open positions' marked gain or loss at that sample.
- **Realized PnL** comes directly from the snapshot, including realized portions of open positions.
  It is not inferred by subtracting floating PnL from account equity. Deposits, withdrawals,
  funding and other account adjustments can make those values differ. Foreign-currency realized
  PnL uses the portfolio's valuation exchange rates, which can differ from the rates used when
  cash settled; this can leave a translation difference even after all positions close.
- **Sampled drawdown** uses the displayed equity history and starting capital. It can miss price
  extrema between samples. The engine's returns statistics retain their own daily or explicitly
  labeled trade-return basis; intraday drawdown and daily drawdown need not agree.

Missing prices produce gaps and unavailable values, not zero PnL. A carried valuation remains
visible with a stale flag. The example does not sum unlike currencies or invent exchange rates.
An aggregate that cannot be represented completely in the requested currency is unavailable.
A stale or incomplete daily close is not treated as a zero-return day by the analyzer.

## Record floating PnL in another backtest

Enable the existing optional sampling stream in the engine configuration:

```rust
use nautilus_backtest::config::BacktestEngineConfig;
use nautilus_portfolio::config::PortfolioConfig;

let config = BacktestEngineConfig::builder()
    .portfolio(PortfolioConfig {
        snapshot_interval_ms: Some(1_000),
        ..PortfolioConfig::default()
    })
    .build();
```

The optional stream records at the chosen interval while positions are open, after position
open/change/close events, and on finalization. This preserves trades that open and close between
timer ticks. It does not record every market tick. Default daily equity snapshots remain unchanged.
The engine finalizes snapshots automatically when a backtest ends; direct portfolio callers can
use `finalize_equity_curve()`. Finalization stops sampling until `reset()`.

Retrieve samples through `portfolio.snapshots(&account_id)`, or consume `PortfolioSnapshot`
events from the existing message bus for long-running sessions. The in-memory ring retains up to
1,000,000 snapshots per account; use message-bus persistence when the full history exceeds that cap.

## Extend the examples

The workbench is an example application over `BacktestEngine`, `Portfolio` and
`PortfolioAnalyzer`. It adds no web-framework dependency to those components.
To add a strategy or data source, extend `run_backtest` and the scenario selection in
`crates/backtest/examples/workbench_serve.rs`, then add its parameter controls in
`crates/backtest/examples/workbench.html`.

The shared exporter at `crates/backtest/examples/workbench_data/mod.rs` validates one-account,
one-currency series. Both the local server and `workbench-export` use the same exporter.
The offline sweep writes a JSON dataset:

```bash
cargo run --release -p nautilus-backtest --features examples \
  --example workbench-export -- workbench_data.json
```

This example currently offers three built-in strategy scenarios. It does not provide arbitrary
strategy uploads, live trading controls, or a general optimization service.
