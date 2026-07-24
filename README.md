# 실행 방법

## Build

```bash
cargo build --release
```

## Test

```bash
cargo test
```

## 실행

- positional argument로 입력 파일를 지정할 수 있습니다.
- 출력 결과는 CSV 형식입니다.

```bash
target/release/mx-task ticks_{symbol}.csv
```

- pipe로 입력하는 것도 가능합니다.

```bash
cat ticks_{symbol}.csv | target/release/mx-task
```

- 기본 출력은 stdout이고, 두 번째 positional argument로 출력 파일을 지정할 수 있습니다.

```bash
target/release/mx-task ticks_{symbol}.csv output.csv
```

# 선정 지표

- RSI (상대강도지소; relative strength index)
  - 14개 가격 사용
  - 일정 기간 동안의 가격이 빠르고 올랐는지 또는 내렸는지를 알 수 있는 지표로서 시장의 흐름을 빠르게 파악하는데 도움이 될 수 있다고 판단하여 선택하였습니다.

- TWAP
  - 이전 가격이 유지된 시간을 기준으로 누적한 평균 가격으로 계산하였습니다.
    - 급격한 순간 변동에 덜 민감하여 오랫동안 유지된 가격을 더 비중있게 반영하고, 실제 체감되는 평균값이라고 보고 선정하였습니다.

# 심화

- 대용량 입력
  - 메모리 할당을 최소화하고자 입력된 체결 데이터를 읽을 때 가급적 숫자(u32 등)나 바이트 슬라이스 형태로 유지하면서 계산하였습니다.
  - 의도한 것은 아닙니다만, 할당을 최소화환 간단한 CSV 파서를 만들어서 체결 데이터를 파싱하였습니다.
    - CSV 구조가 변경되면, 파서에 적지않은 수정이 필요할 수 있어 이에 따른 단점도 있습니다.
  - 또한, 체결 데이터를 읽을 때에도 지정한 크기(1MB; 하드코딩)의 버퍼와 `BufRead::fill_buf()`, `BufRead::consume()`를 사용하여 필요 이상의 메모리 사용을 자제하였습니다.

# 출력 포맷

```
MINUTE,{날짜},{종목코드},{1분 시작 시각},{1분 종료 시각},{시가},{고가},{저가},{종가},{거래량},{RSI(14)}
HOUR,{날짜},{종목코드},{1시간 시작 시각},{1시간 종료 시각},{VWAP},{TWAP}
SUMMARY,TOTAL_VOLUME,{총 거래량}
SUMMARY,TOTAL_TRADED_VALUE,{총 거래대금}
```

- RSI(14)값은 14개 종가가 쌓이기 전까지는 비어 있습니다.

## 예

```
$ target/release/mx-task ticks_005930.csv

MINUTE,2026-07-07,005930,09:00:00,09:01:00,173400,173900,173000,173900,1080153,
...
MINUTE,2026-07-07,005930,09:59:00,10:00:00,171200,171200,171000,171100,126251,32.80139116
HOUR,2026-07-07,005930,09:00:00,10:00:00,172778.84098921,172622.36963889
SUMMARY,TOTAL_VOLUME,11422344
SUMMARY,TOTAL_TRADED_VALUE,1973539357700
```
