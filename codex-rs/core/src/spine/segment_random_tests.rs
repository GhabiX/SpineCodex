use super::*;
use std::collections::BTreeMap;

fn next_u64(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    *seed
}

fn trial_count(default: u64) -> u64 {
    std::env::var("SPINE_RANDOM_TRIALS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[test]
fn segment_random_trace_generator_covers_required_ops() {
    for trial in 0..trial_count(96) {
        let seed = 652 + trial;
        let mut trace = DeterministicTrace::new(seed);
        trace.run_valid_trace();
    }
}

#[test]
fn segment_random_expected_failures_are_classified() {
    for trial in 0..trial_count(32).min(512) {
        let seed = 7000 + trial;
        let mut trace = DeterministicTrace::new(seed);
        trace.run_expected_failure_trace();
    }
}

#[test]
#[should_panic(expected = "resource budget policy: auto compact must reduce Cost(Pi)")]
fn auto_compact_rejects_non_reducing_mem() {
    let mut trace = DeterministicTrace::new(1616);
    let span = RawSpan::new(0, 4).expect("span");
    let raw_cost = trace.raw_cost(span);

    trace.install_auto_compact_with_cost("non-reducing-auto", span, raw_cost);
}

struct DeterministicTrace {
    seed: u64,
    rng: u64,
    raw_len: u64,
    segments: Vec<Segment>,
    artifacts: SegmentArtifacts,
    pending: SegmentArtifacts,
    raw_tokens: Vec<u64>,
    mem_tokens: BTreeMap<String, u64>,
    ops: Vec<String>,
}

impl DeterministicTrace {
    fn new(seed: u64) -> Self {
        let mut rng = seed;
        let raw_len = 96 + (next_u64(&mut rng) % 64);
        let raw_tokens = (0..raw_len)
            .map(|_| 10 + (next_u64(&mut rng) % 40))
            .collect::<Vec<_>>();
        Self {
            seed,
            rng,
            raw_len,
            segments: vec![Segment::raw(0, raw_len).expect("valid initial raw")],
            artifacts: SegmentArtifacts::new(),
            pending: SegmentArtifacts::new(),
            raw_tokens,
            mem_tokens: BTreeMap::new(),
            ops: vec![format!("init raw_len={raw_len}")],
        }
    }

    fn run_valid_trace(&mut self) {
        let prefix = 4 + (next_u64(&mut self.rng) % 8);
        let child_width = 8 + (next_u64(&mut self.rng) % 16);
        let gap_width = 4 + (next_u64(&mut self.rng) % 12);
        let pending_width = 8 + (next_u64(&mut self.rng) % 16);
        let child = RawSpan::new(prefix, prefix + child_width).expect("child span");
        let pending = RawSpan::new(
            child.end + gap_width,
            (child.end + gap_width + pending_width).min(self.raw_len - 8),
        )
        .expect("pending span");
        self.partition_raw_cover(&[
            0,
            child.start,
            child.end,
            pending.start,
            pending.end,
            self.raw_len,
        ]);
        let child_id = format!("seed-{}-child", self.seed);
        let pending_id = format!("seed-{}-pending", self.seed);
        let root_id = format!("seed-{}-root", self.seed);

        self.install_compact(&child_id, child, CompactKind::Auto);
        self.validate_cover_and_live(&[child.start, child.end, pending.start, self.raw_len]);

        self.stage_compact(&pending_id, pending);
        self.validate_pending_is_invisible();
        self.commit_pending(&pending_id);
        self.validate_cover_and_live(&[child.start, child.end, pending.start, pending.end]);
        self.validate_budget("after suffix compact");

        let root = RawSpan::new(child.start, pending.end).expect("root span");
        self.install_named_mem(&root_id, root, self.compact_memory_cost(root));
        self.replace(root, Segment::mem(root_id.as_str()));
        self.validate_cover_and_live(&[root.start, root.end, self.raw_len]);
        self.validate_budget("after root archive");

        let projected = self.ok(
            canonical_cover(pending.start, [child_id.as_str()], &self.artifacts),
            "rollback/fork canonical cover",
        );
        self.ops.push(format!(
            "restrict raw_len={} survivors=[{}]",
            pending.start, child_id
        ));
        self.segments = projected;
        self.raw_len = pending.start;
        self.raw_tokens.truncate(pending.start as usize);
        self.mem_tokens
            .retain(|compact_id, _| compact_id == &child_id);
        self.validate_cover_and_live(&[child.start, child.end, self.raw_len]);
        self.validate_budget("after rollback/fork restriction");
    }

    fn run_expected_failure_trace(&mut self) {
        let start = 4 + (next_u64(&mut self.rng) % 8);
        let span = RawSpan::new(start, start + 12).expect("failure span");
        self.partition_raw_cover(&[0, span.start, span.end, span.end + 2, self.raw_len]);
        let compact_id = format!("seed-{}-bad", self.seed);
        self.install_compact(&compact_id, span, CompactKind::Auto);

        let inside = span.start + 1 + (next_u64(&mut self.rng) % (span.end - span.start - 1));
        self.ops
            .push(format!("expected_fail live_start_inside_mem raw={inside}"));
        let err = validate_future_live_boundaries(&self.segments, &self.artifacts, &[inside])
            .expect_err("live start inside Mem must fail");
        assert!(
            matches!(err, SegmentError::LiveStartInsideMem { .. }),
            "seed {}\nops:\n{}\nunexpected error {err:?}",
            self.seed,
            self.ops.join("\n")
        );

        self.ops
            .push("expected_fail exposed_mem_without_artifact".to_string());
        let exposed = replace_exact_cover(
            &self.segments,
            &self.artifacts,
            RawSpan::new(span.end, span.end + 2).expect("exposed span"),
            Segment::mem("missing-artifact"),
        );
        let err = match exposed {
            Ok(next_segments) => validate_cover(&next_segments, &self.artifacts)
                .expect_err("missing artifact must fail"),
            Err(err) => err,
        };
        assert!(
            matches!(err, SegmentError::MissingMemArtifact { .. }),
            "seed {}\nops:\n{}\nunexpected error {err:?}",
            self.seed,
            self.ops.join("\n")
        );
    }

    fn install_compact(&mut self, compact_id: &str, span: RawSpan, kind: CompactKind) {
        let raw_cost = self.raw_cost(span);
        let memory_cost = match kind {
            CompactKind::Auto => (raw_cost / 2).max(1),
            CompactKind::Boundary => raw_cost + 3,
        };
        if matches!(kind, CompactKind::Auto) {
            return self.install_auto_compact_with_cost(compact_id, span, memory_cost);
        }
        self.install_named_mem(compact_id, span, memory_cost);
        self.replace(span, Segment::mem(compact_id));
    }

    fn install_auto_compact_with_cost(
        &mut self,
        compact_id: &str,
        span: RawSpan,
        memory_cost: u64,
    ) {
        let raw_cost = self.raw_cost(span);
        assert!(
            memory_cost < raw_cost,
            "resource budget policy: auto compact must reduce Cost(Pi): seed {}\nops:\n{}\nraw_cost={raw_cost} memory_cost={memory_cost}",
            self.seed,
            self.ops.join("\n")
        );
        self.install_named_mem(compact_id, span, memory_cost);
        self.replace(span, Segment::mem(compact_id));
    }

    fn install_named_mem(&mut self, compact_id: &str, span: RawSpan, memory_cost: u64) {
        self.ops.push(format!(
            "install compact_id={compact_id} span={span} memory_cost={memory_cost}"
        ));
        self.artifacts.insert(compact_id.to_string(), span);
        self.mem_tokens.insert(compact_id.to_string(), memory_cost);
    }

    fn stage_compact(&mut self, compact_id: &str, span: RawSpan) {
        self.ops
            .push(format!("stage compact_id={compact_id} span={span}"));
        self.pending.insert(compact_id.to_string(), span);
    }

    fn commit_pending(&mut self, compact_id: &str) {
        let span = self
            .pending
            .remove(compact_id)
            .unwrap_or_else(|| panic!("seed {}\nmissing pending {compact_id}", self.seed));
        self.install_compact(compact_id, span, CompactKind::Boundary);
    }

    fn validate_pending_is_invisible(&mut self) {
        self.ops.push("validate pending invisible".to_string());
        self.ok(
            validate_cover(&self.segments, &self.artifacts),
            "pending cover",
        );
        for compact_id in self.pending.keys() {
            assert!(
                !self.segments.iter().any(
                    |segment| matches!(segment, Segment::Mem { compact_id: id } if id == compact_id)
                ),
                "seed {}\nops:\n{}\npending compact {compact_id} is visible",
                self.seed,
                self.ops.join("\n")
            );
        }
    }

    fn replace(&mut self, span: RawSpan, replacement: Segment) {
        self.ops
            .push(format!("replace span={span} replacement={replacement:?}"));
        self.segments = self.ok(
            replace_exact_cover(&self.segments, &self.artifacts, span, replacement),
            "replace exact cover",
        );
    }

    fn validate_cover_and_live(&mut self, live_starts: &[u64]) {
        self.ops
            .push(format!("validate live_starts={live_starts:?}"));
        self.ok(
            validate_cover(&self.segments, &self.artifacts),
            "validate cover",
        );
        self.ok(
            validate_future_live_boundaries(&self.segments, &self.artifacts, live_starts),
            "validate live starts",
        );
    }

    fn validate_budget(&mut self, label: &str) {
        let total = self.cover_cost();
        let budget = self.raw_tokens.iter().sum::<u64>() + 128;
        self.ops
            .push(format!("budget {label}: total={total} budget={budget}"));
        assert!(
            total <= budget,
            "seed {}\nops:\n{}\nCost(Pi) {total} exceeds budget {budget}",
            self.seed,
            self.ops.join("\n")
        );
    }

    fn partition_raw_cover(&mut self, boundaries: &[u64]) {
        self.ops
            .push(format!("partition raw cover boundaries={boundaries:?}"));
        self.segments = boundaries
            .windows(2)
            .filter_map(|window| {
                let start = window[0];
                let end = window[1];
                (start < end).then(|| Segment::raw(start, end).expect("valid raw partition"))
            })
            .collect();
    }

    fn compact_memory_cost(&self, span: RawSpan) -> u64 {
        (self.raw_cost(span) / 2).max(1)
    }

    fn raw_cost(&self, span: RawSpan) -> u64 {
        self.raw_tokens[span.start as usize..span.end as usize]
            .iter()
            .sum()
    }

    fn cover_cost(&self) -> u64 {
        self.segments
            .iter()
            .map(|segment| match segment {
                Segment::Raw(span) => self.raw_cost(*span),
                Segment::Mem { compact_id } => {
                    *self.mem_tokens.get(compact_id).unwrap_or_else(|| {
                        panic!(
                            "seed {}\nops:\n{}\nmissing cost for {compact_id}",
                            self.seed,
                            self.ops.join("\n")
                        )
                    })
                }
                Segment::Note { .. } => 8,
            })
            .sum()
    }

    fn ok<T, E: std::fmt::Debug>(&self, result: Result<T, E>, label: &str) -> T {
        result.unwrap_or_else(|err| {
            panic!(
                "seed {}\noperation={label}\nops:\n{}\nerror: {err:?}",
                self.seed,
                self.ops.join("\n")
            )
        })
    }
}

#[derive(Clone, Copy)]
enum CompactKind {
    Auto,
    Boundary,
}
