//! `at9` — command-line front end for the at9codec library.
//!
//! Usage:
//!   at9 info   <in.at9>
//!   at9 decode <in.at9> <out.wav>
//!   at9 --version

use std::process::ExitCode;

fn usage() {
    eprintln!(
        "at9 {} — clean-room ATRAC9 codec\n\n\
         USAGE:\n  \
         at9 info   <in.at9>            Print container metadata\n  \
         at9 decode <in.at9> <out.wav>  Decode AT9 to 16-bit PCM WAV\n  \
         at9 --version",
        at9codec::VERSION
    );
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") | Some("-V") => {
            println!("at9 {}", at9codec::VERSION);
            ExitCode::SUCCESS
        }
        Some("info") if args.len() == 2 => cmd_info(&args[1]),
        Some("decode") if args.len() == 3 => cmd_decode(&args[1], &args[2]),
        _ => {
            usage();
            ExitCode::FAILURE
        }
    }
}

fn cmd_info(path: &str) -> ExitCode {
    let buf = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let c = match at9codec::parse(&buf) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("file            : {path}");
    println!("sample_rate     : {} Hz", c.sample_rate);
    println!("channels        : {}", c.channels);
    println!("block_align     : {}", c.block_align);
    println!("total_samples   : {} (true length)", c.total_samples);
    println!(
        "duration        : {:.3} s",
        c.total_samples as f64 / c.sample_rate as f64
    );
    println!("encoder_delay   : {} samples", c.encoder_delay);
    println!(
        "config          : raw={:02X?} rate_idx={} ch_cfg={} frame_bytes={} frames/superframe={}",
        c.config.raw,
        c.config.sample_rate_index,
        c.config.channel_config_index,
        c.config.frame_bytes,
        c.config.frames_per_superframe
    );
    println!(
        "superframes     : {} ({} samples each)",
        c.superframe_count(),
        c.config.superframe_samples
    );
    match c.loop_info {
        Some(l) => println!(
            "loop            : start={} end={} ({}){}",
            l.start,
            l.end,
            if l.is_whole { "whole track" } else { "partial" },
            format_args!(
                "  [{:.3}s .. {:.3}s]",
                l.start as f64 / c.sample_rate as f64,
                l.end as f64 / c.sample_rate as f64
            )
        ),
        None => println!("loop            : none"),
    }
    println!("data            : offset={} len={}", c.data_offset, c.data_len);
    ExitCode::SUCCESS
}

fn cmd_decode(input: &str, output: &str) -> ExitCode {
    let buf = match std::fs::read(input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: cannot read {input}: {e}");
            return ExitCode::FAILURE;
        }
    };
    match at9codec::decode_to_pcm16(&buf) {
        Ok(pcm) => {
            let f = match std::fs::File::create(output) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("error: cannot create {output}: {e}");
                    return ExitCode::FAILURE;
                }
            };
            if let Err(e) = at9codec::wav::write_pcm16(
                std::io::BufWriter::new(f),
                pcm.channels,
                pcm.sample_rate,
                &pcm.samples,
            ) {
                eprintln!("error: writing {output}: {e}");
                return ExitCode::FAILURE;
            }
            println!("decoded {} frames -> {output}", pcm.frames());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
