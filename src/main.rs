// Copyright (C) 2026 Henry Abrahamsen
//
// This file is part of RPSFish.
//
// RPSFish is free software: you can redistribute it and/or modify it under the
// terms of the GNU Lesser General Public License as published by the Free
// Software Foundation, either version 3 of the License, or (at your option) any
// later version.
//
// RPSFish is distributed in the hope that it will be useful, but WITHOUT ANY
// WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR
// A PARTICULAR PURPOSE. See the GNU Lesser General Public License for more
// details.
//
// You should have received a copy of the GNU Lesser General Public License
// along with RPSFish. If not, see <https://www.gnu.org/licenses/>.
//
// SPDX-License-Identifier: LGPL-3.0-or-later

use std::env;
use std::process::ExitCode;
use std::time::Duration;

use rpsfish::{
    AnalysisLine, AnalysisUpdate, EvalParams, Mode, Position, SearchLimits, Searcher, perft,
};

const MAX_SEARCH_DEPTH: u8 = 127;

fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            print_usage();
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: Vec<String>) -> Result<(), String> {
    let command = arguments.first().map_or("search", String::as_str);
    match command {
        "search" => {
            let options = parse_search_options(&arguments[1..])?;
            run_search(options);
            Ok(())
        }
        "perft" => {
            let mode = parse_mode(arguments.get(1).map_or("annihilation", String::as_str))?;
            let depth = parse_depth(arguments.get(2), 4)?;
            run_perft(mode, depth);
            Ok(())
        }
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        unknown => Err(format!("unknown command {unknown:?}")),
    }
}

#[derive(Clone, Copy, Debug)]
struct SearchOptions {
    mode: Mode,
    limits: SearchLimits,
    variations: usize,
    hash_megabytes: usize,
    params: EvalParams,
}

fn parse_search_options(arguments: &[String]) -> Result<SearchOptions, String> {
    let deep = arguments.iter().any(|argument| argument == "--deep");
    let mut options = SearchOptions {
        mode: Mode::Annihilation,
        limits: if deep {
            SearchLimits {
                max_depth: MAX_SEARCH_DEPTH,
                max_nodes: Some(50_000_000),
                move_time: Some(Duration::from_secs(60)),
            }
        } else {
            SearchLimits {
                max_depth: 8,
                max_nodes: Some(5_000_000),
                move_time: Some(Duration::from_secs(10)),
            }
        },
        variations: 1,
        hash_megabytes: if deep { 32 } else { 16 },
        params: EvalParams::DEFAULT,
    };

    let mut index = 0;
    if let Some(mode) = arguments.first().filter(|value| !value.starts_with('-')) {
        options.mode = parse_mode(mode)?;
        index += 1;
    }
    if let Some(depth) = arguments.get(index).filter(|value| !value.starts_with('-')) {
        options.limits.max_depth = parse_depth(Some(depth), options.limits.max_depth)?;
        index += 1;
    }

    while index < arguments.len() {
        let flag = arguments[index].as_str();
        index += 1;
        match flag {
            "--deep" => {}
            "--depth" => {
                options.limits.max_depth = parse_depth(arguments.get(index), 0)?;
                index += 1;
            }
            "--nodes" => {
                options.limits.max_nodes =
                    Some(parse_positive::<u64>(arguments.get(index), "node limit")?);
                index += 1;
            }
            "--time-ms" => {
                let milliseconds = parse_positive::<u64>(arguments.get(index), "time limit")?;
                options.limits.move_time = Some(Duration::from_millis(milliseconds));
                index += 1;
            }
            "--multipv" => {
                options.variations =
                    parse_positive::<usize>(arguments.get(index), "MultiPV count")?;
                index += 1;
            }
            "--param" => {
                let entry = arguments
                    .get(index)
                    .ok_or_else(|| "missing --param assignment".to_owned())?;
                options
                    .params
                    .apply_assignments(entry.split(','))
                    .map_err(|error| format!("--param: {error}"))?;
                index += 1;
            }
            "--params-file" => {
                let path = arguments
                    .get(index)
                    .ok_or_else(|| "missing --params-file path".to_owned())?;
                let body = std::fs::read_to_string(path)
                    .map_err(|error| format!("reading {path}: {error}"))?;
                options
                    .params
                    .apply_assignments(body.lines())
                    .map_err(|error| format!("{path}: {error}"))?;
                index += 1;
            }
            "--show-params" => {
                for (name, value) in options.params.entries() {
                    println!("{name}={value}");
                }
            }
            "--hash-mb" => {
                options.hash_megabytes =
                    parse_positive::<usize>(arguments.get(index), "hash size")?;
                if options.hash_megabytes > 512 {
                    return Err("hash size must not exceed 512 MiB".to_owned());
                }
                index += 1;
            }
            unknown => return Err(format!("unknown search option {unknown:?}")),
        }
    }

    Ok(options)
}

fn parse_mode(value: &str) -> Result<Mode, String> {
    Mode::parse(value).ok_or_else(|| format!("unknown mode {value:?}"))
}

fn parse_depth(value: Option<&String>, default: u8) -> Result<u8, String> {
    let Some(value) = value else {
        return if default == 0 {
            Err("missing depth".to_owned())
        } else {
            Ok(default)
        };
    };
    let depth = value
        .parse::<u8>()
        .map_err(|error| format!("invalid depth {value:?}: {error}"))?;
    if depth == 0 || depth > MAX_SEARCH_DEPTH {
        return Err(format!("depth must be between 1 and {MAX_SEARCH_DEPTH}"));
    }
    Ok(depth)
}

