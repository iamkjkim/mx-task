use crate::parse::{ParseError, ParsedTick};
use std::collections::HashMap;
use std::fmt::{self, Display, Formatter};
use std::io::{self, Write};

const MSEC_PER_SEC: u32 = 1_000;
const MSEC_PER_MIN: u32 = 60 * MSEC_PER_SEC;
const MSEC_PER_HOUR: u32 = 60 * MSEC_PER_MIN;
const RSI_PERIOD: u32 = 14; // RSI에서 14개의 가격을 기준으로 계산

pub(crate) struct Aggregator {
    states: HashMap<String, SymbolAgg>, // 키: 종목코드, 값: 1분/1시간 누적값
    summary: Summary,                   // 총 거래량/대금
}

impl Aggregator {
    pub(crate) fn new() -> Self {
        Self {
            states: HashMap::new(),
            summary: Summary::default(),
        }
    }

    pub(crate) fn process_line<W: Write>(&mut self, line: &[u8], line_num: usize, out: &mut W) -> Result<(), AppError> {
        match ParsedTick::parse(line) {
            Ok(Some(parsed)) => self.process_tick(&parsed, out),
            Ok(None) => Ok(()),
            Err(source) => Err(AppError::Parse { line_num, source }),
        }
    }

    fn process_tick<W: Write>(&mut self, tick: &ParsedTick<'_>, out: &mut W) -> Result<(), AppError> {
        // 종목 해시맵에서 종목 1분 및 한 시간 단위 누적값 구조체 레퍼런스 가져옴
        let state = match self.states.get_mut(tick.symbol) {
            Some(state) => state,
            None => self.states.entry(tick.symbol.to_owned()).or_default(),
        };

        state.check_ts_order(tick)?;
        state.record_parsed_tick(tick, out)?; // 계산
        self.summary.record_parsed_tick(tick); // 총 거래량/대금 누적
        Ok(())
    }

    // 종료시 호출
    pub(crate) fn flush<W: Write>(&mut self, out: &mut W) -> Result<(), AppError> {
        let mut entries: Vec<_> = self.states.drain().collect();
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));

        for (symbol, mut sa) in entries {
            sa.flush(&symbol, out)?;
        }

        write!(out, "{}", self.summary.as_output())?;
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) enum AppError {
    Parse {
        line_num: usize,
        source: ParseError,
    },
    OutOfOrderDate {
        symbol: String,
        got: u32,
        current: u32,
    },
    OutOfOrderTime {
        symbol: String,
        date_key: u32,
        got_ms: u32,
        current_ms: u32,
    },
    Io(io::Error),
}

impl Display for AppError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse { line_num, source } => write!(f, "line {}: {}", line_num, source),
            Self::OutOfOrderDate { symbol, got, current } => write!(
                f,
                "out-of-order date for symbol={}: {} < {}",
                symbol,
                DateKey(*got),
                DateKey(*current)
            ),
            Self::OutOfOrderTime {
                symbol,
                date_key,
                got_ms,
                current_ms,
            } => write!(
                f,
                "out-of-order timestamp for symbol={}, date={}: {} < {}",
                symbol,
                DateKey(*date_key),
                HmsMsec(*got_ms),
                HmsMsec(*current_ms)
            ),
            Self::Io(e) => write!(f, "io error: {}", e),
        }
    }
}

impl std::error::Error for AppError {}

// reader.fill_buf()?
// writer.flush()?
// write!(...)?
impl From<io::Error> for AppError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

// 종목별 1분봉 리스트 값, RSI 누적값, VWAP, TWAP 누적값
#[derive(Debug, Clone, Default)]
struct SymbolAgg {
    cur_date_key: Option<u32>,
    cur_hour: Option<HourAgg>,
    cur_min: Option<MinuteAgg>,
    last_seen_ts_ms: Option<u32>,
    rsi_1m: RsiState,
}

