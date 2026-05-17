//! Baseline performance probes. NOT a regression test — these don't assert
//! anything; they just print numbers when the user file is present so we can
//! see the per-line tokenize cost on the actual files the user is editing.
//!
//! Run with: `cargo test --release --test baseline_perf -- --ignored --nocapture`

use anvil_core::editor::buffer;
use anvil_core::editor::syntax;
use anvil_core::editor::tokenizer;

fn time_tokenize(path: &str, label: &str) {
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("{label}: file {path} not present; skipping");
        return;
    };
    let datadir = "/home/daniel/dev/lite-anvil/data";
    let index = syntax::load_syntax_index(datadir);
    let filename = path.rsplit('/').next().unwrap_or(path);
    let Some(entry) = syntax::match_syntax_entry(filename, &index) else {
        eprintln!("{label}: no syntax for {filename}");
        return;
    };
    let Some(def) = entry.load_full() else {
        eprintln!("{label}: failed to load syntax def");
        return;
    };
    let Ok(compiled) = tokenizer::compile_from_definition(&def) else {
        eprintln!("{label}: failed to compile syntax");
        return;
    };

    let lines = buffer::split_lines(&text);
    let n = lines.len();
    let viewport = 60.min(n);

    // Warm: tokenize once.
    let mut acc: Vec<u8> = Vec::new();
    for line in lines.iter().take(viewport) {
        let (_, end) = tokenizer::tokenize_line_with_state(&compiled, line, &acc);
        acc = end;
    }

    // Measure: tokenize the same viewport repeatedly. This mirrors the
    // worst case the user hits — every edit at line 1 forces the cache to
    // re-tokenize the visible region from scratch.
    let iters = 50;
    let start = std::time::Instant::now();
    for _ in 0..iters {
        let mut state: Vec<u8> = Vec::new();
        for line in lines.iter().take(viewport) {
            let (_, end) = tokenizer::tokenize_line_with_state(&compiled, line, &state);
            state = end;
        }
        std::hint::black_box(&state);
    }
    let el = start.elapsed();
    let total_lines = iters * viewport;
    eprintln!(
        "{label}: {} lines, viewport={}, x{} iters = {:?} ({:?}/line)",
        n,
        viewport,
        iters,
        el,
        el / total_lines as u32,
    );
}

#[test]
#[ignore]
fn baseline_web_ready() {
    time_tokenize("/home/daniel/dev/contexts/gos/web_ready.md", "web_ready.md");
}

#[test]
#[ignore]
fn baseline_changelog() {
    time_tokenize("/home/daniel/dev/lite-anvil/changelog.md", "changelog.md");
}
