use skb::state::UsageState;
use skb::FileIndex;
use skb_v2::{GenerationEngine, SearchQuery};
use std::env;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn main() {
    let mut args = env::args().skip(1);
    let entries = parse_usize(args.next(), 100_000);
    let prefix_rounds = parse_usize(args.next(), 20).max(1);
    let fuzzy_rounds = parse_usize(args.next(), 3).max(1);

    println!("SKB v2 retrieval microbenchmark");
    println!("entries={entries} prefix_rounds={prefix_rounds} fuzzy_rounds={fuzzy_rounds}");

    let build_started = Instant::now();
    let index = FileIndex::synthetic(entries);
    let state_path = PathBuf::from("synthetic://skb-v2-search-bench-state.json");
    let engine = GenerationEngine::new(index, UsageState::default(), state_path);
    println!("index_build_ms={:.3}", millis(build_started.elapsed()));

    let prefix_query = SearchQuery::prefix(prefix_term(entries));
    let (prefix_elapsed, prefix_hits) = run_rounds(&engine, &prefix_query, prefix_rounds);
    println!(
        "prefix_avg_us={:.3} prefix_hits={} prefix_query={:?}",
        micros(prefix_elapsed) / prefix_rounds as f64,
        prefix_hits,
        prefix_query.term
    );

    let fuzzy_query = SearchQuery::fuzzy(fuzzy_term(entries));
    let (fuzzy_elapsed, fuzzy_hits) = run_rounds(&engine, &fuzzy_query, fuzzy_rounds);
    println!(
        "fuzzy_avg_us={:.3} fuzzy_hits={} fuzzy_query={:?}",
        micros(fuzzy_elapsed) / fuzzy_rounds as f64,
        fuzzy_hits,
        fuzzy_query.term
    );
}

fn run_rounds(engine: &GenerationEngine, query: &SearchQuery, rounds: usize) -> (Duration, usize) {
    let started = Instant::now();
    let mut hits = 0usize;
    for _ in 0..rounds {
        hits = engine.search(query).hits.len();
        std::hint::black_box(hits);
    }
    (started.elapsed(), hits)
}

fn prefix_term(entries: usize) -> String {
    let target = entries.saturating_sub(1).min(99_999_999);
    let text = format!("file_{target:08}");
    text.chars().take(10).collect()
}

fn fuzzy_term(entries: usize) -> String {
    let target = entries.saturating_div(2).min(99_999_999);
    format!("file {target:08}")
}

fn parse_usize(value: Option<String>, fallback: usize) -> usize {
    value
        .and_then(|text| text.parse::<usize>().ok())
        .unwrap_or(fallback)
}

fn micros(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000_000.0
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}
