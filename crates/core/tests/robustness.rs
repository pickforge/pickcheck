//! Seeded robustness test: mutated golden fixtures must parse without a
//! panic. The mutations come from a fixed seed, so every run is the same.

use std::{
    collections::BTreeMap,
    fs,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

use pickcheck_core::{Language, parse_source};

const SEED: u64 = 0x5EED_0052;
const MUTANTS_PER_FIXTURE: usize = 200;
const MAX_SPAN: usize = 64;
/// Far above the few seconds a fixture takes in a debug build, so only a
/// hang reaches it.
const FIXTURE_TIMEOUT: Duration = Duration::from_secs(300);

type Parse = fn(Language, &str) -> anyhow::Result<()>;

/// SplitMix64: small, fast, and good enough to spread mutations.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..bound`; `bound` must be positive.
    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % bound as u64).unwrap()
    }
}

#[derive(Debug)]
enum Mutation {
    Truncate {
        at: usize,
    },
    Delete {
        start: usize,
        len: usize,
    },
    Duplicate {
        start: usize,
        len: usize,
    },
    Swap {
        first: usize,
        second: usize,
        len: usize,
    },
    Overwrite {
        start: usize,
        text: String,
    },
}

struct Fixture {
    name: String,
    language: Language,
    source: Vec<char>,
    punctuation: Vec<char>,
}

/// Each case has its own seed, so the fixtures can run on separate threads
/// and still give the same mutants on every run.
#[test]
fn mutated_fixtures_parse_without_panicking() {
    let fixtures = fixture_sources()
        .into_iter()
        .map(|path| {
            let source: Vec<char> = fs::read_to_string(&path).unwrap().chars().collect();
            Fixture {
                name: path.file_name().unwrap().to_string_lossy().into_owned(),
                language: Language::from_path(&path).unwrap(),
                punctuation: punctuation(&source),
                source,
            }
        })
        .collect();
    let failures = run_fixtures(fixtures, FIXTURE_TIMEOUT, |language, text| {
        parse_source(language, text).map(drop)
    })
    .unwrap_or_else(|timeout| panic!("{timeout}"));
    assert!(
        failures.is_empty(),
        "seed {SEED:#x} panicked on:\n{}",
        failures.join("\n")
    );
}

/// A hung parse cannot be stopped, so each fixture runs on its own detached
/// thread. Returns the panics, or an error naming every fixture still running
/// when `timeout` ends.
fn run_fixtures(
    fixtures: Vec<Fixture>,
    timeout: Duration,
    parse: Parse,
) -> Result<Vec<String>, String> {
    let (sender, receiver) = mpsc::channel();
    let progress: Vec<_> = fixtures
        .into_iter()
        .map(|fixture| {
            let current = Arc::new(AtomicUsize::new(0));
            let name = fixture.name.clone();
            let (sender, shared) = (sender.clone(), Arc::clone(&current));
            thread::spawn(move || {
                let failures: Vec<_> = (0..MUTANTS_PER_FIXTURE)
                    .filter_map(|case| {
                        shared.store(case, Ordering::Relaxed);
                        run_case(&fixture, case, parse)
                    })
                    .collect();
                // The receiver is gone only after a timeout already failed.
                let _ = sender.send((fixture.name, failures));
            });
            (name, current)
        })
        .collect();
    drop(sender);
    let deadline = Instant::now() + timeout;
    let mut done = BTreeMap::new();
    while done.len() < progress.len() {
        let left = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(left) {
            Ok((name, failures)) => {
                done.insert(name, failures);
            }
            Err(RecvTimeoutError::Timeout) => {
                let hung: Vec<_> = progress
                    .iter()
                    .filter(|(name, _)| !done.contains_key(name))
                    .map(|(name, current)| {
                        let case = current.load(Ordering::Relaxed);
                        format!(
                            "{name} case {case} (case seed {:#x})",
                            case_seed(name, case)
                        )
                    })
                    .collect();
                return Err(format!(
                    "seed {SEED:#x} timed out after {}s on:\n{}",
                    timeout.as_secs(),
                    hung.join("\n")
                ));
            }
            Err(RecvTimeoutError::Disconnected) => panic!("a fixture thread died"),
        }
    }
    let mut failures: Vec<_> = done.into_values().flatten().collect();
    failures.sort();
    Ok(failures)
}

