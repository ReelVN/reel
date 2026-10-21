//! Civil time uses Jiff; scheduling remains the embedder's responsibility.
use crate::{NativeError, StandardLibrary};
use aside_host::aside_vm::{Value, bytecode::Effects};
use aside_host::{CallContext, native};
use jiff::{Timestamp, Zoned, tz::TimeZone};

fn datetime(timestamp: i64, timezone: Option<String>) -> Result<Zoned, NativeError> {
    let timestamp = Timestamp::from_second(timestamp)
        .map_err(|error| NativeError::Message(error.to_string()))?;
    let timezone = match timezone {
        Some(name) => {
            TimeZone::get(&name).map_err(|error| NativeError::Message(error.to_string()))?
        }
        None => TimeZone::system(),
    };
    Ok(timestamp.to_zoned(timezone))
}

/// Return the current UTC Unix timestamp in whole seconds.
#[native(symbol = "std::time::now", effects = Effects(Effects::HOST))]
pub(super) fn now(
    #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
) -> Result<i64, NativeError> {
    Ok(Timestamp::now().as_second())
}

/// Split a Unix timestamp into civil time fields. `timezone` is an IANA name
/// such as `Asia/Shanghai`; `null` selects the system timezone. The result has
/// `year`, `month`, `day`, `hour`, `minute`, `second`, `weekday` (Monday = 1),
/// and `offset_seconds` east of UTC.
///
/// ```aside
/// use std::time::parts;
/// fn main() {
///     let epoch = parts(0, timezone: "UTC");
///     if epoch.year != 1970 || epoch.month != 1 { throw "civil time"; }
/// }
/// ```
#[native(symbol = "std::time::parts", effects = Effects(Effects::HOST | Effects::ALLOCATE))]
pub(super) fn parts(
    #[aside(context)] context: &mut CallContext<'_, StandardLibrary>,
    timestamp: i64,
    #[aside(default = null)] timezone: Option<String>,
) -> Result<Value, NativeError> {
    let date = datetime(timestamp, timezone)?;
    let heap = context.vm.heap_mut();
    let result = heap.alloc_table()?;
    for (index, (name, value)) in [
        ("year", i64::from(date.year())),
        ("month", i64::from(date.month())),
        ("day", i64::from(date.day())),
        ("hour", i64::from(date.hour())),
        ("minute", i64::from(date.minute())),
        ("second", i64::from(date.second())),
        ("weekday", i64::from(date.weekday().to_monday_one_offset())),
        ("offset_seconds", i64::from(date.offset().seconds())),
    ]
    .into_iter()
    .enumerate()
    {
        let key = heap.alloc_string(name)?;
        heap.table_store(result, index as u32, key, Value::int(value))?;
    }
    Ok(result)
}

/// Format a Unix timestamp using strftime directives. The default pattern is
/// `%Y-%m-%d %H:%M`; `timezone: null` uses the system timezone.
///
/// ```aside
/// use std::time::format;
/// fn main() {
///     if format(0, "%Y-%m-%d", "UTC") != "1970-01-01" { throw "date"; }
/// }
/// ```
#[native(symbol = "std::time::format", effects = Effects(Effects::HOST | Effects::ALLOCATE))]
pub(super) fn format(
    #[aside(context)] _context: &mut CallContext<'_, StandardLibrary>,
    timestamp: i64,
    #[aside(default = "%Y-%m-%d %H:%M")] pattern: String,
    #[aside(default = null)] timezone: Option<String>,
) -> Result<String, NativeError> {
    let date = datetime(timestamp, timezone)?;
    jiff::fmt::strtime::format(&pattern, &date)
        .map_err(|error| NativeError::Message(error.to_string()))
}