impl SymbolAgg {
    // 체결시각 역전 검사
    fn check_ts_order(&self, tick: &ParsedTick<'_>) -> Result<(), AppError> {
        if let Some(cur_date_key) = self.cur_date_key {
            if tick.date_key < cur_date_key {
                return Err(AppError::OutOfOrderDate {
                    symbol: tick.symbol.to_owned(),
                    got: tick.date_key,
                    current: cur_date_key,
                });
            }

            if tick.date_key == cur_date_key
                && let Some(last_seen) = self.last_seen_ts_ms
                && tick.ts_ms < last_seen
            {
                return Err(AppError::OutOfOrderTime {
                    symbol: tick.symbol.to_owned(),
                    date_key: tick.date_key,
                    got_ms: tick.ts_ms,
                    current_ms: last_seen,
                });
            }
        }
        Ok(())
    }

    // 1분봉 리스트 및 RSI 출력
    fn write_minute_bar<W: Write>(&mut self, symbol: &str, bar: MinuteAgg, out: &mut W) -> Result<(), AppError> {
        let rsi = self.rsi_1m.on_close(bar.close as f64);
        let finalized = bar.finalize(rsi);
        writeln!(out, "{}", finalized.as_output(symbol))?;
        Ok(())
    }

    // 1분봉 리스트, RSI 및 VWAP, TWAP 출력
    fn flush_current<W: Write>(&mut self, symbol: &str, out: &mut W) -> Result<(), AppError> {
        if let Some(bar) = self.cur_min.take() {
            // 1분봉 리스트 및 RSI 출력
            self.write_minute_bar(symbol, bar, out)?;
        }

        if let Some(hour) = self.cur_hour.as_mut() {
            // 1시간 구간 끝까지 TWAP 면적 누적
            hour.add_twap(hour.end_ms());
        }

        if let Some(hour) = self.cur_hour.take() {
            // VWAP, TWAP 출력
            writeln!(out, "{}", hour.as_output(symbol))?;
        }

        self.last_seen_ts_ms = None;
        Ok(())
    }

    fn check_date<W: Write>(&mut self, tick: &ParsedTick<'_>, out: &mut W) -> Result<(), AppError> {
        match self.cur_date_key {
            None => {
                self.cur_date_key = Some(tick.date_key);
            }
            Some(cur_date_key) if cur_date_key == tick.date_key => {}
            Some(_) => {
                // 마지막으로 처리한 데이터의 날짜가 현재 처리할 체결시각 날짜보다 이전이라면,
                // 분봉 리스트, RSI 및 VWAP, TWAP 출력
                self.flush_current(tick.symbol, out)?;
                self.cur_date_key = Some(tick.date_key);
            }
        }
        Ok(())
    }

    fn check_hour<W: Write>(&mut self, tick: &ParsedTick<'_>, out: &mut W) -> Result<(), AppError> {
        let target_hour_start = HourAgg::floor_ms(tick.ts_ms); // 체결시각의 floor 시

        if self.cur_hour.is_none() {
            self.cur_hour = Some(HourAgg::new(tick.date_key, target_hour_start));
            return Ok(());
        }

        while self
            .cur_hour
            .as_ref()
            .map(|h| h.date_key < tick.date_key || (h.date_key == tick.date_key && h.hour_start_ms < target_hour_start))
            .unwrap_or(false)
        {
            let carry_price = {
                let hour = self.cur_hour.as_mut().expect("hour state should exist");
                // 이전 1시간 구간 끝까지 TWAP 면적 누적
                hour.add_twap(hour.end_ms());
                writeln!(out, "{}", hour.as_output(tick.symbol))?;
                hour.last_price
            };

            self.cur_hour = Some(HourAgg::with_carry(tick.date_key, target_hour_start, carry_price));
        }

        Ok(())
    }

    fn check_minute<W: Write>(&mut self, tick: &ParsedTick<'_>, out: &mut W) -> Result<(), AppError> {
        let target_min_start = MinuteAgg::floor_ms(tick.ts_ms);

        // 마지막 처리한 분봉이 현재 처리할 데이터의 체결시각 분보다 이전이라면, 마지막 분봉값들을 flush
        while self
            .cur_min
            .as_ref()
            .map(|m| m.date_key < tick.date_key || (m.date_key == tick.date_key && m.minute_start_ms < target_min_start))
            .unwrap_or(false)
        {
            if let Some(bar) = self.cur_min.take() {
                self.write_minute_bar(tick.symbol, bar, out)?;
            }
        }

        Ok(())
    }

