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

//! Shared loader for the recorded 2019 FX quote ticks under `test_data/`.
//!
//! # Why this is not a plain `DataBackendSession` call
//!
//! The two files are encoded at a fixed-point scale of `10^18`, which is what
//! `FIXED_PRECISION` was for 128-bit values when they were written. The constant is
//! now `16`, so the engine's decoder reads every price and size a hundred times too
//! large — EUR/USD comes back as `114.605` and a 1,000,000 unit quote as
//! `100,000,000`.
//!
//! Nothing else in the repository reads these files, and the tests that read the
//! other `test_data/nautilus/*` recordings assert only instrument id, row count and
//! ordering, never a price value, which is why the drift went unnoticed. The 128-bit
//! fixtures under `tests/test_data/databento/` are at the current scale, so this is
//! specific to the older recordings rather than a decoder bug.
//!
//! Rescaling is done here, on raw integers, rather than by editing shared fixtures.
//! The exponent is derived from the live `FIXED_PRECISION`, so the correction becomes
//! a no-op if the files are ever regenerated, and [`load`] range-checks the result so
//! a third scale fails loudly instead of quietly trading wrong prices.

use nautilus_model::{
    data::{Data, QuoteTick},
    identifiers::{Symbol, Venue},
    instruments::{Instrument, InstrumentAny, stubs::default_fx_ccy},
    types::{Price, Quantity, fixed::FIXED_PRECISION, price::PriceRaw, quantity::QuantityRaw},
};
use nautilus_persistence::backend::session::DataBackendSession;

const VENUE: &str = "SIM";

const PATH_A: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../test_data/quote_tick_eurusd_2019_sim_rust.parquet"
);
const PATH_B: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../test_data/quote_tick_usdjpy_2019_sim_rust.parquet"
);
const CHUNK_SIZE: usize = 10_000;

/// Fixed-point scale the recordings were written at.
const RECORDED_PRECISION: u8 = 18;

/// Mid-price band each leg must land in once rescaled.
///
/// January 2019 EUR/USD traded near 1.146 and USD/JPY near 109.7; these bands are wide
/// enough to be uninteresting if the scale is right and impossible to satisfy if it is
/// off by even one decimal place.
const BAND_A: (f64, f64) = (0.8, 1.6);
const BAND_B: (f64, f64) = (70.0, 160.0);

/// The two legs, with the price precisions the recordings carry (5 and 3).
#[must_use]
pub(crate) fn instruments() -> (InstrumentAny, InstrumentAny) {
    let venue = Venue::from(VENUE);
    (
        InstrumentAny::CurrencyPair(default_fx_ccy(Symbol::from("EUR/USD"), Some(venue))),
        InstrumentAny::CurrencyPair(default_fx_ccy(Symbol::from("USD/JPY"), Some(venue))),
    )
}

/// Divides a decoded quote back to the scale the current build expects.
///
/// Raw integer division keeps the values exact: `1_146_050_000_000_000_000 / 100` is
/// `1.14605` at a scale of `10^16` with nothing to round.
fn rescaled(quote: &QuoteTick, price_factor: PriceRaw, size_factor: QuantityRaw) -> QuoteTick {
    QuoteTick::new(
        quote.instrument_id,
        Price::from_raw(
            quote.bid_price.raw / price_factor,
            quote.bid_price.precision,
        ),
        Price::from_raw(
            quote.ask_price.raw / price_factor,
            quote.ask_price.precision,
        ),
        Quantity::from_raw(quote.bid_size.raw / size_factor, quote.bid_size.precision),
        Quantity::from_raw(quote.ask_size.raw / size_factor, quote.ask_size.precision),
        quote.ts_event,
        quote.ts_init,
    )
}

fn check_band(quote: &QuoteTick, band: (f64, f64)) -> anyhow::Result<()> {
    let mid = f64::midpoint(quote.bid_price.as_f64(), quote.ask_price.as_f64());
    anyhow::ensure!(
        (band.0..=band.1).contains(&mid),
        "{} rescaled to a mid of {mid}, outside the expected {:?}. The recordings were \
         written at a fixed-point scale of 10^{RECORDED_PRECISION} and this build uses \
         10^{FIXED_PRECISION}; if the files were regenerated, update RECORDED_PRECISION.",
        quote.instrument_id,
        band
    );
    Ok(())
}

/// Decodes both files through one session, which k-merges them into a single stream
/// ordered by `ts_init`, then corrects the scale.
pub(crate) fn load() -> anyhow::Result<Vec<Data>> {
    anyhow::ensure!(
        RECORDED_PRECISION >= FIXED_PRECISION,
        "recordings are at a coarser scale (10^{RECORDED_PRECISION}) than this build \
         (10^{FIXED_PRECISION}); rescaling would have to multiply, which this loader \
         does not do because it cannot recover precision that was never stored"
    );

    let mut session = DataBackendSession::new(CHUNK_SIZE);
    session.add_file::<QuoteTick>("quotes_a", PATH_A, None, None)?;
    session.add_file::<QuoteTick>("quotes_b", PATH_B, None, None)?;
    let decoded = session
        .get_query_result()
        .collect::<Result<Vec<Data>, _>>()?;
    anyhow::ensure!(!decoded.is_empty(), "no quotes decoded from the recordings");

    let exponent = u32::from(RECORDED_PRECISION - FIXED_PRECISION);
    let price_factor = (10 as PriceRaw).pow(exponent);
    let size_factor = (10 as QuantityRaw).pow(exponent);

    let (instrument_a, _) = instruments();
    let symbol_a = instrument_a.id().symbol;

    let mut quotes = Vec::with_capacity(decoded.len());
    let (mut checked_a, mut checked_b) = (false, false);

    for item in &decoded {
        let Data::Quote(quote) = item else {
            anyhow::bail!("recordings must contain only quotes, found {item:?}");
        };
        let quote = rescaled(quote, price_factor, size_factor);

        // One check per leg: the scale is a property of the file, not of a row.
        let is_leg_a = quote.instrument_id.symbol == symbol_a;
        if is_leg_a && !checked_a {
            check_band(&quote, BAND_A)?;
            checked_a = true;
        } else if !is_leg_a && !checked_b {
            check_band(&quote, BAND_B)?;
            checked_b = true;
        }

        quotes.push(Data::Quote(quote));
    }

    anyhow::ensure!(
        checked_a && checked_b,
        "expected quotes for both legs, saw leg A: {checked_a}, leg B: {checked_b}"
    );
    Ok(quotes)
}