fn case_seed(name: &str, case: usize) -> u64 {
    SEED ^ fnv(name) ^ case as u64
}

/// Applies one to three mutations and returns a description on a panic.
fn run_case(fixture: &Fixture, case: usize, parse: Parse) -> Option<String> {
    let mut rng = Rng(case_seed(&fixture.name, case));
    rng.next();
    let mut mutant = fixture.source.clone();
    let mutations: Vec<_> = (0..=rng.below(3))
        .map(|_| {
            let mutation = pick(&mut rng, mutant.len(), &fixture.punctuation);
            apply(&mut mutant, &mutation);
            mutation
        })
        .collect();
    let text: String = mutant.into_iter().collect();
    catch_unwind(AssertUnwindSafe(|| parse(fixture.language, &text)))
        .is_err()
        .then(|| format!("{} case {case}: {mutations:?}", fixture.name))
}

/// FNV-1a, so a fixture's seeds do not depend on the fixture order.
fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xCBF2_9CE4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01B3)
    })
}

fn fixture_sources() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut sources: Vec<_> = fs::read_dir(root)
        .unwrap()
        .flat_map(|entry| fs::read_dir(entry.unwrap().path()).unwrap())
        .map(|entry| entry.unwrap().path())
        .filter(|path| Language::from_path(path).is_some())
        .collect();
    sources.sort();
    assert_eq!(sources.len(), 8, "one fixture per language, including TSX");
    sources
}

/// The distinct ASCII punctuation in the source, in a stable order.
fn punctuation(source: &[char]) -> Vec<char> {
    let mut chars: Vec<_> = source
        .iter()
        .copied()
        .filter(char::is_ascii_punctuation)
        .collect();
    chars.sort_unstable();
    chars.dedup();
    chars
}

fn pick(rng: &mut Rng, len: usize, punctuation: &[char]) -> Mutation {
    if len < 2 {
        return Mutation::Truncate { at: 0 };
    }
    let span = 1 + rng.below(MAX_SPAN.min(len / 2));
    let start = rng.below(len - span);
    match rng.below(5) {
        0 => Mutation::Truncate { at: rng.below(len) },
        1 => Mutation::Delete { start, len: span },
        2 => Mutation::Duplicate { start, len: span },
        3 => Mutation::Swap {
            first: start,
            second: rng.below(len - span),
            len: span,
        },
        _ => Mutation::Overwrite {
            start,
            text: (0..span.min(8))
                .map(|_| punctuation[rng.below(punctuation.len())])
                .collect(),
        },
    }
}

fn apply(source: &mut Vec<char>, mutation: &Mutation) {
    match *mutation {
        Mutation::Truncate { at } => source.truncate(at),
        Mutation::Delete { start, len } => {
            source.drain(start..start + len);
        }
        Mutation::Duplicate { start, len } => {
            let copy = source[start..start + len].to_vec();
            source.splice(start..start, copy);
        }
        Mutation::Swap { first, second, len } => {
            let (low, high) = (first.min(second), first.max(second));
            if high >= low + len {
                for offset in 0..len {
                    source.swap(low + offset, high + offset);
                }
            }
        }
        Mutation::Overwrite { start, ref text } => {
            for (offset, character) in text.chars().enumerate() {
                source[start + offset] = character;
            }
        }
    }
}

#[test]
fn a_hung_fixture_fails_with_its_name_and_seed() {
    let fixture = Fixture {
        name: "hang.js".to_owned(),
        language: Language::JavaScript,
        source: "let a = 1;".chars().collect(),
        punctuation: vec![';'],
    };
    let error = run_fixtures(vec![fixture], Duration::from_millis(200), |_, _| {
        loop {
            thread::park();
        }
    })
    .unwrap_err();
    assert!(error.contains(&format!("seed {SEED:#x}")), "{error}");
    assert!(error.contains("hang.js case 0 (case seed "), "{error}");
}