    fn record_parsed_tick<W: Write>(&mut self, tick: &ParsedTick<'_>, out: &mut W) -> Result<(), AppError> {
        self.check_date(tick, out)?;
        self.check_hour(tick, out)?;
        self.check_minute(tick, out)?;

        self.cur_hour.as_mut().expect("hour state should exist").record_parsed_tick(tick);

        let min_start = MinuteAgg::floor_ms(tick.ts_ms);
        match self.cur_min.as_mut() {
            None => {
                self.cur_min = Some(MinuteAgg::new(tick.date_key, min_start, tick.price, tick.volume));
            }
            Some(bar) if bar.date_key == tick.date_key && bar.minute_start_ms == min_start => {
                bar.record_parsed_tick(tick);
            }
            Some(_) => {
                let old_bar = self.cur_min.take().expect("minute state should exist");
                self.write_minute_bar(tick.symbol, old_bar, out)?;
                self.cur_min = Some(MinuteAgg::new(tick.date_key, min_start, tick.price, tick.volume));
            }
        }

        self.cur_date_key = Some(tick.date_key);
        self.last_seen_ts_ms = Some(tick.ts_ms);
        Ok(())
    }

    fn flush<W: Write>(&mut self, symbol: &str, out: &mut W) -> Result<(), AppError> {
        self.flush_current(symbol, out)?;
        self.cur_date_key = None;
        Ok(())
    }
}

// 1분봉 리스트 계산
#[derive(Debug, Clone)]
struct MinuteAgg {
    date_key: u32,
    minute_start_ms: u32,
    open: u64,
    high: u64,
    low: u64,
    close: u64,
    volume: u64,
}

impl MinuteAgg {
    fn floor_ms(ms: u32) -> u32 {
        (ms / MSEC_PER_MIN) * MSEC_PER_MIN
    }

    fn new(date_key: u32, minute_start_ms: u32, price: u64, volume: u64) -> Self {
        Self {
            date_key,
            minute_start_ms,
            open: price,
            high: price,
            low: price,
            close: price,
            volume,
        }
    }

    fn record_parsed_tick(&mut self, tick: &ParsedTick<'_>) {
        if tick.price > self.high {
            self.high = tick.price;
        }
        if tick.price < self.low {
            self.low = tick.price;
        }
        self.close = tick.price;
        self.volume += tick.volume;
    }

    fn finalize(self, rsi14: Option<f64>) -> MinuteFinal {
        MinuteFinal {
            date_key: self.date_key,
            minute_start_ms: self.minute_start_ms,
            open: self.open,
            high: self.high,
            low: self.low,
            close: self.close,
            volume: self.volume,
            rsi14,
        }
    }
}

// 1분봉 리스트 및 RSI 출력
#[derive(Debug, Clone, Copy)]
struct MinuteFinal {
    date_key: u32,
    minute_start_ms: u32,
    open: u64,
    high: u64,
    low: u64,
    close: u64,
    volume: u64,
    rsi14: Option<f64>,
}

impl MinuteFinal {
    fn as_output<'a>(&'a self, symbol: &'a str) -> MinuteOutput<'a> {
        MinuteOutput { symbol, bar: self }
    }
}

struct MinuteOutput<'a> {
    symbol: &'a str,
    bar: &'a MinuteFinal,
}

impl Display for MinuteOutput<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "MINUTE,{},{},{},{},{},{},{},{},{},",
            DateKey(self.bar.date_key),
            self.symbol,
            Hms(self.bar.minute_start_ms),
            Hms(self.bar.minute_start_ms + MSEC_PER_MIN),
            self.bar.open,
            self.bar.high,
            self.bar.low,
            self.bar.close,
            self.bar.volume
        )?;

        if let Some(rsi) = self.bar.rsi14 {
            write!(f, "{:.8}", rsi)
        } else {
            Ok(())
        }
    }
}

// VWAP, TWAP 누적값
#[derive(Debug, Clone)]
struct HourAgg {
    date_key: u32,
    hour_start_ms: u32, // floor
    pv_sum: u128,       // 체결가 x 체결수량 합
    vol_sum: u64,       // 체결수량 합
    twap_num: u128,     // 가격 x 시간의 누적 면적
    last_price: Option<u64>,
    last_ts_ms: Option<u32>,
}

