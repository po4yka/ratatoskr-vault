//! Probe executable for runner-property tests.
//!
//! This integration-test target compiles with `harness = false`, so it is a plain executable
//! that lives next to the real test binaries. Bounds and environment tests point
//! `RunConfig.git_binary` at it (the Git binary location is trusted configuration, so a test may
//! substitute its own probe) and drive it purely through the runner: arguments, environment, and
//! working directory are whatever the runner decides to give it.
//!
//! Behaviour comes from a `probe-instructions` file in the process working directory, because the
//! runner deliberately offers no channel to inject arbitrary environment variables or arguments.
//! When run by `cargo test` directly (no instructions present) it simply exits 0.
//!
//! Instruction lines are `key: value` pairs; unknown keys are refused loudly so a typo cannot
//! silently turn an attack test into a green one.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::print_stdout,
    reason = "printing to stdout IS the probe's function; it is a test fixture executable"
)]

use std::io::{Read, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let Ok(instructions) = std::fs::read_to_string("probe-instructions") else {
        return ExitCode::SUCCESS;
    };

    let mut mode = String::new();
    let mut stream = String::from("stdout");
    let mut bytes: usize = 0;
    let mut text = String::new();
    let mut exit_code: i32 = 0;

    for line in instructions.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            eprintln!("probe: malformed instruction line {line:?}");
            return ExitCode::FAILURE;
        };
        match key.trim() {
            "mode" => mode = value.trim().to_string(),
            "stream" => stream = value.trim().to_string(),
            "bytes" => {
                let Ok(parsed) = value.trim().parse() else {
                    eprintln!("probe: `bytes` must be a number");
                    return ExitCode::FAILURE;
                };
                bytes = parsed;
            }
            "text" => text = value.trim().to_string(),
            "exit_code" => {
                let Ok(parsed) = value.trim().parse() else {
                    eprintln!("probe: `exit_code` must be a number");
                    return ExitCode::FAILURE;
                };
                exit_code = parsed;
            }
            other => {
                eprintln!("probe: unknown instruction key {other:?}");
                return ExitCode::FAILURE;
            }
        }
    }

    if instructions.is_empty() {
        return ExitCode::SUCCESS;
    }

    match mode.as_str() {
        // Print argv, then the full environment: everything an outside observer could learn
        // about how this process was started.
        "argv-env" => {
            for (index, argument) in std::env::args_os().enumerate() {
                println!(
                    "arg{index}={}",
                    argument.to_string_lossy().replace('\n', " ")
                );
            }
            for (key, value) in std::env::vars_os() {
                println!("{}={}", key.to_string_lossy(), value.to_string_lossy());
            }
        }
        // Print the full environment: the observable state of the child's env block.
        "env" => {
            for (key, value) in std::env::vars_os() {
                println!("{}={}", key.to_string_lossy(), value.to_string_lossy());
            }
        }
        // Never terminate on its own: records its pid, then waits for stdin forever,
        // exercising the runner deadline and the kill that must follow it.
        "hang" => {
            hang_parent();
        }
        "hang-with-descendant" => {
            hang_with_descendant();
        }
        // Emit `bytes` pattern bytes (or the literal `text`) on the requested stream(s).
        "emit" => {
            let payload: Vec<u8> = if text.is_empty() {
                vec![b'x'; bytes]
            } else {
                text.into_bytes()
            };
            if stream == "stdout" || stream == "both" {
                let _ignored = std::io::stdout().write_all(&payload);
            }
            if stream == "stderr" || stream == "both" {
                let _ignored = std::io::stderr().write_all(&payload);
            }
        }
        "mark" => {
            std::fs::write("probe-marker", b"spawned").expect("probe marker file must be writable");
        }
        other => {
            eprintln!("probe: unknown mode {other:?}");
            return ExitCode::FAILURE;
        }
    }

    if exit_code == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(u8::try_from(exit_code).unwrap_or(1))
    }
}

fn hang_parent() {
    std::fs::write("probe-pid", format!("{}\n", std::process::id()))
        .expect("probe pid file must be writable");
    let mut sink = [0_u8; 4096];
    loop {
        let read = std::io::stdin().read(&mut sink).unwrap_or(0);
        if read == 0 {
            break;
        }
    }
}

fn hang_with_descendant() {
    std::fs::write("probe-pid", format!("{}\n", std::process::id()))
        .expect("probe pid file must be writable");
    let mut descendant = std::process::Command::new("/bin/sleep")
        .arg("60")
        .spawn()
        .expect("probe descendant must start");
    std::fs::write("probe-descendant-pid", format!("{}\n", descendant.id()))
        .expect("descendant pid file must be writable");
    let mut sink = [0_u8; 4096];
    loop {
        let read = std::io::stdin().read(&mut sink).unwrap_or(0);
        if read == 0 {
            break;
        }
    }
    let _ignored = descendant.kill();
    let _ignored = descendant.wait();
}