fn parse_positive<T>(value: Option<&String>, label: &str) -> Result<T, String>
where
    T: std::str::FromStr + PartialEq + Default,
    T::Err: std::fmt::Display,
{
    let value = value.ok_or_else(|| format!("missing {label}"))?;
    let parsed = value
        .parse::<T>()
        .map_err(|error| format!("invalid {label} {value:?}: {error}"))?;
    if parsed == T::default() {
        return Err(format!("{label} must be at least 1"));
    }
    Ok(parsed)
}

fn run_search(options: SearchOptions) {
    let position = Position::starting(options.mode);
    let mut searcher = Searcher::new(options.hash_megabytes);
    searcher.set_eval_params(options.params);

    println!("engine rpsfish {}", env!("CARGO_PKG_VERSION"));
    println!("rules {}", rpsfish::RULES_VERSION);
    println!("mode {} ({})", options.mode.name(), options.mode.id());
    let rules = options.mode.rules();
    println!(
        "rule facts annihilation-loses={} other-loss={} territory={} boundary-goal={} immortality-floor={}",
        rules.annihilation_loses,
        rules.has_other_loss_condition,
        rules.uses_territory,
        rules.uses_boundary_goal,
        rules.immortality_prevents_loss(),
    );
    if options.params != EvalParams::DEFAULT {
        println!("weights modified from defaults");
    }
    println!("position {:016x}", position.hash());
    let result =
        searcher.analyze_with_updates(position, options.limits, options.variations, print_update);

    println!("stop {:?}", result.stop_reason);
    if let Some(best) = result.lines.first() {
        println!("bestmove {}", best.movement);
    } else {
        println!("bestmove (none)");
    }
}

fn print_update(update: &AnalysisUpdate) {
    let elapsed_ms = update.stats.elapsed.as_millis();
    let nodes_per_second = if elapsed_ms == 0 {
        0
    } else {
        u128::from(update.stats.nodes).saturating_mul(1_000) / elapsed_ms
    };
    for (index, line) in update.lines.iter().enumerate() {
        println!(
            "info depth {} seldepth {} multipv {} score {} confidence {} nodes {} nps {} time {} pv {}",
            update.completed_depth,
            update.stats.selective_depth,
            index + 1,
            line.score,
            update.confidence,
            update.stats.nodes,
            nodes_per_second,
            elapsed_ms,
            format_principal_variation(line),
        );
    }
}

fn format_principal_variation(line: &AnalysisLine) -> String {
    line.principal_variation
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" | ")
}

fn run_perft(mode: Mode, depth: u8) {
    let mut position = Position::starting(mode);
    let started = std::time::Instant::now();
    let nodes = perft(&mut position, depth);
    let elapsed = started.elapsed();
    let nodes_per_second = if elapsed.is_zero() {
        0.0
    } else {
        nodes as f64 / elapsed.as_secs_f64()
    };
    println!("mode: {} ({})", mode.name(), mode.id());
    println!("depth: {depth}");
    println!("nodes: {nodes}");
    println!("elapsed: {elapsed:.3?}");
    println!("nodes/s: {nodes_per_second:.0}");
}

fn print_usage() {
    eprintln!(
        "usage:\n  rpsfish search [mode] [depth] [--deep] [--depth N] [--nodes N] [--time-ms N] [--multipv N] [--hash-mb N] [--param k=v,..] [--params-file F] [--show-params]\n  rpsfish perft <annihilation|total-war|infiltration> [depth]\n\n--deep searches toward depth 127 with 50M-node and 60-second safety caps; explicit limits override the preset.\nEvaluation weights are data: --param and --params-file load a tuned set produced by the arena binary."
    );
}

#[cfg(test)]
mod tests {
    use super::{MAX_SEARCH_DEPTH, parse_search_options, run};

    #[test]
    fn help_and_bad_arguments_are_handled() {
        assert!(run(vec!["help".to_owned()]).is_ok());
        assert!(run(vec!["unknown".to_owned()]).is_err());
        assert!(
            run(vec![
                "search".to_owned(),
                "annihilation".to_owned(),
                "0".to_owned(),
            ])
            .is_err()
        );
    }

    #[test]
    fn deep_preset_remains_bounded_and_can_be_overridden() {
        let arguments = [
            "infiltration".to_owned(),
            "--deep".to_owned(),
            "--depth".to_owned(),
            "24".to_owned(),
            "--nodes".to_owned(),
            "12345".to_owned(),
            "--time-ms".to_owned(),
            "678".to_owned(),
            "--multipv".to_owned(),
            "2".to_owned(),
        ];
        let options = parse_search_options(&arguments).expect("options must parse");
        assert_eq!(options.limits.max_depth, 24);
        assert_eq!(options.limits.max_nodes, Some(12_345));
        assert_eq!(options.limits.move_time.unwrap().as_millis(), 678);
        assert_eq!(options.variations, 2);

        let deep = parse_search_options(&["--deep".to_owned()]).expect("preset must parse");
        assert_eq!(deep.limits.max_depth, MAX_SEARCH_DEPTH);
        assert!(deep.limits.max_nodes.is_some());
        assert!(deep.limits.move_time.is_some());
    }

    #[test]
    fn weights_can_be_overridden_on_the_command_line() {
        let options = parse_search_options(&[
            "annihilation".to_owned(),
            "--param".to_owned(),
            "annihilation_material=333,annihilation_mobility=7".to_owned(),
        ])
        .expect("options must parse");
        assert_eq!(options.params.annihilation_material, 333);
        assert_eq!(options.params.annihilation_mobility, 7);

        assert!(
            parse_search_options(&["--param".to_owned(), "nope=1".to_owned()]).is_err(),
            "unknown weights must be rejected rather than silently ignored"
        );
    }
}