impl HourAgg {
    fn floor_ms(ms: u32) -> u32 {
        (ms / MSEC_PER_HOUR) * MSEC_PER_HOUR
    }

    fn new(date_key: u32, hour_start_ms: u32) -> Self {
        Self {
            date_key,
            hour_start_ms,
            pv_sum: 0,
            vol_sum: 0,
            twap_num: 0,
            last_price: None,
            last_ts_ms: None,
        }
    }

    // 이전 체결가 carry_price로 초기화
    fn with_carry(date_key: u32, hour_start_ms: u32, carry_price: Option<u64>) -> Self {
        Self {
            date_key,
            hour_start_ms,
            pv_sum: 0,
            vol_sum: 0,
            twap_num: 0,
            last_price: carry_price,
            last_ts_ms: carry_price.map(|_| hour_start_ms),
        }
    }

    // 1시간 끝
    fn end_ms(&self) -> u32 {
        self.hour_start_ms + MSEC_PER_HOUR
    }

    // TWAP 시간 가중 누적값을 한 구간씩 쌓음
    // 마지막으로 처리한 가격이 이전부터 until_ts_ms까지 유지됐다고 보고
    // 해당 구간의 가격 x 지속시간 (면적)을 누적
    fn add_twap(&mut self, until_ts_ms: u32) {
        let (last_price, last_ts_ms) = match (self.last_price, self.last_ts_ms) {
            (Some(price), Some(ts)) => (price, ts),
            _ => return,
        };

        if until_ts_ms <= last_ts_ms {
            return;
        }

        let duration_ms = until_ts_ms - last_ts_ms;
        self.twap_num += (last_price as u128) * (duration_ms as u128);
        self.last_ts_ms = Some(until_ts_ms);
    }

    fn record_parsed_tick(&mut self, tick: &ParsedTick<'_>) {
        // 현재 체결 데이터의 체결시각까지 TWAP 면적 누적
        self.add_twap(tick.ts_ms);

        // VWAP
        self.pv_sum += (tick.price as u128) * (tick.volume as u128);
        self.vol_sum += tick.volume;

        self.last_price = Some(tick.price);
        self.last_ts_ms = Some(tick.ts_ms);
    }

    fn as_output<'a>(&'a self, symbol: &'a str) -> HourOutput<'a> {
        HourOutput { symbol, hour: self }
    }
}

struct HourOutput<'a> {
    symbol: &'a str,
    hour: &'a HourAgg,
}

impl Display for HourOutput<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "HOUR,{},{},{},{},{:.8},{:.8}",
            DateKey(self.hour.date_key),
            self.symbol,
            Hms(self.hour.hour_start_ms),
            Hms(self.hour.end_ms()),
            self.hour.pv_sum as f64 / self.hour.vol_sum as f64, // VWAP
            self.hour.twap_num as f64 / MSEC_PER_HOUR as f64    // TWAP
        )
    }
}

// RSI(14) 누적값
#[derive(Debug, Clone, Default)]
struct RsiState {
    prev_close: Option<f64>,
    warmup_count: u32,
    gain_sum: f64,
    loss_sum: f64,
    avg_gain: Option<f64>,
    avg_loss: Option<f64>,
}

