use std::fmt::{self, Display, Formatter};
use std::str::Utf8Error;

// 체결 데이터
pub(crate) struct ParsedTick<'a> {
    pub(crate) symbol: &'a str,
    pub(crate) date_key: u32,
    pub(crate) ts_ms: u32,
    pub(crate) price: u64,
    pub(crate) volume: u64,
}

impl<'a> ParsedTick<'a> {
    fn parse_unsigned_u64(bytes: &[u8]) -> Result<u64, ParseError> {
        if bytes.is_empty() {
            return Err(ParseError::EmptyColumn);
        }

        let mut v: u64 = 0;
        for &b in bytes {
            if !b.is_ascii_digit() {
                return Err(ParseError::InvalidDigit);
            }
            v = v
                .checked_mul(10)
                .and_then(|x| x.checked_add((b - b'0') as u64))
                .ok_or(ParseError::NumericOverflow)?;
        }
        Ok(v)
    }

    // comma 전후 인덱스 찾아서 5개 튜플 배열로 리턴
    fn pinpoint_columns(line: &[u8]) -> Result<[(usize, usize); 5], ParseError> {
        let mut ranges = [(0usize, 0usize); 5];
        let mut field_idx = 0usize;
        let mut start = 0usize;

        for (i, &b) in line.iter().enumerate() {
            if b == b',' {
                if field_idx >= 5 {
                    return Err(ParseError::TooManyColumns);
                }
                ranges[field_idx] = (start, i);
                field_idx += 1;
                start = i + 1;
            }
        }

        if field_idx != 4 {
            return Err(ParseError::InvalidColumnCount);
        }
        ranges[4] = (start, line.len());
        Ok(ranges)
    }

    pub(crate) fn parse(line: &'a [u8]) -> Result<Option<Self>, ParseError> {
        let line = match line.last() {
            // 윈도우 EOL CR 제거
            Some(b'\r') => &line[..line.len() - 1],
            _ => line,
        };

        // 빈 줄이거나, CSV 헤더이면 아무 것도 안 함
        if line.is_empty() || line == b"symbol,timestamp,price,volume,side" {
            return Ok(None);
        }

        let columns = Self::pinpoint_columns(line)?;

        let symbol_bytes = &line[columns[0].0..columns[0].1];
        let ts_bytes = &line[columns[1].0..columns[1].1];
        let price_bytes = &line[columns[2].0..columns[2].1];
        let volume_bytes = &line[columns[3].0..columns[3].1];
        let _side_bytes = &line[columns[4].0..columns[4].1];

        let symbol = std::str::from_utf8(symbol_bytes)?;
        let parsed_ts = ParsedTs::parse(ts_bytes)?;
        let price = Self::parse_unsigned_u64(price_bytes)?;
        let volume = Self::parse_unsigned_u64(volume_bytes)?;

        Ok(Some(Self {
            symbol,
            date_key: parsed_ts.date_key,
            ts_ms: parsed_ts.ms_of_day,
            price,
            volume,
        }))
    }
}

#[derive(Debug)]
pub(crate) enum ParseError {
    EmptyColumn,
    InvalidDigit,
    NumericOverflow,
    InvalidDatetime,
    InvalidColumnCount,
    TooManyColumns,
    InvalidUtf8(Utf8Error),
}

impl Display for ParseError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyColumn => write!(f, "empty column"),
            Self::InvalidDigit => write!(f, "invalid digit"),
            Self::NumericOverflow => write!(f, "numeric overflow"),
            Self::InvalidDatetime => write!(f, "invalid datetime"),
            Self::InvalidColumnCount => write!(f, "invalid column count"),
            Self::TooManyColumns => write!(f, "too many columns"),
            Self::InvalidUtf8(e) => write!(f, "invalid utf-8: {}", e),
        }
    }
}

impl std::error::Error for ParseError {}

// std::str::from_utf8(...)?
impl From<Utf8Error> for ParseError {
    fn from(value: Utf8Error) -> Self {
        Self::InvalidUtf8(value)
    }
}

#[derive(Debug, Clone, Copy)]
struct ParsedTs {
    date_key: u32,  // YYYYMMDD 정수
    ms_of_day: u32, // 해당일 0시부터의 밀리초
}

impl ParsedTs {
    // 바이트 슬라이스를 십진수로 변환
    fn dec(bytes: &[u8]) -> Result<u32, ParseError> {
        if bytes.is_empty() {
            return Err(ParseError::EmptyColumn);
        }

        let mut v = 0u32;
        for &b in bytes {
            if !b.is_ascii_digit() {
                return Err(ParseError::InvalidDigit);
            }
            v = v
                .checked_mul(10)
                .and_then(|x| x.checked_add((b - b'0') as u32))
                .ok_or(ParseError::NumericOverflow)?;
        }
        Ok(v)
    }

    // 2026-07-07T09:00:14.125 => 20260707 (u32), milliseconds of day (u32)
    // 유효한 calendar 날짜인지 검증하지 않음; e.g. 2월 30일 통과
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        if bytes.len() < 19 {
            return Err(ParseError::InvalidDatetime);
        }

        if bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' || bytes[13] != b':' || bytes[16] != b':' {
            return Err(ParseError::InvalidDatetime);
        }

        let year = Self::dec(&bytes[0..4])?;
        let month = Self::dec(&bytes[5..7])?;
        let day = Self::dec(&bytes[8..10])?;
        let hour = Self::dec(&bytes[11..13])?;
        let minute = Self::dec(&bytes[14..16])?;
        let second = Self::dec(&bytes[17..19])?;

        // 소수점 없으면 SS.0 초
        let msec = if bytes.len() == 19 {
            0
        } else {
            if bytes[19] != b'.' {
                return Err(ParseError::InvalidDatetime);
            }

            let frac = &bytes[20..];
            if frac.is_empty() {
                return Err(ParseError::InvalidDatetime);
            }

            let frac_len = frac.len().min(3);
            let head = Self::dec(&frac[..frac_len])?;

            if frac.len() > 3 {
                for &b in &frac[3..] {
                    if !b.is_ascii_digit() {
                        return Err(ParseError::InvalidDatetime);
                    }
                }
            }

            match frac_len {
                1 => head * 100,
                2 => head * 10,
                _ => head,
            }
        };

        if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return Err(ParseError::InvalidDatetime);
        }

        if hour > 23 || minute > 59 || second > 59 || msec > 999 {
            return Err(ParseError::InvalidDatetime);
        }

        // 연도 x 1만 + 월 x 100 + 일 => u32로 만듦
        let date_key = year
            .checked_mul(10_000)
            .and_then(|x| x.checked_add(month * 100))
            .and_then(|x| x.checked_add(day))
            .ok_or(ParseError::NumericOverflow)?;

        // 해당일 0시부터 해당 시각까지 밀리초
        let ms_of_day = (((hour * 60 + minute) * 60 + second) * 1000)
            .checked_add(msec)
            .ok_or(ParseError::NumericOverflow)?;

        Ok(Self { date_key, ms_of_day })
    }
}
