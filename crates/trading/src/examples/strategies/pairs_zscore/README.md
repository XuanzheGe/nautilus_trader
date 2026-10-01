# Pairs Z-Score

Two-instrument mean reversion strategy that regresses one leg's log mid on the
other's over a rolling window and trades the standing residual.

## Strategy overview

The strategy subscribes to quotes for two instruments, leg A (the dependent leg)
and leg B (the independent leg). On every quote from either leg it records one
synchronized observation using the latest mid of the other leg, then fits

```text
log(mid_A) = intercept + beta * log(mid_B) + residual
```

over the last `lookback` observations by ordinary least squares. The fit carries
an intercept, so the window's residuals have mean zero by construction, and the
signal is the standing residual divided by the window's residual dispersion.

Working in log space means the two legs need no common price scale: a pair
quoted at 1.14 and a pair quoted at 109.7 regress against each other without
any normalization step.

### Trading cycle

Each quote on either leg triggers the following:

1. Record the leg's mid, then append one synchronized `(log A, log B)` sample.
2. Return if the window is not yet full, or if the fit is degenerate (a
   near-constant independent leg, or residuals with no dispersion).
3. When flat and `|z| >= entry_z`, open the spread — but only if the portfolio
   is flat on **both** legs, so a leg stranded by a partial exit is never built
   on.
4. When holding, close both legs if `|z| <= exit_z` (reversion) or
   `|z| >= stop_z` (divergence, when a stop is configured).

A residual above the fit means leg A is rich against the relationship, so the
strategy sells leg A; below the fit is the mirror.

### Hedge sizing

Leg A trades a fixed `trade_size_a`. Leg B trades

```text
min(|beta|, max_hedge_ratio) * hedge_scale * trade_size_a
```

rounded to leg B's size increment, on the **opposite** side when `beta` is
positive and the **same** side when it is negative.

The regression slope is dimensionless, so it cannot by itself bridge two legs
whose quantities are denominated differently — different base currency, contract
size, or lot multiplier. `hedge_scale` carries that conversion and the strategy
does not attempt to infer it. `max_hedge_ratio` bounds the slope used for
sizing, because a regression against a near-constant leg produces an arbitrarily
large one.

### Asynchronous quote streams

The two legs quote independently, so the sampler carries the slower leg forward
(last observation carried forward). This is the standard treatment for two
asynchronous streams, and it biases the fit towards the more frequently quoted
leg. A pair with very different quote rates is worth resampling upstream.

### What this strategy does not do

**Cointegration is an input, not an output.** No stationarity or cointegration
test gates entry. Applied to a pair whose relationship is not stable, the signal
still fires on every excursion and the results describe that particular sample
rather than an edge. Establishing that the residual mean-reverts is the user's
job, before the strategy is pointed at a pair.

The position state machine records the side it intended and reads it back rather
than deriving it from fills, so it assumes this strategy is the only actor on its
two instruments.

## Parameters

| Parameter | Description | Default |
| --- | --- | --- |
| `instrument_id_a` | Dependent leg; trades a fixed size | — |
| `instrument_id_b` | Independent leg; sized by the hedge ratio | — |
| `trade_size_a` | Quantity traded on leg A per entry | — |
| `lookback` | Samples in the rolling regression window | `240` |
| `entry_z` | Absolute residual z-score that opens the spread | `2.0` |
| `exit_z` | Absolute residual z-score that closes it | `0.5` |
| `stop_z` | Absolute z-score that abandons a diverging spread | `None` |
| `hedge_scale` | Unit conversion from leg A quantity to leg B quantity | `1.0` |
| `max_hedge_ratio` | Cap on the absolute slope used for sizing | `5.0` |

The gap between `entry_z` and `exit_z` is the hysteresis band; without it the
strategy churns around a single threshold. `entry_z` must exceed `exit_z`, and
`stop_z` must exceed `entry_z`; all of this is validated at `on_start`.

The base config defaults to `OmsType::Netting`, so each leg carries a single net
position and the two read as one spread position.

## Rust usage

```rust,ignore
use nautilus_model::{identifiers::InstrumentId, types::Quantity};
use nautilus_trading::examples::strategies::{PairsZScore, PairsZScoreConfig};

let strategy = PairsZScore::new(
    PairsZScoreConfig::builder()
        .instrument_id_a(InstrumentId::from("EUR/USD.SIM"))
        .instrument_id_b(InstrumentId::from("USD/JPY.SIM"))
        .trade_size_a(Quantity::from("100000"))
        .lookback(600)
        .entry_z(2.0)
        .exit_z(0.5)
        .stop_z(4.0)
        .build(),
);
engine.add_strategy(strategy)?;
```

Both instruments must be added to the engine before the strategy; leg B is
resolved from the cache at `on_start` for its size precision and quantity limits.

A runnable example over recorded quote ticks lives at
`crates/backtest/examples/pairs_zscore_fx.rs`.

## Extending this strategy

- Gate entry on a rolling stationarity test of the residual, so the strategy
  stands down when the relationship breaks rather than trading through it.
- Estimate the hedge ratio on a longer window than the z-score, which is the
  usual split: the relationship moves more slowly than the deviation from it.
- Replace the market orders with limit orders at the touch; a spread worth two
  standard deviations rarely needs to cross two spreads to capture.