impl RsiState {
    fn on_close(&mut self, close: f64) -> Option<f64> {
        let prev = match self.prev_close {
            Some(v) => v,
            None => {
                self.prev_close = Some(close);
                return None;
            }
        };

        let change = close - prev;
        let gain = if change > 0.0 { change } else { 0.0 };
        let loss = if change < 0.0 { -change } else { 0.0 };

        self.prev_close = Some(close);

        if self.avg_gain.is_none() {
            self.gain_sum += gain;
            self.loss_sum += loss;
            self.warmup_count += 1;

            // RSI_PERIOD(14) 보다 누적값이 적으면 출력 안 함
            if self.warmup_count < RSI_PERIOD {
                return None;
            }

            let period = RSI_PERIOD as f64;
            let avg_gain = self.gain_sum / period;
            let avg_loss = self.loss_sum / period;
            self.avg_gain = Some(avg_gain);
            self.avg_loss = Some(avg_loss);
            return Some(Self::calc_rsi(avg_gain, avg_loss));
        }

        let period = RSI_PERIOD as f64;
        let prev_avg_gain = self.avg_gain.expect("avg_gain should exist");
        let prev_avg_loss = self.avg_loss.expect("avg_loss should exist");

        let avg_gain = ((prev_avg_gain * (period - 1.0)) + gain) / period;
        let avg_loss = ((prev_avg_loss * (period - 1.0)) + loss) / period;

        self.avg_gain = Some(avg_gain);
        self.avg_loss = Some(avg_loss);

        Some(Self::calc_rsi(avg_gain, avg_loss))
    }

    fn calc_rsi(avg_gain: f64, avg_loss: f64) -> f64 {
        if avg_loss == 0.0 {
            if avg_gain == 0.0 { 50.0 } else { 100.0 }
        } else {
            let rs = avg_gain / avg_loss;
            100.0 - (100.0 / (1.0 + rs))
        }
    }
}

// 총 거래량/대금
#[derive(Debug, Clone, Default)]
struct Summary {
    total_volume: u128, // 총 거래량
    total_pv: u128,     // 총 거래대금
}

impl Summary {
    fn record_parsed_tick(&mut self, tick: &ParsedTick<'_>) {
        self.total_volume += tick.volume as u128;
        self.total_pv += (tick.price as u128) * (tick.volume as u128);
    }

    fn as_output(&self) -> SummaryOutput<'_> {
        SummaryOutput { summary: self }
    }
}

struct SummaryOutput<'a> {
    summary: &'a Summary,
}

impl Display for SummaryOutput<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        writeln!(f, "SUMMARY,TOTAL_VOLUME,{}", self.summary.total_volume)?;
        writeln!(f, "SUMMARY,TOTAL_TRADED_VALUE,{}", self.summary.total_pv)
    }
}

#[derive(Debug, Clone, Copy)]
struct DateKey(u32); // ParsedTs::date_key

impl Display for DateKey {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let year = self.0 / 10_000;
        let month = (self.0 / 100) % 100;
        let day = self.0 % 100;
        write!(f, "{:04}-{:02}-{:02}", year, month, day)
    }
}

#[derive(Debug, Clone, Copy)]
struct Hms(u32); // ParsedTs::ms_of_day

impl Display for Hms {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let total_sec = self.0 / 1000;
        let hh = total_sec / 3600;
        let mm = (total_sec % 3600) / 60;
        let ss = total_sec % 60;
        write!(f, "{:02}:{:02}:{:02}", hh, mm, ss)
    }
}

// 에러 출력용
#[derive(Debug, Clone, Copy)]
struct HmsMsec(u32);

