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
         at9 encode [options] <in.wav> <out.at9>\n                                 Encode 16-bit PCM WAV to AT9.\n    Size:   --vita-system-bgm  cap the file under 5 MB (PS Vita theme BGM)\n            --ps-cross         cap at 2 MiB (works on PS4/PS5 too)\n                               (no size flag = no cap, general game AT9)\n    Loop:   --loop             loop the whole track\n            --loop-start <n>   loop from sample n (in input-WAV samples)\n            --loop-end <n>     inclusive last sample of the loop\n                               (omit for the track end; no loop flag = no loop)\n  \
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
        Some("encode") if args.len() >= 3 => cmd_encode(&args[1..]),
        Some("rewrap") if args.len() == 3 => cmd_rewrap(&args[1], &args[2]),
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

/// Re-wrap an existing AT9 file's frame data with our own container writer.
/// Diagnostic: isolates whether our container (vs our frame encoding) is valid.
fn cmd_rewrap(input: &str, output: &str) -> ExitCode {
    let buf = std::fs::read(input).expect("read");
    let c = at9codec::parse(&buf).expect("parse");
    let data = c.data(&buf).to_vec();
    let cfg = at9codec::encode::EncoderConfig {
        sample_rate_index: c.config.sample_rate_index as usize,
        channel_config_index: c.config.channel_config_index as usize,
        superframe_index: c.config.superframe_index as u32,
        frame_bytes: c.config.frame_bytes as u32,
        sample_rate: c.sample_rate,
        channels: c.channels,
        frame_samples: c.config.frame_samples as usize,
        frames_per_superframe: c.config.frames_per_superframe as usize,
        superframe_bytes: c.block_align as u32,
    };
    let out = at9codec::encode::build_at9_container(
        &cfg, &data, c.total_samples, c.encoder_delay,
        c.loop_info.map(|l| l.start),
        c.loop_info.map(|l| l.end),
    );
    std::fs::write(output, &out).expect("write");
    println!("rewrapped {} -> {output} ({} bytes)", input, out.len());
    ExitCode::SUCCESS
}

fn cmd_encode(args: &[String]) -> ExitCode {
    use at9codec::encode::SizeLimit;
    let mut vita_bgm = false;
    let mut ps_cross = false;
    let mut whole_loop = false;
    let mut loop_start: Option<u32> = None;
    let mut loop_end: Option<u32> = None;
    let mut positional: Vec<&str> = Vec::new();

    // Parse flags, consuming a value after --loop-start / --loop-end.
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--vita-system-bgm" => vita_bgm = true,
            "--ps-cross" => ps_cross = true,
            "--loop" => whole_loop = true,
            "--loop-start" | "--loop-end" => {
                let name = args[i].clone();
                i += 1;
                match args.get(i).and_then(|s| s.parse::<u32>().ok()) {
                    Some(n) if name == "--loop-start" => loop_start = Some(n),
                    Some(n) => loop_end = Some(n),
                    None => {
                        eprintln!("error: {name} needs a sample-number value");
                        return ExitCode::FAILURE;
                    }
                }
            }
            s if s.starts_with("--") => {
                eprintln!("error: unknown option {s}");
                usage();
                return ExitCode::FAILURE;
            }
            _ => positional.push(args[i].as_str()),
        }
        i += 1;
    }
    if positional.len() != 2 {
        usage();
        return ExitCode::FAILURE;
    }
    let (input, output) = (positional[0], positional[1]);
    let buf = match std::fs::read(input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: cannot read {input}: {e}");
            return ExitCode::FAILURE;
        }
    };
    // --ps-cross is the tighter 2 MiB cap (PS4/PS5); --vita-system-bgm is the
    // Vita's under-5 MB cap; neither flag = no cap (general game AT9). The codec
    // owns the actual limits — the flags just name the target.
    let size_limit = if ps_cross {
        SizeLimit::PsCross
    } else if vita_bgm {
        SizeLimit::VitaSystemBgm
    } else {
        SizeLimit::None
    };
    // A loop exists if any loop flag is given. --loop-start sets where it repeats
    // from; --loop (or a bare --loop-end) means the whole track (from sample 0).
    // --loop-end is the inclusive last sample; omit it to loop to the track end.
    let loop_start = match loop_start {
        Some(n) => Some(n),
        None if whole_loop || loop_end.is_some() => Some(0),
        None => None,
    };
    let loop_end = if loop_start.is_some() { loop_end } else { None };
    let opts = at9codec::encode::EncodeOptions {
        size_limit,
        loop_start,
        loop_end,
        ..Default::default()
    };
    match at9codec::encode::encode_wav(&buf, &opts) {
        Ok(at9) => {
            if let Err(e) = std::fs::write(output, &at9) {
                eprintln!("error: writing {output}: {e}");
                return ExitCode::FAILURE;
            }
            println!("encoded {} bytes -> {output}", at9.len());
            ExitCode::SUCCESS
        }
        Err(at9codec::error::Error::ExceedsSizeLimit) => {
            eprintln!(
                "error: the track is too long to fit the size limit even at the lowest \
                 bitrate — the console will not use it; shorten the track."
            );
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
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
