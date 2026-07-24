mod agg;
mod parse;

use agg::{Aggregator, AppError};
use std::env;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, IsTerminal, Write};

const INPUT_BUF_SIZE: usize = 1024 * 1024;

fn main() -> Result<(), AppError> {
    let args: Vec<String> = env::args().collect();

    // 입력으로 첫 번째 positional argument 있으면 파일 입력으로 사용하고
    // 없으면 stdin이 tty가 아니면 입력으로 사용하고
    // 둘 다 아니면 에러
    let mut reader: Box<dyn BufRead> = if args.len() > 1 {
        Box::new(BufReader::with_capacity(INPUT_BUF_SIZE, File::open(&args[1])?))
    } else if !io::stdin().is_terminal() {
        Box::new(BufReader::with_capacity(INPUT_BUF_SIZE, io::stdin().lock()))
    } else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "nothing to read").into());
    };

    // 출력으로 두 번째 positional argument 있으면 파일 출력으로 사용하고
    // 없으면 stdout으로 출력
    let mut writer: Box<dyn Write> = if args.len() > 2 {
        Box::new(BufWriter::new(OpenOptions::new().create(true).append(true).open(&args[2])?))
    } else {
        Box::new(BufWriter::new(io::stdout().lock()))
    };

    let mut agg = Aggregator::new();
    let mut carry: Vec<u8> = Vec::with_capacity(1024);
    let mut line_num: usize = 0; // 에러 출력용

    loop {
        let amt = {
            let buf = reader.fill_buf()?;
            if buf.is_empty() {
                if !carry.is_empty() {
                    line_num += 1;
                    agg.process_line(&carry, line_num, &mut writer)?;
                    carry.clear();
                }
                break;
            }

            let mut start = 0usize;

            for i in 0..buf.len() {
                // LF 안 들어감
                if buf[i] == b'\n' {
                    line_num += 1;
                    let line_part = &buf[start..i];

                    if carry.is_empty() {
                        agg.process_line(line_part, line_num, &mut writer)?;
                    } else {
                        // 이전 buf에서 마지막 chunk가 있음
                        carry.extend_from_slice(line_part);
                        agg.process_line(&carry, line_num, &mut writer)?;
                        carry.clear();
                    }

                    start = i + 1;
                }
            }

            // 현재 buf의 마지막 chunk
            if start < buf.len() {
                carry.extend_from_slice(&buf[start..]);
            }

            buf.len()
        };

        reader.consume(amt);
    }

    agg.flush(&mut writer)?;
    writer.flush()?;
    Ok(())
}