impl Display for HmsMsec {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let total_sec = self.0 / 1000;
        let msec = self.0 % 1000;
        let hh = total_sec / 3600;
        let mm = (total_sec % 3600) / 60;
        let ss = total_sec % 60;
        write!(f, "{:02}:{:02}:{:02}.{:03}", hh, mm, ss, msec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tick<'a>(symbol: &'a str, date_key: u32, ts_ms: u32, price: u64, volume: u64) -> ParsedTick<'a> {
        ParsedTick {
            symbol,
            date_key,
            ts_ms,
            price,
            volume,
        }
    }

    #[test]
    fn rsi_is_none() {
        let mut rsi = RsiState::default();

        for close in [
            100.0, 101.0, 102.0, 103.0, 104.0, 105.0, 106.0, 107.0, 108.0, 109.0, 110.0, 111.0, 112.0, 113.0,
        ] {
            assert!(rsi.on_close(close).is_none());
        }

        let first = rsi.on_close(114.0);
        assert!(first.is_some());
    }

    #[test]
    fn rsi_is_100() {
        let mut rsi = RsiState::default();
        let mut value = None;

        for close in 1..=15 {
            value = rsi.on_close(close as f64);
        }

        let rsi = value.expect("rsi should be available");
        assert!((rsi - 100.0).abs() < 1e-12);
    }

    #[test]
    fn rsi_is_50() {
        let mut rsi = RsiState::default();
        let mut value = None;

        for _ in 0..15 {
            value = rsi.on_close(100.0);
        }

        let rsi = value.expect("rsi should be available");
        assert!((rsi - 50.0).abs() < 1e-12);
    }

    #[test]
    fn flush_previous_minute() {
        let mut state = SymbolAgg::default();
        let mut out = Vec::new();

        let t1 = tick("AB34", 20260723, 9 * MSEC_PER_HOUR + 1_000, 100, 1);
        let t2 = tick("AB34", 20260723, 9 * MSEC_PER_HOUR + 30_000, 101, 2);
        let t3 = tick("AB34", 20260723, 9 * MSEC_PER_HOUR + MSEC_PER_MIN + 1_000, 102, 3);

        state.record_parsed_tick(&t1, &mut out).unwrap();
        state.record_parsed_tick(&t2, &mut out).unwrap();
        state.record_parsed_tick(&t3, &mut out).unwrap();

        assert!(state.cur_min.is_some());

        _ = state.flush("AB34", &mut out);

        assert!(state.cur_min.is_none());

        let s = String::from_utf8(out).unwrap();

        // 1분봉 리스트, RSI, VWAP, TWAP값 비교
        assert!(s.contains("MINUTE,2026-07-23,AB34,09:00:00,09:01:00,100,101,100,101,3,"));
        assert!(s.contains("MINUTE,2026-07-23,AB34,09:01:00,09:02:00,102,102,102,102,3,"));
        assert!(s.contains("HOUR,2026-07-23,AB34,09:00:00,10:00:00,101.33333333,101.94694444"));
    }

    #[test]
    fn flush_previous_day() {
        let mut state = SymbolAgg::default();
        let mut out = Vec::new();

        let t1 = tick("AB34", 20260723, 23 * MSEC_PER_HOUR + 59 * MSEC_PER_MIN, 100, 1);
        let t2 = tick("AB34", 20260724, 0, 101, 1);

        state.record_parsed_tick(&t1, &mut out).unwrap();
        state.record_parsed_tick(&t2, &mut out).unwrap();

        assert!(state.cur_hour.is_some());

        _ = state.flush("AB34", &mut out);

        assert!(state.cur_hour.is_none());

        let s = String::from_utf8(out).unwrap();

        // 1분봉 리스트, RSI, VWAP, TWAP값 비교
        assert!(s.contains("MINUTE,2026-07-23,AB34,23:59:00,24:00:00,100,100,100,100,1,"));
        assert!(s.contains("HOUR,2026-07-23,AB34,23:00:00,24:00:00,100.00000000,1.66666667"));
        assert!(s.contains("MINUTE,2026-07-24,AB34,00:00:00,00:01:00,101,101,101,101,1,"));
        assert!(s.contains("HOUR,2026-07-24,AB34,00:00:00,01:00:00,101.00000000,101.00000000"));
    }

    #[test]
    fn err_out_of_order_ts() {
        let mut state = SymbolAgg::default();
        let t1 = tick("AB34", 20260723, 9 * MSEC_PER_HOUR + 10_000, 100, 1);
        let t2 = tick("AB34", 20260723, 9 * MSEC_PER_HOUR + 9_000, 101, 1);

        state.cur_date_key = Some(t1.date_key);
        state.last_seen_ts_ms = Some(t1.ts_ms);

        match state.check_ts_order(&t2).unwrap_err() {
            AppError::OutOfOrderTime { .. } => {}
            other => panic!("unexpected error: {}", other),
        }
    }

    #[test]
    fn parser_accepts_five_columns() {
        let parsed = ParsedTick::parse(b"AB34,2026-07-23T09:57:01.357,100,7,BUY").unwrap().unwrap();

        assert_eq!(parsed.symbol, "AB34");
        assert_eq!(parsed.date_key, 20260723);
        assert_eq!(parsed.ts_ms, 9 * MSEC_PER_HOUR + 57 * MSEC_PER_MIN + 1_357);
        assert_eq!(parsed.price, 100);
        assert_eq!(parsed.volume, 7);
    }
}
