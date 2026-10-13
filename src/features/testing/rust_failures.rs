//! libtest failure attribution; reported metadata never changes process outcomes.
use super::{FailedTest, parse_rust_stack_trace};
use regex::Regex;
use std::collections::BTreeMap;

pub(super) fn parse(stdout: &str, stderr: &str) -> Vec<FailedTest> {
    let combined = format!("{stdout}\n{stderr}");
    let lines: Vec<_> = combined.lines().collect();
    let (Ok(running), Ok(failed), Ok(header), Ok(panic)) = (
        Regex::new(r"^running \d+ tests?$"),
        Regex::new(r"^test (.+) \.\.\. FAILED$"),
        Regex::new(r"^---- (.+) (?:stdout|stderr) ----$"),
        Regex::new(r"^thread '.+'(?: \(\d+\))? panicked at (.+):(\d+):(\d+):(?:.*)$"),
    ) else {
        return Vec::new();
    };
    let mut boundaries: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| running.is_match(line).then_some(i))
        .collect();
    if boundaries.first() != Some(&0) {
        boundaries.insert(0, 0);
    }
    boundaries.push(lines.len());
    let mut output = Vec::new();
    for range in boundaries.windows(2) {
        let run = &lines[range[0]..range[1]];
        let summary_end = run
            .iter()
            .position(|line| *line == "failures:" || header.is_match(line))
            .unwrap_or(run.len());
        let names: Vec<_> = run[..summary_end]
            .iter()
            .filter_map(|line| failed.captures(line).map(|c| c[1].to_string()))
            .collect();
        let mut counts = BTreeMap::new();
        for name in &names {
            *counts.entry(name.clone()).or_insert(0usize) += 1;
        }
        let mut blocks: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for (index, line) in run.iter().enumerate() {
            let Some(capture) = header.captures(line) else {
                continue;
            };
            let Some(name) = capture.get(1).map(|capture| capture.as_str()) else {
                continue;
            };
            let end = run[index + 1..]
                .iter()
                .position(|line| {
                    header.is_match(line)
                        || *line == "failures:"
                        || line.starts_with("test result:")
                })
                .map_or(run.len(), |offset| index + 1 + offset);
            blocks
                .entry(name)
                .or_default()
                .push(run[index + 1..end].join("\n"));
        }
        for name in names {
            let mut failure = FailedTest {
                name: name.clone(),
                file_path: None,
                line_number: None,
                message: String::new(),
                expected: None,
                actual: None,
                stack_trace: None,
                related_files: vec![],
            };
            // Duplicate identities/sections are ambiguous. Preserve the named
            // failure and raw diagnostics, rather than assigning another test's location.
            if counts.get(name.as_str()) == Some(&1)
                && let Some(sections) = blocks.get(name.as_str())
                && sections.len() == 1
            {
                let body = &sections[0];
                let locations: Vec<_> = body
                    .lines()
                    .filter_map(|line| panic.captures(line))
                    .collect();
                if locations.len() == 1 {
                    failure.file_path = Some(locations[0][1].to_string());
                    failure.line_number = locations[0][2].parse().ok();
                }
                let message = body
                    .lines()
                    .take_while(|line| *line != "stack backtrace:")
                    .collect::<Vec<_>>()
                    .join("\n");
                failure.message =
                    crate::utils::text_budget::head_truncate(message.trim(), 2000).text;
                // assert_eq! operand order is caller-defined. Keep libtest's
                // left/right labels, never invent expected/actual semantics.
                let frames = parse_rust_stack_trace(body);
                if !frames.is_empty() {
                    failure.related_files = frames
                        .iter()
                        .filter_map(|frame| frame.file_path.clone())
                        .collect();
                    failure.related_files.sort();
                    failure.related_files.dedup();
                    failure.stack_trace = Some(frames);
                }
            }
            output.push(failure);
        }
    }
    output
}
